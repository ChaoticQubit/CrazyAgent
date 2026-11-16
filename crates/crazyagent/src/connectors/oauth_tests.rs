use super::*;
use crate::app_plugins::connector::{
    ConnectorSpec, McpEndpoint, OAuthSpec, ReadOnly, Registration,
};
use crate::connectors::mcp_client::tests::{Seen, Vendor, empty, json_reply, no_lookup, send};
use crate::connectors::mcp_client::{Access, Credential, McpError, McpSession};
use crate::connectors::settings::SizeCap;
use crate::connectors::tests::{Case, add, case};
use crate::notifications::{NoticeClass, NoticeFilter, NoticeKind, NoticeQuery, PAGE_MAX};
use crate::protocol::CredentialRef;
use crate::storage::accounts::PLUGIN_KEYS;
use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

const CLIENT: &str = "board-own-client-1";
const SECRET: &str = "own-secret-0001";
const CIMD: &str = "https://crazyagent.example/oauth/client-metadata.json";
const DCR_CLIENT: &str = "dcr-client-0001";
const CODE: &str = "code-from-the-browser-1";
const ACCESS: &str = "at-board-0001";
const REFRESH: &str = "rt-board-0001";
const REDIRECT: &str = "http://127.0.0.1:53682/callback";
const SHA: &str = "sha256-of-the-trusted-board-file";
const REFUSED: &str = "connector_address_refused";

const BOARD_OAUTH: &str = "schema: 1
kind: connector
name: board
title: Board
version: 1
mcp:
  url: https://mcp.board.example/mcp
  hosts: [mcp.board.example]
oauth:
  hosts: [auth.board.example]
  registration: [own, cimd, dcr]
  scopes_read: [read]
  scopes_write: [write]
";

/// How the stand-in vendor answers: the `resource_metadata` its 401 names (none for a bare
/// challenge), the authorization servers its resource metadata lists, the two endpoints
/// its server metadata gives, whether it promises `iss` in every answer, whether it takes
/// client metadata documents, its registration endpoint, the token endpoint's client
/// sign-in methods, the tokens and registered client it gives, the issuer its resource
/// metadata names instead once one is set, and the hold its token endpoint keeps renewals
/// in. Each `{base}` becomes the stand-in's own address.
#[derive(Clone)]
struct Shape {
    hint: Option<&'static str>,
    servers: Vec<&'static str>,
    authorize: &'static str,
    token: &'static str,
    iss: bool,
    cimd: bool,
    register: Option<&'static str>,
    methods: &'static [&'static str],
    access: &'static str,
    refresh: &'static str,
    client: &'static str,
    moved: Arc<Mutex<Option<String>>>,
    renewal: Arc<Mutex<&'static str>>,
    hold: Option<Hold>,
}

/// A token endpoint that keeps each renewal until the row lets it go or ten seconds pass:
/// it tells `entered` that a renewal waits, and marks `timed_out` once one gave up waiting.
#[derive(Clone)]
struct Hold {
    entered: Arc<Mutex<mpsc::Sender<()>>>,
    release: Arc<Mutex<mpsc::Receiver<()>>>,
    timed_out: Arc<AtomicBool>,
}

fn shape() -> Shape {
    Shape {
        hint: Some("{base}/.well-known/oauth-protected-resource/mcp"),
        servers: vec!["{base}"],
        authorize: "{base}/authorize",
        token: "{base}/token",
        iss: true,
        cimd: false,
        register: None,
        methods: &[],
        access: ACCESS,
        refresh: REFRESH,
        client: DCR_CLIENT,
        moved: Arc::new(Mutex::new(None)),
        renewal: Arc::new(Mutex::new("200 OK")),
        hold: None,
    }
}

/// A loopback vendor that is both the MCP server and its authorization server, answering
/// as the shape says. Input: the shape. Output: the running stand-in.
fn vendor(shape: Shape) -> Vendor {
    Vendor::start(move |seen, _, stream| {
        let base = format!("http://{}", seen.header("host").unwrap_or_default());
        let fill = |text: &str| text.replace("{base}", &base);
        let reply = match (seen.verb.as_str(), seen.path.as_str()) {
            ("POST", "/mcp") => {
                let challenge = match shape.hint {
                    Some(hint) => {
                        format!(
                            "Bearer resource_metadata=\"{}\", scope=\"read\"",
                            fill(hint)
                        )
                    }
                    None => "Bearer scope=\"read\"".to_owned(),
                };
                empty(
                    "401 Unauthorized",
                    &[("WWW-Authenticate", challenge.as_str())],
                )
            }
            ("GET", "/.well-known/oauth-protected-resource/mcp") => {
                let moved = shape.moved.lock().unwrap().clone();
                let servers: Vec<String> = match moved {
                    Some(issuer) => vec![issuer],
                    None => shape.servers.iter().copied().map(fill).collect(),
                };
                json_reply(
                    "200 OK",
                    &[],
                    &json!({"resource": fill("{base}/mcp"), "authorization_servers": servers}),
                )
            }
            ("GET", "/.well-known/oauth-authorization-server") => json_reply(
                "200 OK",
                &[],
                &json!({
                    "issuer": base,
                    "authorization_endpoint": fill(shape.authorize),
                    "token_endpoint": fill(shape.token),
                    "response_types_supported": ["code"],
                    "code_challenge_methods_supported": ["S256"],
                    "authorization_response_iss_parameter_supported": shape.iss,
                    "client_id_metadata_document_supported": shape.cimd,
                    "registration_endpoint": shape.register.map(fill),
                    "token_endpoint_auth_methods_supported": shape.methods,
                }),
            ),
            ("POST", "/register") => json_reply(
                "201 Created",
                &[],
                &json!({
                    "client_id": shape.client,
                    "token_endpoint_auth_method": "none",
                    "redirect_uris": seen.body["redirect_uris"],
                }),
            ),
            ("POST", "/token") => {
                let renewing = url::form_urlencoded::parse(seen.raw.as_bytes())
                    .any(|(key, value)| key == "grant_type" && value == "refresh_token");
                if renewing && let Some(hold) = &shape.hold {
                    let _ = hold.entered.lock().unwrap().send(());
                    let waited = hold
                        .release
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(10));
                    if waited.is_err() {
                        hold.timed_out.store(true, Ordering::SeqCst);
                    }
                }
                let suffix = if renewing { "-renewed" } else { "" };
                let status = *shape.renewal.lock().unwrap();
                if renewing && status != "200 OK" {
                    json_reply(status, &[], &json!({"error": "invalid_grant"}))
                } else {
                    json_reply(
                        "200 OK",
                        &[("Cache-Control", "no-store")],
                        &json!({
                            "access_token": format!("{}{suffix}", shape.access),
                            "token_type": "Bearer",
                            "expires_in": 3600,
                            "refresh_token": format!("{}{suffix}", shape.refresh),
                        }),
                    )
                }
            }
            _ => empty("404 Not Found", &[]),
        };
        send(stream, &reply);
    })
}

struct Sign {
    case: Case,
    vendor: Vendor,
    spec: ConnectorSpec,
    sha256: String,
}

impl Sign {
    fn issuer(&self) -> String {
        format!("http://127.0.0.1:{}", self.vendor.port)
    }

    fn resource(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.vendor.port)
    }

    fn paths(&self) -> Vec<String> {
        self.vendor
            .seen()
            .iter()
            .map(|seen| format!("{} {}", seen.verb, seen.path))
            .collect()
    }

    fn token_requests(&self) -> Vec<Seen> {
        self.vendor
            .seen()
            .into_iter()
            .filter(|seen| seen.path == "/token")
            .collect()
    }
}

/// A real engine root with the trusted oauth-only `board` connector and the owner's own
/// client saved for it, signing in at a loopback stand-in. Inputs: the stand-in's shape and
/// the OAuth hosts declared. Output: the case, the stand-in and the connector as declared.
fn signing(shape: Shape, oauth_hosts: &[&str]) -> Sign {
    prepared(
        shape,
        oauth_hosts,
        &[Registration::Own],
        Some((CLIENT, None)),
    )
}

/// [`signing`] with the registration ways declared and the owner's own client, id and
/// secret, saved or not.
fn prepared(
    shape: Shape,
    oauth_hosts: &[&str],
    registration: &[Registration],
    own: Option<(&str, Option<&str>)>,
) -> Sign {
    let case = case();
    add(&case, BOARD_OAUTH, false);
    if let Some((id, secret)) = own {
        assert_eq!(
            set_own_client(&case.db, &case.root, "board", id, secret),
            Ok(())
        );
    }
    let vendor = vendor(shape);
    let spec = ConnectorSpec {
        mcp: vendor.endpoint(),
        key: None,
        oauth: Some(OAuthSpec {
            hosts: oauth_hosts.iter().map(|host| (*host).to_owned()).collect(),
            registration: registration.to_vec(),
            scopes_read: vec!["read".to_owned()],
            scopes_write: vec!["write".to_owned()],
        }),
        read_tools: Vec::new(),
    };
    Sign {
        case,
        vendor,
        spec,
        sha256: SHA.to_owned(),
    }
}

fn redirect() -> url::Url {
    url::Url::parse(REDIRECT).unwrap()
}

fn start(sign: &Sign, writes: bool) -> Result<Pending, OAuthError> {
    start_with(sign, writes, None)
}

fn start_with(sign: &Sign, writes: bool, cimd: Option<&url::Url>) -> Result<Pending, OAuthError> {
    begin_with(
        &sign.case.root,
        "board",
        &sign.sha256,
        &sign.spec,
        &redirect(),
        writes,
        cimd,
        &no_lookup(),
    )
}

fn begun(sign: &Sign, writes: bool) -> Pending {
    begun_with(sign, writes, None)
}

fn begun_with(sign: &Sign, writes: bool, cimd: Option<&url::Url>) -> Pending {
    let begun = start_with(sign, writes, cimd);
    assert!(begun.is_ok(), "a declared sign-in begins: {begun:?}");
    begun.unwrap()
}

fn finished(sign: &Sign, state: &str, iss: Option<&str>) -> Result<(), OAuthError> {
    finish_with(&sign.case.root, state, CODE, iss, &no_lookup())
}

/// The values of the keys saved under one name prefix in `plugin-keys`, by name.
fn saved(case: &Case, prefix: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(case.root.join(PLUGIN_KEYS)) else {
        return Vec::new();
    };
    let mut found: Vec<(String, String)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            name.starts_with(prefix)
                .then(|| (name, std::fs::read_to_string(entry.path()).unwrap()))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, value)| value).collect()
}

fn query(url: &url::Url) -> BTreeMap<String, String> {
    url.query_pairs().into_owned().collect()
}

fn form(seen: &Seen) -> BTreeMap<String, String> {
    url::form_urlencoded::parse(seen.raw.as_bytes())
        .into_owned()
        .collect()
}

/// The smallest real check: an answer that names another issuer, or none where the server
/// promised one, never reaches the token endpoint and ends its sign-in.
#[test]
fn a_code_with_a_mismatched_iss_is_never_redeemed() {
    let sign = signing(shape(), &["127.0.0.1"]);
    let issuer = sign.issuer();
    let good = begun(&sign, false);
    assert_eq!(finished(&sign, &good.state, Some(&issuer)), Ok(()));
    assert_eq!(sign.token_requests().len(), 1);
    assert_eq!(
        saved(&sign.case, "connector-token-board-"),
        vec![ACCESS.to_owned()]
    );
    assert_eq!(
        finished(&sign, &good.state, Some(&issuer)),
        Err(OAuthError("connector_oauth_state_unknown")),
        "a state is answered once"
    );
    let slash = format!("{issuer}/");
    for iss in [
        Some("http://127.0.0.1:1"),
        Some(slash.as_str()),
        Some(""),
        None,
    ] {
        let pending = begun(&sign, false);
        assert_eq!(
            finished(&sign, &pending.state, iss),
            Err(OAuthError("connector_oauth_issuer_mismatch")),
            "{iss:?}"
        );
        assert_eq!(
            finished(&sign, &pending.state, Some(&issuer)),
            Err(OAuthError("connector_oauth_state_unknown")),
            "a refused answer ends its sign-in: {iss:?}"
        );
    }
    assert_eq!(
        sign.token_requests().len(),
        1,
        "no code met the token endpoint after a wrong or missing iss"
    );

    let quiet = signing(
        Shape {
            iss: false,
            ..shape()
        },
        &["127.0.0.1"],
    );
    let pending = begun(&quiet, false);
    assert_eq!(
        finished(&quiet, &pending.state, Some("http://127.0.0.1:1")),
        Err(OAuthError("connector_oauth_issuer_mismatch")),
        "an iss given is checked where the server promised none"
    );
    let pending = begun(&quiet, false);
    assert_eq!(finished(&quiet, &pending.state, None), Ok(()));
    assert_eq!(quiet.token_requests().len(), 1);
}

/// The authorization request carries the S256 challenge of the verifier the token request
/// then sends, and both name the MCP server as the resource, the same redirect and client.
#[test]
fn pkce_s256_and_resource_are_in_both_requests() {
    let sign = signing(shape(), &["127.0.0.1"]);
    let pending = begun(&sign, true);
    let asked = query(&pending.authorize_url);
    assert_eq!(pending.authorize_url.host_str(), Some("127.0.0.1"));
    assert_eq!(pending.authorize_url.port(), Some(sign.vendor.port));
    assert_eq!(pending.authorize_url.path(), "/authorize");
    assert_eq!(asked["response_type"], "code");
    assert_eq!(asked["client_id"], CLIENT);
    assert_eq!(asked["redirect_uri"], REDIRECT);
    assert_eq!(asked["state"], pending.state);
    assert_eq!(asked["code_challenge_method"], "S256");
    assert_eq!(asked["resource"], sign.resource());
    assert_eq!(asked["scope"], "read write");
    assert!(pending.state.len() >= 43, "a state of 256 random bits");
    let probe = &sign.vendor.seen()[0];
    assert_eq!((probe.verb.as_str(), probe.path.as_str()), ("POST", "/mcp"));
    assert_eq!(probe.header("authorization"), None);

    assert_eq!(
        finished(&sign, &pending.state, Some(&sign.issuer())),
        Ok(())
    );
    let tokens = sign.token_requests();
    assert_eq!(tokens.len(), 1);
    let sent = form(&tokens[0]);
    assert_eq!(sent["grant_type"], "authorization_code");
    assert_eq!(sent["code"], CODE);
    assert_eq!(sent["redirect_uri"], REDIRECT);
    assert_eq!(sent["client_id"], CLIENT);
    assert_eq!(sent["resource"], sign.resource());
    let verifier = &sent["code_verifier"];
    let unreserved = |byte: u8| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte);
    assert!((43..=128).contains(&verifier.len()));
    assert!(verifier.bytes().all(unreserved));
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(asked["code_challenge"], challenge);
    assert_eq!(
        tokens[0].header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(tokens[0].header("authorization"), None);
    assert_eq!(
        saved(&sign.case, "connector-refresh-board-"),
        vec![REFRESH.to_owned()]
    );

    let again = query(&begun(&sign, false).authorize_url);
    assert_ne!(again["state"], asked["state"]);
    assert_ne!(again["code_challenge"], asked["code_challenge"]);
    assert_eq!(
        again["scope"], "read",
        "writes off asks for the read scopes only"
    );
}

/// Every address of a sign-in is checked before it is used: resource metadata on a host
/// the file declares, and the authorization server and both its endpoints on a declared
/// OAuth host. A refused one ends the sign-in before anything is sent there.
#[test]
fn an_authorization_url_on_an_undeclared_host_is_refused() {
    let sign = signing(shape(), &["127.0.0.1"]);
    assert_eq!(
        begun(&sign, false).authorize_url.host_str(),
        Some("127.0.0.1")
    );
    let bare = signing(
        Shape {
            hint: None,
            ..shape()
        },
        &["127.0.0.1"],
    );
    assert_eq!(
        begun(&bare, false).authorize_url.host_str(),
        Some("127.0.0.1")
    );
    assert_eq!(
        bare.paths(),
        vec![
            "POST /mcp",
            "GET /.well-known/oauth-protected-resource/mcp",
            "GET /.well-known/oauth-authorization-server",
        ],
        "with no hint the resource metadata is read at the MCP host's well-known address"
    );

    let evil = "https://evil.example";
    for (bent, hosts, reached) in [
        (
            Shape {
                authorize: "https://evil.example/authorize",
                ..shape()
            },
            &["127.0.0.1"][..],
            3,
        ),
        (
            Shape {
                token: "https://evil.example/token",
                ..shape()
            },
            &["127.0.0.1"][..],
            3,
        ),
        (
            Shape {
                servers: vec![evil],
                ..shape()
            },
            &["127.0.0.1"][..],
            2,
        ),
        (
            Shape {
                hint: Some("https://evil.example/.well-known/oauth-protected-resource"),
                ..shape()
            },
            &["127.0.0.1"][..],
            1,
        ),
        (shape(), &["auth.board.example"][..], 2),
    ] {
        let sign = signing(bent, hosts);
        let refused = start(&sign, false);
        assert_eq!(refused, Err(OAuthError(REFUSED)), "{:?}", sign.paths());
        assert_eq!(sign.vendor.seen().len(), reached, "{:?}", sign.paths());
    }
}

fn document() -> url::Url {
    url::Url::parse(CIMD).unwrap()
}

fn registrations(sign: &Sign) -> Vec<Seen> {
    sign.vendor
        .seen()
        .into_iter()
        .filter(|seen| seen.path == "/register")
        .collect()
}

/// With no own client, the app's client metadata document is the client where the server
/// takes one and the build has its address; otherwise the app registers itself once per
/// issuer (RFC 7591) and uses that client again; with neither the owner needs an own app.
#[test]
fn cimd_is_used_when_advertised_else_dcr() {
    let both = [Registration::Cimd, Registration::Dcr];
    let open = Shape {
        cimd: true,
        register: Some("{base}/register"),
        ..shape()
    };
    let advertised = prepared(open.clone(), &["127.0.0.1"], &both, None);
    let pending = begun_with(&advertised, false, Some(&document()));
    assert_eq!(query(&pending.authorize_url)["client_id"], CIMD);
    assert_eq!(
        finished(&advertised, &pending.state, Some(&advertised.issuer())),
        Ok(())
    );
    let token = &advertised.token_requests()[0];
    assert_eq!(form(token)["client_id"], CIMD);
    assert_eq!(token.header("authorization"), None);
    assert!(registrations(&advertised).is_empty());

    for (bent, cimd) in [
        (
            Shape {
                cimd: false,
                ..open.clone()
            },
            Some(document()),
        ),
        (open.clone(), None),
    ] {
        let sign = prepared(bent, &["127.0.0.1"], &both, None);
        let first = begun_with(&sign, false, cimd.as_ref());
        assert_eq!(query(&first.authorize_url)["client_id"], DCR_CLIENT);
        let registered = registrations(&sign);
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].verb, "POST");
        assert_eq!(registered[0].body["redirect_uris"], json!([REDIRECT]));
        assert_eq!(registered[0].body["token_endpoint_auth_method"], "none");
        assert_eq!(
            registered[0].body["grant_types"],
            json!(["authorization_code", "refresh_token"])
        );
        assert_eq!(registered[0].body["response_types"], json!(["code"]));
        assert_eq!(registered[0].header("authorization"), None);
        let again = begun_with(&sign, false, cimd.as_ref());
        assert_eq!(query(&again.authorize_url)["client_id"], DCR_CLIENT);
        assert_eq!(
            registrations(&sign).len(),
            1,
            "a registered client is used again at its issuer"
        );
        assert_eq!(finished(&sign, &again.state, Some(&sign.issuer())), Ok(()));
        assert_eq!(form(&sign.token_requests()[0])["client_id"], DCR_CLIENT);
    }

    let only = prepared(
        Shape {
            cimd: false,
            ..open.clone()
        },
        &["127.0.0.1"],
        &[Registration::Cimd],
        None,
    );
    let closed = prepared(
        Shape {
            register: None,
            cimd: false,
            ..shape()
        },
        &["127.0.0.1"],
        &both,
        None,
    );
    for sign in [&only, &closed] {
        assert_eq!(
            start_with(sign, false, Some(&document())),
            Err(OAuthError("connector_oauth_needs_own_app"))
        );
        assert!(registrations(sign).is_empty());
    }
    let elsewhere = prepared(
        Shape {
            register: Some("https://evil.example/register"),
            ..shape()
        },
        &["127.0.0.1"],
        &[Registration::Dcr],
        None,
    );
    assert_eq!(
        start_with(&elsewhere, false, None),
        Err(OAuthError(REFUSED)),
        "a registration endpoint on an undeclared host is never called"
    );
}

/// The owner's own OAuth app comes before the app's document and registration, and its
/// secret goes to the token endpoint alone: in a Basic header, or in the body where the
/// server takes only that.
#[test]
fn own_client_wins_over_cimd() {
    let all = [Registration::Own, Registration::Cimd, Registration::Dcr];
    let open = Shape {
        cimd: true,
        register: Some("{base}/register"),
        ..shape()
    };
    let sign = prepared(
        open.clone(),
        &["127.0.0.1"],
        &all,
        Some((CLIENT, Some(SECRET))),
    );
    let pending = begun_with(&sign, false, Some(&document()));
    assert_eq!(query(&pending.authorize_url)["client_id"], CLIENT);
    assert!(!pending.authorize_url.as_str().contains(SECRET));
    assert_eq!(
        finished(&sign, &pending.state, Some(&sign.issuer())),
        Ok(())
    );
    let token = &sign.token_requests()[0];
    let basic =
        base64::engine::general_purpose::STANDARD.encode(format!("{CLIENT}:{SECRET}").as_bytes());
    assert_eq!(
        token.header("authorization"),
        Some(format!("Basic {basic}").as_str())
    );
    assert!(!form(token).contains_key("client_secret"));
    assert!(registrations(&sign).is_empty());
    for seen in sign.vendor.seen() {
        if seen.path != "/token" {
            assert!(!seen.raw.contains(SECRET), "{}", seen.path);
            assert_eq!(seen.header("authorization"), None, "{}", seen.path);
        }
    }

    let posted = prepared(
        Shape {
            methods: &["client_secret_post"],
            ..open.clone()
        },
        &["127.0.0.1"],
        &all,
        Some((CLIENT, Some(SECRET))),
    );
    let pending = begun_with(&posted, false, Some(&document()));
    assert_eq!(
        finished(&posted, &pending.state, Some(&posted.issuer())),
        Ok(())
    );
    let token = &posted.token_requests()[0];
    assert_eq!(token.header("authorization"), None);
    let sent = form(token);
    assert_eq!(sent["client_id"], CLIENT);
    assert_eq!(sent["client_secret"], SECRET);

    let unsaved = prepared(open, &["127.0.0.1"], &all, None);
    let pending = begun_with(&unsaved, false, Some(&document()));
    assert_eq!(query(&pending.authorize_url)["client_id"], CIMD);
}

/// A sign-in is redeemed through `finish` only while the connector's file is the one
/// trusted when it began; a file trusted again since ends it with nothing sent.
#[test]
fn a_sign_in_whose_connector_file_changed_since_it_began_is_never_redeemed() {
    let sign = signing(shape(), &["127.0.0.1"]);
    let trusted =
        crate::app_plugins::store::trusted_connector(&sign.case.db, &sign.case.root, "board")
            .unwrap()
            .sha256;
    let begun_at = |sha: &str| {
        let begun = begin_with(
            &sign.case.root,
            "board",
            sha,
            &sign.spec,
            &redirect(),
            false,
            None,
            &no_lookup(),
        );
        assert!(begun.is_ok(), "{begun:?}");
        begun.unwrap()
    };
    let good = begun_at(&trusted);
    assert_eq!(
        finish(
            &sign.case.db,
            &sign.case.root,
            &good.state,
            CODE,
            Some(&sign.issuer())
        ),
        Ok(())
    );
    assert_eq!(sign.token_requests().len(), 1);
    let stale = begun_at(SHA);
    assert_eq!(
        finish(
            &sign.case.db,
            &sign.case.root,
            &stale.state,
            CODE,
            Some(&sign.issuer())
        ),
        Err(OAuthError("connector_oauth_trust_changed"))
    );
    assert_eq!(
        finish(
            &sign.case.db,
            &sign.case.root,
            &stale.state,
            CODE,
            Some(&sign.issuer())
        ),
        Err(OAuthError("connector_oauth_state_unknown"))
    );
    assert_eq!(
        sign.token_requests().len(),
        1,
        "a changed file's code is never sent"
    );
}

/// An access token longer than a plain key, over 1 KiB, as some vendors give.
fn long(prefix: &str) -> &'static str {
    Box::leak(format!("{prefix}{}", "x".repeat(1500)).into_boxed_str())
}

/// Where the engine saves one kind of `board` token from one issuer.
fn named(kind: &str, issuer: &str) -> CredentialRef {
    let digest = Sha256::digest(issuer.as_bytes());
    let short: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    CredentialRef::new(format!("connector-{kind}-board-{short}")).unwrap()
}

fn clock() -> i64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    i64::try_from(since.as_millis()).unwrap()
}

fn fresh_at(sign: &Sign, now_ms: i64) -> Result<Credential, OAuthError> {
    fresh_for(sign, &sign.spec, now_ms)
}

/// [`fresh_at`] for the connector declared as `spec` instead of the sign's own.
fn fresh_for(sign: &Sign, spec: &ConnectorSpec, now_ms: i64) -> Result<Credential, OAuthError> {
    fresh_with(
        &sign.case.db,
        &sign.case.root,
        "board",
        "Board",
        &sign.sha256,
        spec,
        now_ms,
        &no_lookup(),
    )
}

/// The sign's connector declared with `read_only` as its read-only address.
fn read_only_at(sign: &Sign, read_only: &str) -> ConnectorSpec {
    ConnectorSpec {
        mcp: McpEndpoint {
            read_only: ReadOnly::Url(url::Url::parse(read_only).unwrap()),
            ..sign.spec.mcp.clone()
        },
        ..sign.spec.clone()
    }
}

/// An OAuth token goes only where the sign-in's resource reaches: a read-only address
/// outside it is refused before a sign-in begins, and a sign-in is never handed out for a
/// declaration whose read-only address it does not cover; one under the resource is used.
#[test]
fn an_oauth_token_never_goes_to_a_read_only_address_outside_its_resource() {
    let sign = signing(shape(), &["127.0.0.1"]);
    let pending = begun(&sign, false);
    assert_eq!(
        finished(&sign, &pending.state, Some(&sign.issuer())),
        Ok(())
    );
    let outside = read_only_at(&sign, &format!("{}/ro", sign.issuer()));
    let under = read_only_at(&sign, &format!("{}/readonly", sign.resource()));
    let seen = sign.vendor.seen().len();
    assert_eq!(
        fresh_for(&sign, &outside, clock()),
        Err(OAuthError("connector_sign_in_needed")),
        "a token is never handed out for an address its resource does not cover"
    );
    assert_eq!(sign.vendor.seen().len(), seen);
    assert!(
        matches!(
            fresh_for(&sign, &under, clock()),
            Ok(Credential::OAuth { .. })
        ),
        "a read-only address under the resource gets the token"
    );
    let refused = begin_with(
        &sign.case.root,
        "board",
        SHA,
        &outside,
        &redirect(),
        false,
        None,
        &no_lookup(),
    );
    assert_eq!(
        refused.map(|pending| pending.state),
        Err(OAuthError("connector_oauth_metadata_invalid")),
        "a sign-in whose resource leaves the read-only address out never begins"
    );
    let allowed = begin_with(
        &sign.case.root,
        "board",
        SHA,
        &under,
        &redirect(),
        false,
        None,
        &no_lookup(),
    );
    assert!(allowed.is_ok(), "{allowed:?}");
}

/// A sign-in made under one file of a connector is never used under another: the next
/// credential read ends it with nothing sent, also when the new file moved its read-only
/// address outside the resource, and one Action item asks the owner to sign in again.
#[test]
fn a_sign_in_is_ended_once_its_connector_file_changed() {
    let mut sign = signing(shape(), &["127.0.0.1"]);
    let pending = begun(&sign, false);
    assert_eq!(
        finished(&sign, &pending.state, Some(&sign.issuer())),
        Ok(())
    );
    let now = fresh_at(&sign, clock());
    assert!(matches!(now, Ok(Credential::OAuth { .. })), "{now:?}");
    let seen = sign.vendor.seen().len();
    let moved = read_only_at(&sign, &format!("{}/ro", sign.issuer()));
    sign.sha256 = "sha256-of-a-changed-board-file".to_owned();
    assert_eq!(
        fresh_for(&sign, &moved, clock()),
        Err(OAuthError("connector_sign_in_needed")),
        "a sign-in is never handed out under another file"
    );
    assert_eq!(
        sign.vendor.seen().len(),
        seen,
        "nothing is sent for a changed file"
    );
    for prefix in [
        "connector-token-board-",
        "connector-refresh-board-",
        "connector-signin-board",
    ] {
        assert!(saved(&sign.case, prefix).is_empty(), "{prefix} is removed");
    }
    assert_eq!(
        notices(&mut sign.case),
        vec![(
            NoticeKind::ConnectorSignIn,
            NoticeClass::Action,
            "Board needs you to sign in again.".to_owned(),
            json!({"page": "connectors"}),
        )]
    );
    sign.sha256 = SHA.to_owned();
    assert_eq!(
        fresh_at(&sign, clock()),
        Err(OAuthError("connector_sign_in_needed")),
        "the ended sign-in is gone under the first file too"
    );
}

/// Removing a connector drops every key its sign-ins saved: tokens, the record, the client
/// registered at its issuer, and the owner's own app with its binding; a sign-in still
/// waiting is never redeemed, and another connector's keys stay.
#[test]
fn removing_a_connector_drops_every_key_its_sign_ins_saved() {
    let sign = prepared(
        Shape {
            register: Some("{base}/register"),
            ..shape()
        },
        &["127.0.0.1"],
        &[Registration::Own, Registration::Dcr],
        None,
    );
    let pending = begun(&sign, false);
    assert_eq!(query(&pending.authorize_url)["client_id"], DCR_CLIENT);
    assert_eq!(
        finished(&sign, &pending.state, Some(&sign.issuer())),
        Ok(())
    );
    assert_eq!(
        set_own_client(
            &sign.case.db,
            &sign.case.root,
            "board",
            CLIENT,
            Some(SECRET)
        ),
        Ok(())
    );
    let pending = begun(&sign, false);
    assert_eq!(query(&pending.authorize_url)["client_id"], CLIENT);
    assert_eq!(
        finished(&sign, &pending.state, Some(&sign.issuer())),
        Ok(())
    );
    let waiting = begun(&sign, false);
    for prefix in [
        "connector-token-board-",
        "connector-refresh-board-",
        "connector-signin-board",
        "connector-client-board-",
        "connector-secret-board-own",
        "connector-issuer-board-own",
    ] {
        assert!(!saved(&sign.case, prefix).is_empty(), "{prefix} is saved");
    }
    let another = CredentialRef::new("connector-token-board-two-0123456789abcdef").unwrap();
    crate::storage::accounts::replace_key_in(
        &sign.case.root,
        PLUGIN_KEYS,
        &another,
        b"kept".to_vec(),
    )
    .unwrap();
    let sent = token_calls(&sign.vendor).len();

    crate::app_plugins::store::remove(&sign.case.db, &sign.case.root, "board").unwrap();
    assert_eq!(
        saved(&sign.case, "connector-"),
        vec!["kept".to_owned()],
        "only another connector's key is left"
    );
    assert_eq!(
        finished(&sign, &waiting.state, Some(&sign.issuer())),
        Err(OAuthError("connector_oauth_state_unknown")),
        "a sign-in begun before the removal is never redeemed"
    );
    assert_eq!(token_calls(&sign.vendor).len(), sent);
}

/// The Authorization values the stand-in's MCP address was sent.
fn bearers(vendor: &Vendor) -> Vec<String> {
    vendor
        .seen()
        .iter()
        .filter(|seen| seen.path == "/mcp")
        .filter_map(|seen| seen.header("authorization").map(str::to_owned))
        .collect()
}

fn token_calls(vendor: &Vendor) -> Vec<Seen> {
    vendor
        .seen()
        .into_iter()
        .filter(|seen| seen.path == "/token")
        .collect()
}

/// Tokens, a registered client and the owner's own app stay with the issuer they came
/// from: a renewal goes to the token endpoint of its sign-in, never to a server the
/// resource names later; a sign-in at a second issuer registers again and leaves no token
/// of the first behind; the owner's app is refused at an issuer it was not first used at
/// until it is saved again.
#[test]
fn credentials_from_one_issuer_are_never_sent_to_another() {
    let registering = Shape {
        register: Some("{base}/register"),
        ..shape()
    };
    let second = vendor(Shape {
        access: "at-second-1",
        refresh: "rt-second-1",
        client: "dcr-second-1",
        ..registering.clone()
    });
    let moved = Arc::new(Mutex::new(None));
    let first_access = long("at-first-");
    let sign = prepared(
        Shape {
            access: first_access,
            refresh: "rt-first-1",
            client: "dcr-first-1",
            moved: moved.clone(),
            ..registering
        },
        &["127.0.0.1"],
        &[Registration::Dcr],
        None,
    );
    let first = sign.issuer();
    let other = format!("http://127.0.0.1:{}", second.port);
    let pending = begun(&sign, false);
    assert_eq!(finished(&sign, &pending.state, Some(&first)), Ok(()));

    let now = clock();
    let got = fresh_at(&sign, now);
    assert!(
        matches!(&got, Ok(Credential::OAuth { .. })),
        "a signed-in connector has its OAuth credential: {got:?}"
    );
    let Ok(Credential::OAuth {
        access,
        refresh,
        issuer,
        expires_ms,
    }) = got.clone()
    else {
        unreachable!()
    };
    assert_eq!(access, named("token", &first));
    assert_eq!(refresh, Some(named("refresh", &first)));
    assert_eq!(issuer, first);
    assert!(
        expires_ms > now && expires_ms <= now + 3_600_000,
        "{expires_ms}"
    );
    let opened = McpSession::open_with(
        &sign.case.root,
        &sign.spec.mcp,
        &got.unwrap(),
        Access::ReadOnly,
        SizeCap::Kib256,
        no_lookup(),
    )
    .err();
    assert!(
        matches!(opened, Some(McpError::Unauthorized { .. })),
        "{opened:?}"
    );
    let sent = bearers(&sign.vendor);
    let whole = format!("Bearer {first_access}");
    assert!(
        !sent.is_empty() && sent.iter().all(|value| *value == whole),
        "an access token over 1 KiB is read back and sent whole: {} sent",
        sent.len()
    );

    *moved.lock().unwrap() = Some(other.clone());
    let renewed = fresh_at(&sign, now + 2 * 3_600_000);
    assert!(
        matches!(&renewed, Ok(Credential::OAuth { issuer, .. }) if *issuer == first),
        "{renewed:?}"
    );
    assert!(
        second.seen().is_empty(),
        "a renewal asks nothing of a server the resource names later"
    );
    let calls = token_calls(&sign.vendor);
    assert_eq!(calls.len(), 2);
    let sent = form(&calls[1]);
    assert_eq!(sent["grant_type"], "refresh_token");
    assert_eq!(sent["refresh_token"], "rt-first-1");
    assert_eq!(sent["client_id"], "dcr-first-1");
    assert_eq!(sent["resource"], sign.resource());
    assert_eq!(calls[1].header("authorization"), None);

    let pending = begun(&sign, false);
    assert!(pending.authorize_url.as_str().starts_with(&other));
    assert_eq!(
        query(&pending.authorize_url)["client_id"],
        "dcr-second-1",
        "a second issuer gets a registration of its own"
    );
    assert_eq!(finished(&sign, &pending.state, Some(&other)), Ok(()));
    assert_eq!(
        saved(&sign.case, "connector-token-board-"),
        vec!["at-second-1".to_owned()]
    );
    assert_eq!(
        saved(&sign.case, "connector-refresh-board-"),
        vec!["rt-second-1".to_owned()]
    );
    let again = fresh_at(&sign, clock() + 2 * 3_600_000);
    assert!(
        matches!(&again, Ok(Credential::OAuth { access, issuer, .. })
            if *access == named("token", &other) && *issuer == other),
        "{again:?}"
    );
    assert_eq!(token_calls(&sign.vendor).len(), 2);
    let renewals = token_calls(&second);
    assert_eq!(renewals.len(), 2);
    assert_eq!(form(&renewals[1])["refresh_token"], "rt-second-1");
    assert_eq!(form(&renewals[1])["client_id"], "dcr-second-1");
    for seen in second.seen() {
        for gone in ["at-first-", "rt-first-1", "dcr-first-1"] {
            assert!(!seen.raw.contains(gone), "{} got {gone}", seen.path);
        }
        assert_eq!(seen.header("authorization"), None, "{}", seen.path);
    }
    for seen in sign.vendor.seen() {
        for gone in ["at-second-1", "rt-second-1", "dcr-second-1"] {
            assert!(!seen.raw.contains(gone), "{} got {gone}", seen.path);
        }
    }

    let third = vendor(shape());
    let elsewhere = format!("http://127.0.0.1:{}", third.port);
    let owned = Arc::new(Mutex::new(None));
    let own = prepared(
        Shape {
            moved: owned.clone(),
            ..shape()
        },
        &["127.0.0.1"],
        &[Registration::Own],
        Some((CLIENT, Some(SECRET))),
    );
    let pending = begun(&own, false);
    assert_eq!(finished(&own, &pending.state, Some(&own.issuer())), Ok(()));
    *owned.lock().unwrap() = Some(elsewhere.clone());
    assert_eq!(
        start(&own, false),
        Err(OAuthError("connector_oauth_issuer_mismatch")),
        "the owner's app is not taken to an issuer it was not first used at"
    );
    for seen in third.seen() {
        assert_ne!(seen.path, "/token");
        assert!(!seen.raw.contains(SECRET), "{}", seen.path);
        assert_eq!(seen.header("authorization"), None, "{}", seen.path);
    }
    assert_eq!(
        set_own_client(&own.case.db, &own.case.root, "board", CLIENT, Some(SECRET)),
        Ok(())
    );
    let pending = begun(&own, false);
    assert!(pending.authorize_url.as_str().starts_with(&elsewhere));
    assert_eq!(query(&pending.authorize_url)["client_id"], CLIENT);
}

fn stepped(sign: &Sign, challenge: &str, writes: bool) -> Result<Pending, OAuthError> {
    step_up_with(
        &sign.case.root,
        "board",
        SHA,
        &sign.spec,
        challenge,
        &redirect(),
        writes,
        None,
        &no_lookup(),
    )
}

/// A server that wants more scope (RFC 6750 `insufficient_scope`) gets a new sign-in for
/// the union of what was asked before and what it names, within the scopes the file
/// declares for the access allowed; after two such sign-ins there is no third until a
/// plain one, and a refused step-up asks nothing of the server.
#[test]
fn step_up_asks_for_the_union_and_stops_after_two_tries() {
    let sign = signing(shape(), &["127.0.0.1"]);
    let issuer = sign.issuer();
    let plain = begun(&sign, false);
    assert_eq!(query(&plain.authorize_url)["scope"], "read");
    assert_eq!(finished(&sign, &plain.state, Some(&issuer)), Ok(()));
    let quiet = sign.vendor.seen().len();
    for (challenge, writes) in [
        ("admin", true),
        ("write", false),
        ("read admin", true),
        ("", true),
        ("read\"", true),
    ] {
        assert_eq!(
            stepped(&sign, challenge, writes),
            Err(OAuthError("connector_oauth_scope_undeclared")),
            "{challenge:?} with writes {writes}"
        );
    }
    assert_eq!(
        sign.vendor.seen().len(),
        quiet,
        "a refused step-up asks nothing of the server"
    );
    for _ in 0..2 {
        let more = stepped(&sign, "write", true);
        assert!(more.is_ok(), "a declared scope is asked for: {more:?}");
        let more = more.unwrap();
        assert_eq!(query(&more.authorize_url)["scope"], "read write");
        assert_eq!(finished(&sign, &more.state, Some(&issuer)), Ok(()));
    }
    let quiet = sign.vendor.seen().len();
    assert_eq!(
        stepped(&sign, "write", true),
        Err(OAuthError("connector_oauth_step_up_limit"))
    );
    assert_eq!(
        sign.vendor.seen().len(),
        quiet,
        "a third step-up asks nothing"
    );
    let plain = begun(&sign, true);
    assert_eq!(finished(&sign, &plain.state, Some(&issuer)), Ok(()));
    let again = stepped(&sign, "read", false);
    assert!(
        again.is_ok(),
        "a plain sign-in starts the count again: {again:?}"
    );
    assert_eq!(
        query(&again.unwrap().authorize_url)["scope"],
        "read",
        "writes off keeps a write scope out of the union"
    );
}

/// Two sign-ins with the owner's own app begun at two issuers before either ends: the first
/// to end binds the app, and the other is refused before its code or the app's secret is
/// sent anywhere.
#[test]
fn the_owners_app_is_bound_once_when_two_sign_ins_race() {
    let third = vendor(shape());
    let elsewhere = format!("http://127.0.0.1:{}", third.port);
    let moved = Arc::new(Mutex::new(None));
    let own = prepared(
        Shape {
            moved: moved.clone(),
            ..shape()
        },
        &["127.0.0.1"],
        &[Registration::Own],
        Some((CLIENT, Some(SECRET))),
    );
    let here = begun(&own, false);
    *moved.lock().unwrap() = Some(elsewhere.clone());
    let there = begun(&own, false);
    assert!(there.authorize_url.as_str().starts_with(&elsewhere));
    assert_eq!(finished(&own, &here.state, Some(&own.issuer())), Ok(()));
    assert_eq!(
        finished(&own, &there.state, Some(&elsewhere)),
        Err(OAuthError("connector_oauth_issuer_mismatch"))
    );
    assert!(
        token_calls(&third).is_empty(),
        "the second issuer never gets the code or the secret"
    );
    assert_eq!(
        saved(&own.case, "connector-token-board-"),
        vec![ACCESS.to_owned()]
    );
}

/// Saving another own app ends the sign-in made with the one saved before and drops its
/// sign-ins still waiting: the old issuer gets no request after the save, so it never
/// sees the new app's id or secret, by a renewal or by a code, and one Action item asks
/// the owner to sign in again.
#[test]
fn saving_another_own_app_ends_the_sign_in_made_with_the_last_one() {
    let mut own = prepared(
        shape(),
        &["127.0.0.1"],
        &[Registration::Own],
        Some((CLIENT, Some(SECRET))),
    );
    let pending = begun(&own, false);
    assert_eq!(finished(&own, &pending.state, Some(&own.issuer())), Ok(()));
    let waiting = begun(&own, false);
    let sent = own.vendor.seen().len();
    assert_eq!(
        set_own_client(
            &own.case.db,
            &own.case.root,
            "board",
            "board-own-client-2",
            Some("own-secret-0002")
        ),
        Ok(())
    );
    assert_eq!(
        fresh_at(&own, clock() + 2 * 3_600_000),
        Err(OAuthError("connector_sign_in_needed")),
        "the last app's sign-in is never renewed with the new app"
    );
    assert_eq!(
        finished(&own, &waiting.state, Some(&own.issuer())),
        Err(OAuthError("connector_oauth_state_unknown")),
        "a sign-in begun with the last app is never redeemed"
    );
    assert_eq!(
        own.vendor.seen().len(),
        sent,
        "the old issuer is asked nothing after the save"
    );
    assert!(saved(&own.case, "connector-token-board-").is_empty());
    assert_eq!(
        notices(&mut own.case),
        vec![(
            NoticeKind::ConnectorSignIn,
            NoticeClass::Action,
            "Board needs you to sign in again.".to_owned(),
            json!({"page": "connectors"}),
        )]
    );
    let again = begun(&own, false);
    assert_eq!(
        query(&again.authorize_url)["client_id"],
        "board-own-client-2"
    );
}

/// Saving an own app changes how the connector signs in, so a sign-in made by registration
/// ends too: its tokens are not used with the new app, a sign-in begun by registration is
/// never redeemed, and one Action item asks the owner to sign in again.
#[test]
fn saving_an_own_app_ends_a_sign_in_made_by_registration() {
    let mut sign = prepared(
        Shape {
            register: Some("{base}/register"),
            ..shape()
        },
        &["127.0.0.1"],
        &[Registration::Own, Registration::Dcr],
        None,
    );
    let pending = begun(&sign, false);
    assert_eq!(query(&pending.authorize_url)["client_id"], DCR_CLIENT);
    let waiting = begun(&sign, false);
    assert_eq!(
        finished(&sign, &pending.state, Some(&sign.issuer())),
        Ok(())
    );
    let sent = sign.vendor.seen().len();
    assert_eq!(
        set_own_client(
            &sign.case.db,
            &sign.case.root,
            "board",
            CLIENT,
            Some(SECRET)
        ),
        Ok(())
    );
    assert_eq!(
        fresh_at(&sign, clock()),
        Err(OAuthError("connector_sign_in_needed")),
        "a registered client's tokens are not used once an own app is saved"
    );
    assert_eq!(
        finished(&sign, &waiting.state, Some(&sign.issuer())),
        Err(OAuthError("connector_oauth_state_unknown")),
        "a sign-in begun by registration is never redeemed after the save"
    );
    assert_eq!(sign.vendor.seen().len(), sent);
    assert!(saved(&sign.case, "connector-token-board-").is_empty());
    assert_eq!(
        notices(&mut sign.case),
        vec![(
            NoticeKind::ConnectorSignIn,
            NoticeClass::Action,
            "Board needs you to sign in again.".to_owned(),
            json!({"page": "connectors"}),
        )]
    );
}

/// An own app saved while a sign-in by registration is being answered wins: the answer
/// taken before the save is never redeemed, so no registered client's tokens are saved
/// once an own app is. A file that takes no own app still signs in by registration with
/// one saved.
#[test]
fn a_registered_sign_in_answered_while_an_own_app_is_saved_is_never_redeemed() {
    let registering = Shape {
        register: Some("{base}/register"),
        ..shape()
    };
    let sign = prepared(
        registering.clone(),
        &["127.0.0.1"],
        &[Registration::Own, Registration::Dcr],
        None,
    );
    let pending = begun(&sign, false);
    assert_eq!(query(&pending.authorize_url)["client_id"], DCR_CLIENT);
    let issuer = sign.issuer();
    let answered = finish_with_between(
        &sign.case.root,
        &pending.state,
        CODE,
        Some(&issuer),
        &no_lookup(),
        || {
            assert_eq!(
                set_own_client(
                    &sign.case.db,
                    &sign.case.root,
                    "board",
                    CLIENT,
                    Some(SECRET)
                ),
                Ok(())
            );
        },
    );
    assert_eq!(answered, Err(OAuthError("connector_oauth_state_unknown")));
    assert!(
        sign.token_requests().is_empty(),
        "the registered client's code is never sent"
    );
    assert!(saved(&sign.case, "connector-signin-board").is_empty());
    assert!(saved(&sign.case, "connector-token-board-").is_empty());

    let keyless = prepared(
        registering,
        &["127.0.0.1"],
        &[Registration::Dcr],
        Some((CLIENT, Some(SECRET))),
    );
    let pending = begun(&keyless, false);
    assert_eq!(
        query(&pending.authorize_url)["client_id"],
        DCR_CLIENT,
        "a file that takes no own app registers"
    );
    assert_eq!(
        finished(&keyless, &pending.state, Some(&keyless.issuer())),
        Ok(()),
        "and is redeemed though an own app is saved"
    );
}

/// The Action items waiting on the owner after one reconcile, as kind, class, text and
/// the page a click opens.
fn notices(case: &mut Case) -> Vec<(NoticeKind, NoticeClass, String, serde_json::Value)> {
    crate::notifications::initialize(&case.db).unwrap();
    crate::notifications::reconcile::reconcile(
        &mut case.db,
        &crate::config::ModelAccess::default(),
        clock(),
    )
    .unwrap();
    let query = NoticeQuery {
        filter: NoticeFilter::Pending,
        project_id: None,
        kind: None,
        fold: false,
        before: None,
        after: None,
        limit: PAGE_MAX,
    };
    crate::notifications::page(&case.db, &query)
        .unwrap()
        .items
        .into_iter()
        .map(|item| {
            let target = serde_json::to_value(&item.target).unwrap();
            (item.kind, item.class, item.text, target)
        })
        .collect()
}

fn renewals(vendor: &Vendor) -> usize {
    token_calls(vendor)
        .iter()
        .filter(|seen| form(seen).get("grant_type").map(String::as_str) == Some("refresh_token"))
        .count()
}

/// A renewal the token endpoint refuses ends the sign-in: one Action item asks the owner
/// to sign in again however often it is read, later calls ask the endpoint nothing, a
/// server that is only down raises nothing and keeps the tokens, and a new sign-in
/// resolves the item.
#[test]
fn a_failed_refresh_raises_one_sign_in_notice() {
    let renewal = Arc::new(Mutex::new("503 Service Unavailable"));
    let mut sign = signing(
        Shape {
            renewal: renewal.clone(),
            ..shape()
        },
        &["127.0.0.1"],
    );
    let trusted =
        crate::app_plugins::store::trusted_connector(&sign.case.db, &sign.case.root, "board")
            .unwrap()
            .sha256;
    sign.sha256 = trusted.clone();
    let signed_in = |sign: &Sign| {
        let begun = begin_with(
            &sign.case.root,
            "board",
            &trusted,
            &sign.spec,
            &redirect(),
            false,
            None,
            &no_lookup(),
        );
        assert!(begun.is_ok(), "{begun:?}");
        let state = begun.unwrap().state;
        finish(
            &sign.case.db,
            &sign.case.root,
            &state,
            CODE,
            Some(&sign.issuer()),
        )
    };
    assert_eq!(signed_in(&sign), Ok(()));
    let later = clock() + 2 * 3_600_000;

    assert_eq!(
        fresh_at(&sign, later),
        Err(OAuthError("connector_oauth_refused"))
    );
    assert!(
        notices(&mut sign.case).is_empty(),
        "a server that is down raises nothing"
    );
    assert_eq!(
        saved(&sign.case, "connector-refresh-board-"),
        vec![REFRESH.to_owned()],
        "and keeps the tokens"
    );

    *renewal.lock().unwrap() = "400 Bad Request";
    for _ in 0..2 {
        assert_eq!(
            fresh_at(&sign, later),
            Err(OAuthError("connector_sign_in_needed"))
        );
    }
    assert_eq!(
        renewals(&sign.vendor),
        2,
        "one renewal while down, one refused, none after"
    );
    assert!(
        saved(&sign.case, "connector-refresh-board-").is_empty(),
        "a refused refresh token is dropped"
    );
    let wanted = vec![(
        NoticeKind::ConnectorSignIn,
        NoticeClass::Action,
        "Board needs you to sign in again.".to_owned(),
        json!({"page": "connectors"}),
    )];
    assert_eq!(notices(&mut sign.case), wanted);
    assert_eq!(
        notices(&mut sign.case),
        wanted,
        "one item, however often it is read"
    );

    *renewal.lock().unwrap() = "200 OK";
    assert_eq!(signed_in(&sign), Ok(()));
    assert!(
        notices(&mut sign.case).is_empty(),
        "a new sign-in resolves it"
    );
    let now = fresh_at(&sign, clock());
    assert!(matches!(now, Ok(Credential::OAuth { .. })), "{now:?}");
}

/// A renewal waits only on renewals of its own connector: while one vendor's token
/// endpoint keeps a renewal, another connector of the same engine renews at once.
#[test]
fn a_slow_token_endpoint_holds_up_no_other_connectors_renewal() {
    let (entered_tx, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let hold = Hold {
        entered: Arc::new(Mutex::new(entered_tx)),
        release: Arc::new(Mutex::new(released)),
        timed_out: Arc::new(AtomicBool::new(false)),
    };
    let slow = signing(
        Shape {
            hold: Some(hold.clone()),
            ..shape()
        },
        &["127.0.0.1"],
    );
    let pending = begun(&slow, false);
    assert_eq!(
        finished(&slow, &pending.state, Some(&slow.issuer())),
        Ok(())
    );
    let quick = vendor(Shape {
        register: Some("{base}/register"),
        ..shape()
    });
    let desk = ConnectorSpec {
        mcp: quick.endpoint(),
        oauth: Some(OAuthSpec {
            registration: vec![Registration::Dcr],
            ..slow.spec.oauth.clone().unwrap()
        }),
        ..slow.spec.clone()
    };
    let desk_begun = begin_with(
        &slow.case.root,
        "desk",
        SHA,
        &desk,
        &redirect(),
        false,
        None,
        &no_lookup(),
    );
    assert!(desk_begun.is_ok(), "{desk_begun:?}");
    assert_eq!(
        finish_with(
            &slow.case.root,
            &desk_begun.unwrap().state,
            CODE,
            Some(&format!("http://127.0.0.1:{}", quick.port)),
            &no_lookup()
        ),
        Ok(())
    );
    let later = clock() + 2 * 3_600_000;
    let (root, spec) = (slow.case.root.clone(), slow.spec.clone());
    let waiting = std::thread::spawn(move || {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        fresh_with(
            &db,
            &root,
            "board",
            "Board",
            SHA,
            &spec,
            later,
            &no_lookup(),
        )
        .is_ok()
    });
    assert_eq!(
        entered.recv_timeout(Duration::from_secs(10)),
        Ok(()),
        "the slow renewal waits at its token endpoint"
    );
    let renewed = fresh_with(
        &slow.case.db,
        &slow.case.root,
        "desk",
        "Desk",
        SHA,
        &desk,
        later,
        &no_lookup(),
    );
    assert!(
        matches!(renewed, Ok(Credential::OAuth { .. })),
        "{renewed:?}"
    );
    assert!(
        !hold.timed_out.load(Ordering::SeqCst),
        "the other connector renewed without waiting on the slow one"
    );
    release.send(()).unwrap();
    assert!(waiting.join().unwrap(), "the slow renewal ends once let go");
    assert_eq!(renewals(&slow.vendor), 1);
    assert_eq!(renewals(&quick), 1);
}

/// Removing a connector resolves its "Sign in again" item with it.
#[test]
fn removing_a_connector_resolves_its_sign_in_notice() {
    let mut sign = signing(
        Shape {
            renewal: Arc::new(Mutex::new("400 Bad Request")),
            ..shape()
        },
        &["127.0.0.1"],
    );
    let pending = begun(&sign, false);
    assert_eq!(
        finished(&sign, &pending.state, Some(&sign.issuer())),
        Ok(())
    );
    assert_eq!(
        fresh_at(&sign, clock() + 2 * 3_600_000),
        Err(OAuthError("connector_sign_in_needed"))
    );
    assert_eq!(notices(&mut sign.case).len(), 1);
    crate::app_plugins::store::remove(&sign.case.db, &sign.case.root, "board").unwrap();
    assert!(
        notices(&mut sign.case).is_empty(),
        "a removed connector leaves no item behind"
    );
}

/// What the probes of 11-15 found for one Group O connector (PROBES-connectors.md): its MCP
/// address and read-only address, its MCP host and OAuth host, the scopes it asks to read
/// and to write, and whether its server promises the issuer in every answer.
struct Probed {
    name: &'static str,
    url: &'static str,
    read_only: Option<&'static str>,
    host: &'static str,
    oauth_host: &'static str,
    scopes_read: &'static [&'static str],
    scopes_write: &'static [&'static str],
    iss: bool,
}

const GROUP_O: [Probed; 3] = [
    Probed {
        name: "linear",
        url: "https://mcp.linear.app/mcp",
        read_only: Some("https://mcp.linear.app/mcp/readonly"),
        host: "mcp.linear.app",
        oauth_host: "mcp.linear.app",
        scopes_read: &["read"],
        scopes_write: &["write"],
        iss: true,
    },
    Probed {
        name: "notion",
        url: "https://mcp.notion.com/mcp",
        read_only: None,
        host: "mcp.notion.com",
        oauth_host: "mcp.notion.com",
        scopes_read: &[],
        scopes_write: &[],
        iss: true,
    },
    Probed {
        name: "atlassian",
        url: "https://mcp.atlassian.com/v2/mcp",
        read_only: None,
        host: "mcp.atlassian.com",
        oauth_host: "auth.atlassian.com",
        scopes_read: &[],
        scopes_write: &[],
        iss: false,
    },
];

/// A bundled connector's manifest as it ships. Input: its name. Output: the manifest.
fn shipped(name: &str) -> crate::app_plugins::manifest::Manifest {
    let text = crate::app_plugins::store::bundled()
        .iter()
        .find(|(bundled, _)| *bundled == name)
        .map(|(_, text)| *text);
    assert!(text.is_some(), "{name} ships with the app");
    crate::app_plugins::manifest::parse(text.unwrap())
        .unwrap_or_else(|problem| panic!("{name}: {problem:?}"))
}

/// A shipped connector declared at a loopback stand-in of its vendor: only the addresses
/// and the OAuth hosts are swapped, a read-only address kept under the resource.
fn at_stand_in(spec: &ConnectorSpec, stand_in: &Vendor) -> ConnectorSpec {
    let read_only = match spec.mcp.read_only {
        ReadOnly::Url(_) => ReadOnly::Url(
            url::Url::parse(&format!("http://127.0.0.1:{}/mcp/readonly", stand_in.port)).unwrap(),
        ),
        _ => ReadOnly::None,
    };
    ConnectorSpec {
        mcp: McpEndpoint {
            read_only,
            ..stand_in.endpoint()
        },
        oauth: spec.oauth.clone().map(|oauth| OAuthSpec {
            hosts: vec!["127.0.0.1".to_owned()],
            ..oauth
        }),
        ..spec.clone()
    }
}

/// Linear, Notion and Atlassian ship with the addresses, hosts, registration order and
/// scopes the probes found, and each signs in at a loopback stand-in of its vendor with
/// only the addresses swapped: by registration when no client document is given, by the
/// document once one is, and by the owner's own app once it is saved.
#[test]
fn every_group_o_connector_signs_in_against_a_loopback_server_with_its_declared_registration_order()
{
    for probe in GROUP_O {
        let name = probe.name;
        let manifest = shipped(name);
        let spec = manifest.connector().unwrap().clone();
        assert_eq!(spec.mcp.url.as_str(), probe.url, "{name}");
        assert_eq!(spec.mcp.hosts, [probe.host], "{name}");
        let read_only = match &spec.mcp.read_only {
            ReadOnly::Url(url) => Some(url.as_str()),
            _ => None,
        };
        assert_eq!(read_only, probe.read_only, "{name}");
        assert!(spec.oauth.is_some(), "{name} signs in by OAuth");
        let oauth = spec.oauth.clone().unwrap();
        assert_eq!(oauth.hosts, [probe.oauth_host], "{name}");
        assert_eq!(
            oauth.registration,
            [Registration::Own, Registration::Cimd, Registration::Dcr],
            "{name}"
        );
        assert_eq!(oauth.scopes_read, probe.scopes_read, "{name}");
        assert_eq!(oauth.scopes_write, probe.scopes_write, "{name}");

        for way in [Registration::Dcr, Registration::Cimd, Registration::Own] {
            let stand_in = vendor(Shape {
                iss: probe.iss,
                cimd: way == Registration::Cimd,
                register: Some("{base}/register"),
                ..shape()
            });
            let base = format!("http://127.0.0.1:{}", stand_in.port);
            let swapped = at_stand_in(&spec, &stand_in);
            let case = case();
            if way == Registration::Own {
                assert_eq!(
                    set_own_client(&case.db, &case.root, name, CLIENT, None),
                    Ok(()),
                    "{name}"
                );
            }
            let given = (way != Registration::Dcr).then(document);
            let begun = begin_with(
                &case.root,
                name,
                &manifest.sha256,
                &swapped,
                &redirect(),
                false,
                given.as_ref(),
                &no_lookup(),
            );
            assert!(begun.is_ok(), "{name} by {way:?}: {begun:?}");
            let pending = begun.unwrap();
            let asked = query(&pending.authorize_url);
            let client = match way {
                Registration::Dcr => DCR_CLIENT,
                Registration::Cimd => CIMD,
                Registration::Own => CLIENT,
            };
            assert_eq!(asked["client_id"], client, "{name} by {way:?}");
            let scope = (!probe.scopes_read.is_empty()).then(|| probe.scopes_read.join(" "));
            assert_eq!(
                asked.get("scope"),
                scope.as_ref(),
                "{name} by {way:?} asks only to read"
            );
            assert_eq!(asked["resource"], format!("{base}/mcp"), "{name}");
            assert_eq!(
                finish_with(
                    &case.root,
                    &pending.state,
                    CODE,
                    probe.iss.then_some(base.as_str()),
                    &no_lookup()
                ),
                Ok(()),
                "{name} by {way:?}"
            );
            let got = fresh_with(
                &case.db,
                &case.root,
                name,
                &manifest.title,
                &manifest.sha256,
                &swapped,
                clock(),
                &no_lookup(),
            );
            assert!(
                matches!(&got, Ok(Credential::OAuth { issuer, .. }) if *issuer == base),
                "{name} by {way:?}: {got:?}"
            );
        }
    }
}

/// A release that edits a bundled connector's file signs the owner out of it once: a
/// sign-in made under the file shipped before ends at the next credential read with
/// nothing sent, and one Action item asks the owner to sign in again.
#[test]
fn a_bundled_connector_file_edited_by_a_release_ends_its_sign_in_with_one_item() {
    let manifest = shipped("notion");
    let stand_in = vendor(Shape {
        register: Some("{base}/register"),
        ..shape()
    });
    let swapped = at_stand_in(manifest.connector().unwrap(), &stand_in);
    let mut case = case();
    let begun = begin_with(
        &case.root,
        "notion",
        "sha256-of-the-notion-file-shipped-before",
        &swapped,
        &redirect(),
        false,
        None,
        &no_lookup(),
    );
    assert!(begun.is_ok(), "{begun:?}");
    assert_eq!(
        finish_with(
            &case.root,
            &begun.unwrap().state,
            CODE,
            Some(&format!("http://127.0.0.1:{}", stand_in.port)),
            &no_lookup()
        ),
        Ok(())
    );
    let seen = stand_in.seen().len();
    assert_eq!(
        fresh(&case.db, &case.root, "notion"),
        Err(OAuthError("connector_sign_in_needed")),
        "a sign-in made under the old file is not used under the new one"
    );
    assert_eq!(stand_in.seen().len(), seen, "nothing is sent");
    assert!(saved(&case, "connector-token-notion-").is_empty());
    assert_eq!(
        notices(&mut case),
        vec![(
            NoticeKind::ConnectorSignIn,
            NoticeClass::Action,
            "Notion needs you to sign in again.".to_owned(),
            json!({"page": "connectors"}),
        )]
    );
}

/// An issuer named with one trailing slash on either side is the same issuer, as Box and
/// Google name theirs in resource metadata; nothing else is: not a second slash, another
/// path, another letter case or a written port.
#[test]
fn an_issuer_differing_only_by_one_trailing_slash_is_the_same_issuer_and_nothing_else_is() {
    for (asked, named) in [
        ("https://x/", "https://x"),
        ("https://x", "https://x/"),
        ("https://x", "https://x"),
        ("https://x/", "https://x/"),
        ("https://x/a/", "https://x/a"),
    ] {
        assert!(same_issuer(asked, named), "{asked} is {named}");
    }
    for (asked, named) in [
        ("https://x/a", "https://x"),
        ("https://x", "https://x/a"),
        ("https://x//", "https://x"),
        ("https://x/", "https://x//"),
        ("https://X", "https://x"),
        ("https://x:443", "https://x"),
        ("https://x/", "https://y"),
        ("https://x/", ""),
    ] {
        assert!(!same_issuer(asked, named), "{asked} is not {named}");
    }
}

/// A vendor whose resource metadata names its issuer with a trailing slash signs in,
/// renews and keeps the owner's app bound under the name its server metadata gives: the
/// answer's `iss`, the token names and the binding all use that name.
#[test]
fn a_resource_naming_its_issuer_with_a_trailing_slash_signs_in_and_renews_under_the_servers_name() {
    let sign = signing(
        Shape {
            servers: vec!["{base}/"],
            ..shape()
        },
        &["127.0.0.1"],
    );
    let issuer = sign.issuer();
    let pending = begun(&sign, false);
    assert_eq!(finished(&sign, &pending.state, Some(&issuer)), Ok(()));
    let got = fresh_at(&sign, clock());
    assert!(
        matches!(&got, Ok(Credential::OAuth { access, issuer: given, .. })
            if *access == named("token", &issuer) && *given == issuer),
        "{got:?}"
    );
    let again = fresh_at(&sign, clock() + 2 * 3_600_000);
    assert!(
        matches!(&again, Ok(Credential::OAuth { issuer: given, .. }) if *given == issuer),
        "{again:?}"
    );
    assert_eq!(renewals(&sign.vendor), 1);
    let next = start(&sign, false);
    assert!(next.is_ok(), "the owner's app stays bound: {next:?}");
}

/// Every sign-in code a page can show is listed once, sorted, and has a plain sentence
/// with no code words in it.
#[test]
fn every_oauth_code_has_a_plain_sentence() {
    let mut sorted = OAUTH_CODES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, OAUTH_CODES.to_vec(), "sorted, each once");
    for code in OAUTH_CODES {
        let plain = crate::connectors::plain_problem(code);
        assert!(plain.is_some(), "{code} has a sentence");
        assert!(!plain.unwrap().contains('_'), "{code}");
    }
}
