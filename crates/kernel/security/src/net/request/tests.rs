use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use super::*;
use crate::exposure::holds;
use crate::net::capture::{CaptureRules, SEND_HOSTS_MAX};
use crate::secrets::{Handles, KeptHosts, OwnerEntry, PLUGIN_HANDLES_MAX, Scopes, Secrets};
use crate::store::{SecretRef, Store, reference};

const NAMESPACE: &str = "dev.crazyagent.e0123456789ab";
const TOKEN: &[u8] = b"ghp_owner-paste-0123456789";
const CAPTURED: &str = "gho_captured-access-token-0123456789";

/// One change a row makes to a request.
type Change = Box<dyn Fn(&mut Request)>;

/// The order of calls the rows record across the store and egress.
type Events = Arc<Mutex<Vec<String>>>;

/// The in-memory store double: items by reference, every read and write recorded.
struct FakeStore {
    items: Mutex<BTreeMap<SecretRef, Vec<u8>>>,
    events: Events,
}

impl Store for FakeStore {
    fn put(
        &self,
        service: &str,
        account: &str,
        secret: &SecretString,
    ) -> Result<SecretRef, StoreError> {
        self.events.lock().unwrap().push(format!("put {account}"));
        let found = reference(service, account)?;
        self.items
            .lock()
            .unwrap()
            .insert(found.clone(), secret.expose().as_bytes().to_vec());
        Ok(found)
    }

    fn get(&self, found: &SecretRef) -> Result<SecretString, StoreError> {
        self.events.lock().unwrap().push("get".to_owned());
        let bytes = self.items.lock().unwrap().get(found).cloned();
        bytes.map_or_else(
            || Err(StoreError::code("credential_missing")),
            SecretString::new,
        )
    }

    fn delete(&self, found: &SecretRef) -> Result<(), StoreError> {
        self.events.lock().unwrap().push("delete".to_owned());
        self.items.lock().unwrap().remove(found);
        Ok(())
    }

    fn git_credential_helper(
        &self,
        _found: &SecretRef,
        _username: &str,
    ) -> Result<String, StoreError> {
        unreachable!("net requests never build a git helper")
    }
}

/// The egress double: refuses the hosts it is given, records each call and the config it
/// was handed, and answers one fixed reply.
struct FakeEgress {
    refused: Vec<&'static str>,
    resolved: BTreeMap<&'static str, Vec<IpAddr>>,
    events: Events,
    configs: Mutex<Vec<Vec<u8>>>,
    reply_headers: Mutex<Vec<Vec<String>>>,
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Egress for FakeEgress {
    fn allows(
        &self,
        _caller: &'static str,
        scheme: &str,
        host: &str,
        port: u16,
    ) -> Result<(), StoreError> {
        self.events
            .lock()
            .unwrap()
            .push(format!("allows {scheme} {host} {port}"));
        if self.refused.contains(&host) {
            return Err(StoreError::code("net_host_refused"));
        }
        Ok(())
    }

    fn resolve(&self, host: &str, _port: u16) -> Result<Vec<IpAddr>, StoreError> {
        Ok(self
            .resolved
            .get(host)
            .cloned()
            .unwrap_or_else(|| vec!["140.82.112.4".parse().unwrap()]))
    }

    fn send(&self, sealed: Sealed) -> Result<RawReply, StoreError> {
        self.events.lock().unwrap().push("send".to_owned());
        self.configs.lock().unwrap().push(sealed.config().to_vec());
        self.reply_headers
            .lock()
            .unwrap()
            .push(sealed.reply_headers().to_vec());
        Ok(RawReply {
            status: self.status,
            headers: self.headers.clone(),
            body: self.body.clone(),
        })
    }
}

/// The parts one row builds a request path from.
struct Rig {
    store: FakeStore,
    scopes: Scopes,
    handles: Mutex<Handles>,
    egress: FakeEgress,
    rules: CaptureRules,
}

impl Rig {
    /// A rig whose egress answers one body with status 200. Input: the body. Output: the rig.
    fn new(body: &[u8]) -> Self {
        let events = Events::default();
        Self {
            store: FakeStore {
                items: Mutex::new(BTreeMap::new()),
                events: events.clone(),
            },
            scopes: Scopes::new(&[("github", "github"), ("notes", "notes")]).unwrap(),
            handles: Mutex::new(Handles::new()),
            egress: FakeEgress {
                refused: vec!["refused.example"],
                resolved: BTreeMap::from([
                    ("localhost", vec!["127.0.0.1".parse().unwrap()]),
                    ("private.example", vec!["10.0.0.5".parse().unwrap()]),
                    (
                        "mixed.example",
                        vec!["140.82.112.4".parse().unwrap(), "10.0.0.5".parse().unwrap()],
                    ),
                    ("unresolved.example", Vec::new()),
                    ("v6.example", vec!["2606:4700::1111".parse().unwrap()]),
                ]),
                events,
                configs: Mutex::new(Vec::new()),
                reply_headers: Mutex::new(Vec::new()),
                status: 200,
                headers: vec![
                    ("X-OAuth-Scopes".to_owned(), "repo".to_owned()),
                    ("Set-Cookie".to_owned(), "session=1".to_owned()),
                ],
                body: body.to_vec(),
            },
            rules: CaptureRules::new(&[("github", "github.com", &["access_token", "device_code"])])
                .unwrap()
                .with_sends(&[("github", "access_token", &["api.github.com"], None)])
                .unwrap(),
        }
    }

    /// The secrets module of one owner request holding the owner's token. Output: it.
    fn secrets(&self) -> Secrets<'_> {
        let paste = SecretString::new(TOKEN.to_vec()).unwrap();
        Secrets::for_request(
            &self.store,
            NAMESPACE,
            &self.scopes,
            &self.handles,
            Some(OwnerEntry {
                plugin: "github",
                hosts: [
                    "github.com",
                    "api.github.com",
                    "ghe.example:8443",
                    "127.0.0.1:8080",
                    "[::1]:8080",
                    "localhost:8080",
                    "v6.example:8443",
                    "mixed.example",
                    "private.example",
                    "unresolved.example",
                    "evil.example",
                    "140.82.112.4",
                ]
                .map(str::to_owned)
                .to_vec(),
                value: paste,
            }),
        )
    }

    /// The events recorded so far. Output: them.
    fn events(&self) -> Vec<String> {
        self.egress.events.lock().unwrap().clone()
    }

    /// The single config egress was handed. Output: it as text.
    fn config(&self) -> String {
        let configs = self.egress.configs.lock().unwrap();
        assert_eq!(configs.len(), 1);
        String::from_utf8(configs[0].clone()).unwrap()
    }
}

/// A GET to one URL with nothing else. Input: the URL. Output: the request.
fn get(url: &str) -> Request {
    Request {
        method: Method::Get,
        url: url.to_owned(),
        headers: Vec::new(),
        bearer: None,
        form: Vec::new(),
        secret_form: Vec::new(),
        json: None,
        reply_headers: Vec::new(),
        capture: Vec::new(),
    }
}

/// A capture of one field. Inputs: the field and its keep name. Output: the capture.
fn capture(field: &str, keep_as: Option<&str>) -> Capture {
    Capture {
        field: field.to_owned(),
        keep_as: keep_as.map(str::to_owned),
    }
}

/// The device-flow token reply GitHub answers.
fn token_reply() -> Vec<u8> {
    format!(r#"{{"access_token":"{CAPTURED}","token_type":"bearer","scope":"repo"}}"#).into_bytes()
}

#[test]
fn a_refused_host_never_resolves_a_secret() {
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let token = secrets.put_from_owner("github", "token").unwrap();
    rig.egress.events.lock().unwrap().clear();
    let mut request = get("https://refused.example/v1");
    request.bearer = Some(token);
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    assert_eq!(
        requests.request("github", request),
        Err(StoreError::code("net_host_refused"))
    );
    assert_eq!(rig.events(), ["allows https refused.example 443"]);
}

#[test]
fn the_secret_is_filled_only_after_the_allow() {
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let token = secrets.put_from_owner("github", "token").unwrap();
    rig.egress.events.lock().unwrap().clear();
    let mut request = get("https://api.github.com/user");
    request.bearer = Some(token);
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    requests.request("github", request).unwrap();
    assert_eq!(
        rig.events(),
        ["allows https api.github.com 443", "get", "send"]
    );
    assert!(holds(TOKEN, rig.config().as_bytes()));
}

#[test]
fn another_plugins_handle_is_refused_before_send() {
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let token = secrets.put_from_owner("github", "token").unwrap();
    rig.egress.events.lock().unwrap().clear();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut bearer = get("https://api.github.com/user");
    bearer.bearer = Some(token);
    assert_eq!(
        requests.request("notes", bearer),
        Err(StoreError::code("secret_handle_refused"))
    );
    let mut form = get("https://api.github.com/user");
    form.method = Method::Post;
    form.secret_form = vec![SecretField {
        name: "client_secret".to_owned(),
        secret: token,
    }];
    assert_eq!(
        requests.request("notes", form),
        Err(StoreError::code("secret_handle_refused"))
    );
    assert!(rig.events().iter().all(|event| event.starts_with("allows")));
}

#[test]
fn a_plugin_never_receives_a_captured_value() {
    let duplicate = format!(
        r#"{{"access_token":"{CAPTURED}","token_type":"bearer","access_token":"{CAPTURED}-second"}}"#
    );
    for body in [token_reply(), duplicate.into_bytes()] {
        let rig = Rig::new(&body);
        let secrets = rig.secrets();
        let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
        let mut request = get("https://github.com/login/oauth/access_token");
        request.method = Method::Post;
        request.reply_headers = vec!["x-oauth-scopes".to_owned()];
        let reply = requests.request("github", request).unwrap();
        assert_eq!(reply.captured.len(), 1);
        assert_eq!(reply.captured[0].field, "access_token");
        let mut seen = reply.body.clone();
        for field in &reply.headers {
            seen.extend_from_slice(field.name.as_bytes());
            seen.extend_from_slice(field.value.as_bytes());
        }
        for found in &reply.captured {
            seen.extend_from_slice(found.field.as_bytes());
            seen.extend_from_slice(&found.secret.to_le_bytes());
        }
        assert!(!holds(CAPTURED.as_bytes(), &seen));
        assert!(!holds(b"gho_", &seen));
        let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(body["token_type"], "bearer");
    }
}

#[test]
fn a_request_without_capture_still_gets_a_handle_for_a_manifest_field() {
    let rig = Rig::new(&token_reply());
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let reply = requests
        .request("github", get("https://github.com/login/oauth/access_token"))
        .unwrap();
    assert_eq!(reply.captured.len(), 1);
    let held = secrets
        .value("github", reply.captured[0].secret, "github.com")
        .unwrap();
    assert_eq!(held.expose(), CAPTURED);
}

#[test]
fn a_request_cannot_remove_a_manifest_capture() {
    let rig = Rig::new(&token_reply());
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get("https://github.com/login/oauth/access_token");
    request.capture = vec![capture("scope", None)];
    let reply = requests.request("github", request).unwrap();
    let fields: Vec<&str> = reply
        .captured
        .iter()
        .map(|found| found.field.as_str())
        .collect();
    assert_eq!(fields, ["access_token", "scope"]);
    assert!(!holds(CAPTURED.as_bytes(), &reply.body));
}

#[test]
fn capture_rules_refuse_a_duplicate_row_and_too_many_fields() {
    let refused = Err(StoreError::code("net_capture_rules_invalid"));
    let rules = |rows: &[(&'static str, &str, &[&str])]| CaptureRules::new(rows).map(|_| ());
    assert_eq!(
        rules(&[
            ("github", "github.com", &["a"]),
            ("github", "github.com", &["b"])
        ]),
        refused
    );
    assert_eq!(
        rules(&[("github", "github.com", &["a", "b", "c", "d", "e"])]),
        refused
    );
    assert_eq!(rules(&[("github", "github.com", &["a", "a"])]), refused);
    assert_eq!(rules(&[("github", "github.com", &["a b"])]), refused);
    assert_eq!(rules(&[("github", "github.com", &[""])]), refused);
    let long = "a".repeat(capture::FIELD_BYTES_MAX + 1);
    assert_eq!(
        rules(&[("github", "github.com", &[long.as_str()])]),
        refused
    );
    assert!(
        rules(&[
            ("github", "github.com", &["a", "b", "c", "d"]),
            ("notes", "github.com", &["a"])
        ])
        .is_ok()
    );
}

#[test]
fn an_absent_capture_field_is_skipped() {
    let rig = Rig::new(br#"{"error":"authorization_pending"}"#);
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let reply = requests
        .request("github", get("https://github.com/login/oauth/access_token"))
        .unwrap();
    assert!(reply.captured.is_empty());
    let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(body["error"], "authorization_pending");
}

#[test]
fn a_non_json_reply_with_a_capture_returns_no_body() {
    let form = format!("access_token={CAPTURED}&token_type=bearer");
    for body in [form.as_bytes(), b"[1]".as_slice(), b"".as_slice()] {
        let rig = Rig::new(body);
        let secrets = rig.secrets();
        let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
        assert_eq!(
            requests.request("github", get("https://github.com/login/oauth/access_token")),
            Err(StoreError::code("net_capture_not_json"))
        );
    }
    let rig = Rig::new(form.as_bytes());
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let reply = requests
        .request("github", get("https://api.github.com/user"))
        .unwrap();
    assert_eq!(reply.body, form.as_bytes());
}

#[test]
fn a_non_string_capture_field_returns_no_body() {
    let rig = Rig::new(br#"{"access_token":{"value":"gho_x"}}"#);
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    assert_eq!(
        requests.request("github", get("https://github.com/login/oauth/access_token")),
        Err(StoreError::code("net_capture_not_json"))
    );
}

#[test]
fn a_kept_capture_over_1024_bytes_is_refused() {
    let long = format!(r#"{{"access_token":"{}"}}"#, "a".repeat(1025));
    let rig = Rig::new(long.as_bytes());
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get("https://github.com/login/oauth/access_token");
    request.capture = vec![capture("access_token", Some("token"))];
    assert_eq!(
        requests.request("github", request),
        Err(StoreError::code("credential_secret_invalid"))
    );
    assert!(rig.store.items.lock().unwrap().is_empty());
    let held = format!(r#"{{"access_token":"{}"}}"#, "a".repeat(2048));
    let rig = Rig::new(held.as_bytes());
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let reply = requests
        .request("github", get("https://github.com/login/oauth/access_token"))
        .unwrap();
    assert_eq!(reply.captured.len(), 1);
}

#[test]
fn a_kept_capture_is_found_again_by_name() {
    let rig = Rig::new(&token_reply());
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get("https://github.com/login/oauth/access_token");
    request.capture = vec![capture("access_token", Some("token"))];
    let reply = requests.request("github", request).unwrap();
    let found = secrets.find("github", "token").unwrap().unwrap();
    assert_eq!(found, reply.captured[0].secret);
    assert_eq!(
        secrets
            .value("github", found, "github.com")
            .unwrap()
            .expose(),
        CAPTURED
    );
}

#[test]
fn a_kept_capture_outside_the_scope_is_refused_before_send() {
    let rig = Rig::new(&token_reply());
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get("https://github.com/login/oauth/access_token");
    request.capture = vec![capture("access_token", Some("bad name"))];
    assert_eq!(
        requests.request("github", request),
        Err(StoreError::code("credential_name_invalid"))
    );
    assert!(!rig.events().contains(&"send".to_owned()));
}

#[test]
fn http_to_a_non_loopback_host_is_refused() {
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    for url in [
        "http://api.github.com/user",
        "ftp://api.github.com/user",
        "https://user@api.github.com/user",
        "https://api.github.com/user#x",
    ] {
        assert_eq!(
            requests.request("github", get(url)),
            Err(StoreError::code("net_url_invalid")),
            "{url}"
        );
    }
    assert!(rig.events().is_empty());
    requests
        .request("github", get("http://127.0.0.1:8080/v1?q=a"))
        .unwrap();
    assert_eq!(rig.events()[0], "allows http 127.0.0.1 8080");
}

#[test]
fn a_newline_quote_or_backslash_in_any_plugin_text_adds_no_config_line() {
    let lines = |config: &str| config.lines().count();
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut base = get("https://api.github.com/user");
    base.method = Method::Post;
    base.headers = vec![Field {
        name: "Accept".to_owned(),
        value: "application/json".to_owned(),
    }];
    base.form = vec![Field {
        name: "client_id".to_owned(),
        value: "abc".to_owned(),
    }];
    requests.request("github", base.clone()).unwrap();
    let plain = lines(&rig.config());
    for bad in [
        "a\nheader = \"X: y\"",
        "a\"\nurl = \"https://evil",
        "a\\\"b",
        "a\rb",
    ] {
        let cases: [(&str, Change); 6] = [
            (
                "path",
                Box::new(move |r: &mut Request| r.url = format!("https://api.github.com/{bad}")),
            ),
            (
                "query",
                Box::new(move |r: &mut Request| {
                    r.url = format!("https://api.github.com/user?q={bad}")
                }),
            ),
            (
                "header value",
                Box::new(move |r: &mut Request| r.headers[0].value = bad.to_owned()),
            ),
            (
                "form name",
                Box::new(move |r: &mut Request| r.form[0].name = bad.to_owned()),
            ),
            (
                "form value",
                Box::new(move |r: &mut Request| r.form[0].value = bad.to_owned()),
            ),
            (
                "reply header",
                Box::new(move |r: &mut Request| r.reply_headers = vec![bad.to_owned()]),
            ),
        ];
        for (kind, change) in cases {
            let rig = Rig::new(b"{}");
            let secrets = rig.secrets();
            let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
            let mut request = base.clone();
            change(&mut request);
            match requests.request("github", request) {
                Ok(_) => assert_eq!(lines(&rig.config()), plain, "{kind} {bad:?}"),
                Err(error) => {
                    assert!(
                        [
                            "net_request_invalid",
                            "net_header_name_invalid",
                            "net_url_invalid"
                        ]
                        .contains(&error.0.as_str()),
                        "{kind} {bad:?}: {error}"
                    );
                    assert!(!rig.events().contains(&"send".to_owned()), "{kind}");
                }
            }
        }
        let rig = Rig::new(b"{}");
        let secrets = rig.secrets();
        let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
        let mut request = base.clone();
        request.headers[0].name = bad.to_owned();
        assert_eq!(
            requests.request("github", request),
            Err(StoreError::code("net_header_name_invalid")),
            "header name {bad:?}"
        );
    }
}

#[test]
fn a_header_name_outside_token_characters_is_refused() {
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    for name in [
        "", "X Y", "X:Y", "X%{url}", "X{a}", "Host", "host", "X\u{e9}",
    ] {
        let mut request = get("https://api.github.com/user");
        request.headers = vec![Field {
            name: name.to_owned(),
            value: "v".to_owned(),
        }];
        assert_eq!(
            requests.request("github", request),
            Err(StoreError::code("net_header_name_invalid")),
            "{name:?}"
        );
        let mut reply = get("https://api.github.com/user");
        reply.reply_headers = vec![name.to_owned()];
        if !name.eq_ignore_ascii_case("host") {
            assert_eq!(
                requests.request("github", reply),
                Err(StoreError::code("net_header_name_invalid")),
                "reply {name:?}"
            );
        }
    }
    let mut many = get("https://api.github.com/user");
    many.reply_headers = vec!["x-a".to_owned(); REPLY_HEADERS_MAX + 1];
    assert_eq!(
        requests.request("github", many),
        Err(StoreError::code("net_header_name_invalid"))
    );
    assert!(!rig.events().contains(&"send".to_owned()));
}

#[test]
fn the_config_url_is_built_from_the_parsed_host() {
    for (raw, written) in [
        ("https://API.GitHub.com/user", "https://api.github.com/user"),
        (
            "https://api.github.com:443/user",
            "https://api.github.com/user",
        ),
        (
            "https://api.github.com/a/../user",
            "https://api.github.com/user",
        ),
        (
            "https://api%2egithub.com/user",
            "https://api.github.com/user",
        ),
        ("https:api.github.com/user", "https://api.github.com/user"),
    ] {
        let rig = Rig::new(b"{}");
        let secrets = rig.secrets();
        let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
        requests.request("github", get(raw)).unwrap();
        assert!(
            rig.config().starts_with(&format!("url = \"{written}\"\n")),
            "{raw}: {}",
            rig.config()
        );
        assert_eq!(rig.events()[0], "allows https api.github.com 443", "{raw}");
    }
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    assert_eq!(
        requests.request("github", get("https://api.github.com@evil.example/user")),
        Err(StoreError::code("net_url_invalid"))
    );
    let backslash = Rig::new(b"{}");
    let secrets = backslash.secrets();
    let requests = Requests::new(&secrets, &backslash.egress, &backslash.rules);
    requests
        .request("github", get("https://evil.example\\api.github.com/user"))
        .unwrap();
    assert_eq!(backslash.events()[0], "allows https evil.example 443");
    assert!(
        backslash
            .config()
            .starts_with("url = \"https://evil.example/api.github.com/user\"\n")
    );
}

#[test]
fn the_config_never_follows_redirects() {
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let token = secrets.put_from_owner("github", "token").unwrap();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get("https://api.github.com/user");
    request.method = Method::Patch;
    request.bearer = Some(token);
    request.headers = vec![Field {
        name: "Accept".to_owned(),
        value: "application/vnd.github+json".to_owned(),
    }];
    request.json = Some(br#"{"a":1}"#.to_vec());
    requests.request("github", request).unwrap();
    let keys: Vec<String> = rig
        .config()
        .lines()
        .map(|line| line.split(" = ").next().unwrap().to_owned())
        .collect();
    for key in &keys {
        assert!(
            [
                "url",
                "resolve",
                "user-agent",
                "header",
                "data",
                "request",
                "data-raw"
            ]
            .contains(&key.as_str()),
            "{key}"
        );
    }
}

#[test]
fn the_config_is_the_pinned_format() {
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let token = secrets.put_from_owner("github", "token").unwrap();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get("https://github.com/login/device/code");
    request.method = Method::Post;
    request.bearer = Some(token);
    request.headers = vec![Field {
        name: "Accept".to_owned(),
        value: "application/json".to_owned(),
    }];
    request.form = vec![Field {
        name: "client_id".to_owned(),
        value: "Iv1.a b&c".to_owned(),
    }];
    request.secret_form = vec![SecretField {
        name: "client_secret".to_owned(),
        secret: token,
    }];
    requests.request("github", request).unwrap();
    let token = std::str::from_utf8(TOKEN).unwrap();
    assert_eq!(
        rig.config(),
        format!(
            "url = \"https://github.com/login/device/code\"\nresolve = \"github.com:443:140.82.112.4\"\nuser-agent = \"CrazyAgent\"\nheader = \"Authorization: Bearer {token}\"\nheader = \"Accept: application/json\"\ndata = \"client_id=Iv1.a+b%26c&client_secret={token}\"\n"
        )
    );
    for (method, json, tail) in [
        (Method::Put, None, "request = \"PUT\"\ndata-raw = \"\"\n"),
        (Method::Delete, None, "request = \"DELETE\"\n"),
        (
            Method::Post,
            Some(br#"{"a":"\"b"}"#.to_vec()),
            "request = \"POST\"\nheader = \"Content-Type: application/json\"\ndata-raw = \"{\\\"a\\\":\\\"\\\\\\\"b\\\"}\"\n",
        ),
    ] {
        let rig = Rig::new(b"{}");
        let secrets = rig.secrets();
        let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
        let mut request = get("https://api.github.com/repos/o/r");
        request.method = method;
        request.json = json;
        requests.request("github", request).unwrap();
        assert_eq!(
            rig.config(),
            format!(
                "url = \"https://api.github.com/repos/o/r\"\nresolve = \"api.github.com:443:140.82.112.4\"\nuser-agent = \"CrazyAgent\"\n{tail}"
            )
        );
    }
}

#[test]
fn a_body_on_a_get_or_both_body_kinds_is_refused() {
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut json_get = get("https://api.github.com/user");
    json_get.json = Some(b"{}".to_vec());
    let mut both = get("https://api.github.com/user");
    both.method = Method::Post;
    both.json = Some(b"{}".to_vec());
    both.form = vec![Field {
        name: "a".to_owned(),
        value: "b".to_owned(),
    }];
    let mut not_text = get("https://api.github.com/user");
    not_text.method = Method::Post;
    not_text.json = Some(vec![0xff, 0xfe]);
    for request in [json_get, both, not_text] {
        assert_eq!(
            requests.request("github", request),
            Err(StoreError::code("net_request_invalid"))
        );
    }
    assert!(!rig.events().contains(&"send".to_owned()));
}

#[test]
fn only_the_named_reply_headers_come_back() {
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get("https://api.github.com/user");
    request.reply_headers = vec!["X-OAuth-Scopes".to_owned()];
    let reply = requests.request("github", request).unwrap();
    assert_eq!(
        reply.headers,
        [Field {
            name: "X-OAuth-Scopes".to_owned(),
            value: "repo".to_owned(),
        }]
    );
    assert_eq!(
        rig.egress.reply_headers.lock().unwrap()[0],
        ["x-oauth-scopes"]
    );
}

#[test]
fn a_host_with_a_port_is_captured_by_its_port_key() {
    let mut rig = Rig::new(&token_reply());
    rig.rules = CaptureRules::new(&[("github", "ghe.example:8443", &["access_token"])]).unwrap();
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let reply = requests
        .request(
            "github",
            get("https://ghe.example:8443/login/oauth/access_token"),
        )
        .unwrap();
    assert_eq!(reply.captured.len(), 1);
    assert!(!holds(CAPTURED.as_bytes(), &reply.body));
    let other = requests
        .request(
            "github",
            get("https://ghe.example/login/oauth/access_token"),
        )
        .unwrap();
    assert!(other.captured.is_empty());
}

#[test]
fn a_full_handle_table_refuses_a_capture_before_send() {
    let rig = Rig::new(&token_reply());
    for index in 0..PLUGIN_HANDLES_MAX {
        let found = reference(&format!("{NAMESPACE}.github"), &format!("n{index}")).unwrap();
        rig.handles.lock().unwrap().mint("github", found).unwrap();
    }
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    assert_eq!(
        requests.request("github", get("https://github.com/login/oauth/access_token")),
        Err(StoreError::code("secret_handles_full"))
    );
    assert!(!rig.events().contains(&"send".to_owned()));
}

#[test]
fn a_held_capture_fills_a_later_bearer_without_a_store_read() {
    let rig = Rig::new(&token_reply());
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let reply = requests
        .request("github", get("https://github.com/login/oauth/access_token"))
        .unwrap();
    let held = reply.captured[0].secret;
    let later = Rig::new(b"{}");
    let requests = Requests::new(&secrets, &later.egress, &later.rules);
    let mut request = get("https://api.github.com/user");
    request.bearer = Some(held);
    requests.request("github", request).unwrap();
    assert!(holds(CAPTURED.as_bytes(), later.config().as_bytes()));
    assert!(!rig.events().contains(&"get".to_owned()));
    assert_eq!(
        secrets.value("notes", held, "api.github.com").map(|_| ()),
        Err(StoreError::code("secret_handle_refused"))
    );
}

/// Runs one GET whose URL names a refused address, directly or through the fake
/// resolver, and checks it is refused before any secret is read or anything is sent.
/// Input: the URL.
fn refused_address(url: &str) {
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let token = secrets.put_from_owner("github", "token").unwrap();
    rig.egress.events.lock().unwrap().clear();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get(url);
    request.bearer = Some(token);
    assert_eq!(
        requests.request("github", request),
        Err(StoreError::code("net_url_invalid")),
        "{url}"
    );
    let events = rig.events();
    assert!(!events.contains(&"get".to_owned()), "{url}: {events:?}");
    assert!(!events.contains(&"send".to_owned()), "{url}: {events:?}");
}

#[test]
fn a_10_8_address_is_refused() {
    refused_address("https://10.0.0.1/v1");
}

#[test]
fn a_172_16_12_address_is_refused() {
    refused_address("https://172.16.0.1/v1");
    refused_address("https://172.31.255.254/v1");
}

#[test]
fn a_192_168_16_address_is_refused() {
    refused_address("https://192.168.1.10/v1");
}

#[test]
fn the_v4_metadata_address_is_refused() {
    refused_address("https://169.254.169.254/latest/meta-data/");
    refused_address("https://169.254.1.1/v1");
}

#[test]
fn a_v6_metadata_address_is_refused() {
    refused_address("https://[fd00:ec2::254]/latest/meta-data/");
    refused_address("https://[fe80::1]/v1");
}

#[test]
fn the_v4_unspecified_address_is_refused() {
    refused_address("https://0.0.0.0/v1");
    refused_address("http://0.0.0.0:8080/v1");
}

#[test]
fn the_v6_unspecified_address_is_refused() {
    refused_address("https://[::]/v1");
}

#[test]
fn a_v4_mapped_form_of_a_refused_address_is_refused() {
    refused_address("https://[::ffff:10.0.0.1]/v1");
    refused_address("https://[::ffff:169.254.169.254]/v1");
    refused_address("https://[::10.0.0.1]/v1");
    refused_address("https://[64:ff9b::a00:1]/v1");
}

#[test]
fn a_v6_unique_local_address_is_refused() {
    refused_address("https://[fc00::1]/v1");
    refused_address("https://[fd12:3456::1]/v1");
}

#[test]
fn a_name_resolving_to_a_private_address_is_refused() {
    refused_address("https://private.example/v1");
    refused_address("https://mixed.example/v1");
    refused_address("https://unresolved.example/v1");
}

#[test]
fn localhost_relaxes_no_address_rule() {
    let mut rebound = Rig::new(b"{}");
    rebound
        .egress
        .resolved
        .insert("localhost", vec!["192.168.0.5".parse().unwrap()]);
    let secrets = rebound.secrets();
    let requests = Requests::new(&secrets, &rebound.egress, &rebound.rules);
    assert_eq!(
        requests.request("github", get("http://localhost:8080/v1")),
        Err(StoreError::code("net_url_invalid"))
    );
    assert!(!rebound.events().contains(&"send".to_owned()));
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    requests
        .request("github", get("http://localhost:8080/v1"))
        .unwrap();
    assert!(
        rig.config()
            .contains("resolve = \"localhost:8080:127.0.0.1\"\n")
    );
}

#[test]
fn a_public_or_loopback_address_is_a_target() {
    for (url, allowed) in [
        ("https://140.82.112.4/v1", "allows https 140.82.112.4 443"),
        (
            "https://[2606:4700::1111]/v1",
            "allows https [2606:4700::1111] 443",
        ),
        ("http://127.0.0.1:8080/v1", "allows http 127.0.0.1 8080"),
        ("http://[::1]:8080/v1", "allows http [::1] 8080"),
        (
            "https://[::ffff:140.82.112.4]/v1",
            "allows https [::ffff:8c52:7004] 443",
        ),
    ] {
        let rig = Rig::new(b"{}");
        let secrets = rig.secrets();
        let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
        requests.request("github", get(url)).unwrap();
        assert_eq!(rig.events()[0], allowed, "{url}");
        assert!(!rig.config().contains("resolve = "), "{url}");
    }
}

#[test]
fn the_curl_config_pins_the_checked_address() {
    for (url, line) in [
        (
            "https://api.github.com/user",
            "resolve = \"api.github.com:443:140.82.112.4\"\n",
        ),
        (
            "https://v6.example:8443/user",
            "resolve = \"v6.example:8443:[2606:4700::1111]\"\n",
        ),
    ] {
        let rig = Rig::new(b"{}");
        let secrets = rig.secrets();
        let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
        requests.request("github", get(url)).unwrap();
        let config = rig.config();
        let lines: Vec<&str> = config.lines().collect();
        assert_eq!(format!("{}\n", lines[1]), line, "{url}");
        assert_eq!(config.matches("resolve = ").count(), 1, "{url}");
    }
}

#[test]
fn a_v4_translated_form_of_a_refused_address_is_refused() {
    refused_address("https://[::ffff:0:a9fe:a9fe]/v1");
    refused_address("https://[::ffff:0:a00:1]/v1");
}

#[test]
fn a_6to4_address_is_refused() {
    refused_address("https://[2002:a9fe:a9fe::1]/v1");
    refused_address("https://[2002:8c52:7004::1]/v1");
}

#[test]
fn a_teredo_address_is_refused() {
    refused_address("https://[2001:0:4136:e378:8000:63bf:3fff:fdd2]/v1");
}

#[test]
fn a_discard_only_address_is_refused() {
    refused_address("https://[100::1]/v1");
}

#[test]
fn a_192_0_0_address_is_refused() {
    refused_address("https://192.0.0.170/v1");
}

#[test]
fn a_198_18_benchmark_address_is_refused() {
    refused_address("https://198.18.0.1/v1");
    refused_address("https://198.19.255.254/v1");
}

#[test]
fn a_hex_integer_host_is_refused() {
    refused_address("https://0xa9fea9fe/v1");
}

#[test]
fn a_dotted_hex_host_is_refused() {
    refused_address("https://0xa9.0xfe.0xa9.0xfe/v1");
}

#[test]
fn a_dotted_octal_host_is_refused() {
    refused_address("https://0251.0376.0251.0376/v1");
}

#[test]
fn a_short_form_host_is_refused() {
    refused_address("https://169.254.43518/v1");
}

#[test]
fn an_integer_host_is_refused() {
    refused_address("https://2852039166/v1");
}

#[test]
fn a_hex_last_part_host_is_refused() {
    refused_address("https://1.2.3.0x4/v1");
}

#[test]
fn a_reply_over_its_bound_after_capture_mints_no_handle() {
    let mut body = String::from(r#"{"access_token":"gho_x","pad":["#);
    let items = 1_000_000;
    for index in 0..items {
        if index > 0 {
            body.push(',');
        }
        body.push_str("1e2");
    }
    body.push_str("]}");
    assert!(body.len() < REPLY_BYTES_MAX);
    let rig = Rig::new(body.as_bytes());
    let secrets = rig.secrets();
    let before = secrets.free_handles("github");
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get("https://github.com/login/oauth/access_token");
    request.capture = vec![capture("access_token", Some("token"))];
    assert_eq!(
        requests.request("github", request),
        Err(StoreError::code("cap_output_too_large"))
    );
    assert_eq!(secrets.free_handles("github"), before);
    assert!(rig.store.items.lock().unwrap().is_empty());
    assert!(rig.events().contains(&"send".to_owned()));
}

#[test]
fn reply_headers_over_their_bounds_mint_no_handle() {
    for headers in [
        vec![("X-Long".to_owned(), "a".repeat(REPLY_TEXT_BYTES_MAX + 1))],
        vec![("X-Many".to_owned(), "1".to_owned()); REPLY_LIST_MAX + 1],
    ] {
        let mut rig = Rig::new(&token_reply());
        rig.egress.headers = headers;
        let secrets = rig.secrets();
        let before = secrets.free_handles("github");
        let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
        let mut request = get("https://github.com/login/oauth/access_token");
        request.reply_headers = vec!["x-long".to_owned(), "x-many".to_owned()];
        assert_eq!(
            requests.request("github", request),
            Err(StoreError::code("cap_output_too_large"))
        );
        assert_eq!(secrets.free_handles("github"), before);
    }
    let rig = Rig::new(&token_reply());
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get("https://github.com/login/oauth/access_token");
    request.reply_headers = vec!["x-oauth-scopes".to_owned()];
    assert_eq!(
        requests.request("github", request).unwrap().captured.len(),
        1
    );
}

#[test]
fn a_captured_token_is_refused_at_a_listed_host_outside_its_set() {
    let rig = Rig::new(&token_reply());
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let reply = requests
        .request("github", get("https://github.com/login/oauth/access_token"))
        .unwrap();
    let held = reply.captured[0].secret;
    let mut request = get("https://uploads.github.com/v1");
    request.bearer = Some(held);
    assert_eq!(
        requests.request("github", request),
        Err(StoreError::code("secret_handle_refused"))
    );
    let sends = rig.events().iter().filter(|event| *event == "send").count();
    assert_eq!(sends, 1);
}

#[test]
fn an_owner_paste_is_refused_at_a_host_the_owner_did_not_approve() {
    let rig = Rig::new(b"{}");
    let secrets = rig.secrets();
    let token = secrets.put_from_owner("github", "token").unwrap();
    rig.egress.events.lock().unwrap().clear();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get("https://uploads.github.com/v1");
    request.bearer = Some(token);
    assert_eq!(
        requests.request("github", request),
        Err(StoreError::code("secret_handle_refused"))
    );
    let events = rig.events();
    assert!(!events.contains(&"get".to_owned()), "{events:?}");
    assert!(!events.contains(&"send".to_owned()), "{events:?}");
}

#[test]
fn a_found_name_without_a_declared_keep_as_cannot_be_sent() {
    let rig = Rig::new(&token_reply());
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get("https://github.com/login/oauth/access_token");
    request.capture = vec![capture("access_token", Some("token"))];
    requests.request("github", request).unwrap();
    let later = Mutex::new(Handles::new());
    let next = Secrets::for_request(&rig.store, NAMESPACE, &rig.scopes, &later, None);
    let found = next.find("github", "token").unwrap().unwrap();
    for host in ["github.com", "api.github.com"] {
        assert_eq!(
            next.value("github", found, host).map(|_| ()),
            Err(StoreError::code("secret_handle_refused")),
            "{host}"
        );
    }
    assert_eq!(next.forget("github", found), Ok(()));
}

#[test]
fn a_found_name_with_a_declared_keep_as_gets_its_rule_hosts() {
    let mut rig = Rig::new(&token_reply());
    rig.rules = CaptureRules::new(&[("github", "github.com", &["access_token"])])
        .unwrap()
        .with_sends(&[("github", "access_token", &["api.github.com"], Some("token"))])
        .unwrap();
    let secrets = rig.secrets();
    let requests = Requests::new(&secrets, &rig.egress, &rig.rules);
    let mut request = get("https://github.com/login/oauth/access_token");
    request.capture = vec![capture("access_token", Some("token"))];
    requests.request("github", request).unwrap();
    let kept: KeptHosts = rig.rules.kept_hosts();
    let later = Mutex::new(Handles::new());
    let next =
        Secrets::for_request(&rig.store, NAMESPACE, &rig.scopes, &later, None).with_kept(&kept);
    let found = next.find("github", "token").unwrap().unwrap();
    for host in ["github.com", "api.github.com"] {
        assert_eq!(
            next.value("github", found, host).unwrap().expose(),
            CAPTURED,
            "{host}"
        );
    }
    assert_eq!(
        next.value("github", found, "uploads.github.com")
            .map(|_| ()),
        Err(StoreError::code("secret_handle_refused"))
    );
}

#[test]
fn send_rules_refuse_an_uncaptured_field_a_bad_host_list_and_a_reused_keep_as() {
    let invalid = Err(StoreError::code("net_capture_rules_invalid"));
    let rules = || {
        CaptureRules::new(&[("github", "github.com", &["access_token", "device_code"])]).unwrap()
    };
    let many: Vec<String> = (0..=SEND_HOSTS_MAX)
        .map(|index| format!("h{index}.example"))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    for rows in [
        vec![("github", "refresh_token", &["api.github.com"][..], None)],
        vec![("github", "access_token", &[][..], None)],
        vec![(
            "github",
            "access_token",
            &["api.github.com", "api.github.com"][..],
            None,
        )],
        vec![("github", "access_token", &many[..], None)],
        vec![(
            "github",
            "access_token",
            &["api.github.com"][..],
            Some("Token"),
        )],
        vec![
            (
                "github",
                "access_token",
                &["api.github.com"][..],
                Some("token"),
            ),
            (
                "github",
                "device_code",
                &["api.github.com"][..],
                Some("token"),
            ),
        ],
        vec![
            ("github", "access_token", &["api.github.com"][..], None),
            ("github", "access_token", &["github.com"][..], None),
        ],
    ] {
        assert_eq!(rules().with_sends(&rows).map(|_| ()), invalid, "{rows:?}");
    }
    assert!(
        rules()
            .with_sends(&[(
                "github",
                "access_token",
                &many[..SEND_HOSTS_MAX],
                Some("token")
            )])
            .is_ok()
    );
}
