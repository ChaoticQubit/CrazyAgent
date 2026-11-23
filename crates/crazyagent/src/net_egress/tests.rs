use std::collections::BTreeSet;
use std::sync::Mutex;

use super::*;
use crazyagent_plugin_check as plugin_check;
use kernel_security::net::{Field, Method, Request, Requests};
use kernel_security::secret::SecretString;
use kernel_security::secrets::{Handles, Scopes, Secrets};
use kernel_security::store::{SecretRef, Store, reference};

const TOKEN: &str = "ghp_net-egress-fixture-0123456789";

/// The host table of the allowlist rows.
const HOSTS: &[(&str, &[&str])] = &[
    (
        "github",
        &["github.com", "api.github.com", "ghe.example:8443"],
    ),
    ("local", &["127.0.0.1:8080", "[::1]:9000"]),
];

#[test]
fn a_host_outside_the_allowlist_is_refused() {
    let egress = CurlEgress::with_hosts(HOSTS);
    let refused = Err(StoreError::code("net_host_refused"));
    assert_eq!(egress.allows("github", "https", "github.com", 443), Ok(()));
    assert_eq!(
        egress.allows("github", "https", "ghe.example", 8443),
        Ok(())
    );
    assert_eq!(egress.allows("local", "http", "127.0.0.1", 8080), Ok(()));
    assert_eq!(egress.allows("local", "http", "[::1]", 9000), Ok(()));
    assert_eq!(egress.allows("local", "http", "::1", 9000), refused);
    assert_eq!(
        egress.allows("github", "https", "evil.example", 443),
        refused
    );
    assert_eq!(
        egress.allows("github", "https", "github.com", 8443),
        refused
    );
    assert_eq!(
        egress.allows("github", "https", "ghe.example", 443),
        refused
    );
    assert_eq!(egress.allows("local", "https", "github.com", 443), refused);
    assert_eq!(egress.allows("notes", "https", "github.com", 443), refused);
    assert_eq!(egress.allows("local", "http", "127.0.0.1", 9090), refused);
    assert_eq!(
        CurlEgress::bundled().allows("github", "https", "github.com", 443),
        refused
    );
}

/// Every bundled plugin's `plugin.toml`, read through plugin_check's reader. Output: the
/// manifests.
fn bundled_manifests() -> Vec<kernel_plugin_host::manifest::Manifest> {
    let plugins = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let mut found = Vec::new();
    for tier in std::fs::read_dir(&plugins).unwrap() {
        let tier = tier.unwrap().path();
        if !tier.is_dir() {
            continue;
        }
        for folder in std::fs::read_dir(&tier).unwrap() {
            let folder = folder.unwrap().path();
            if folder.is_dir() {
                found.push(plugin_check::read_manifest(&folder).unwrap());
            }
        }
    }
    assert!(!found.is_empty());
    found
}

/// The (plugin, hosts) rows of some manifests' `[net]` sections. Input: the manifests.
/// Output: the rows.
fn net_hosts(
    manifests: &[kernel_plugin_host::manifest::Manifest],
) -> BTreeSet<(String, Vec<String>)> {
    manifests
        .iter()
        .filter_map(|manifest| {
            manifest
                .net
                .as_ref()
                .map(|net| (manifest.plugin.clone(), net.hosts.clone()))
        })
        .collect()
}

#[test]
fn the_static_net_table_equals_each_bundled_manifest() {
    let manifests = bundled_manifests();
    let hosts = net_hosts(&manifests);
    let with_net = kernel_plugin_host::manifest::parse(
        "plugin = \"probe\"\ntier = \"t2\"\nkind = \"connector\"\nversion = 1\npermissions = [\"net.request\"]\n\n[net]\nhosts = [\"github.com\"]\n",
    )
    .unwrap();
    assert_eq!(
        net_hosts(&[with_net]),
        BTreeSet::from([("probe".to_owned(), vec!["github.com".to_owned()])]),
        "the comparison reads a [net] section when a manifest holds one"
    );
    let table: BTreeSet<(String, Vec<String>)> = NET_HOSTS
        .iter()
        .map(|(plugin, hosts)| {
            (
                (*plugin).to_owned(),
                hosts.iter().map(|host| (*host).to_owned()).collect(),
            )
        })
        .collect();
    assert_eq!(table, hosts);
    let captures: BTreeSet<(String, String, Vec<String>)> = manifests
        .iter()
        .flat_map(|manifest| {
            manifest.net.iter().flat_map(move |net| {
                net.capture.iter().map(move |(host, fields)| {
                    (manifest.plugin.clone(), host.clone(), fields.clone())
                })
            })
        })
        .collect();
    let table: BTreeSet<(String, String, Vec<String>)> = NET_CAPTURES
        .iter()
        .map(|(plugin, host, fields)| {
            (
                (*plugin).to_owned(),
                (*host).to_owned(),
                fields.iter().map(|field| (*field).to_owned()).collect(),
            )
        })
        .collect();
    assert_eq!(table, captures);
    assert!(bundled_captures().is_ok());
    for manifest in &manifests {
        let asks = manifest
            .permissions
            .iter()
            .any(|name| name == "net.request");
        assert_eq!(asks, manifest.net.is_some(), "{}", manifest.plugin);
    }
}

/// A store holding one GitHub token for the byte-equal row.
#[derive(Default)]
struct OneToken {
    items: Mutex<Vec<(SecretRef, String)>>,
}

impl Store for OneToken {
    fn put(
        &self,
        service: &str,
        account: &str,
        secret: &SecretString,
    ) -> Result<SecretRef, StoreError> {
        let found = reference(service, account)?;
        self.items
            .lock()
            .unwrap()
            .push((found.clone(), secret.expose().to_owned()));
        Ok(found)
    }

    fn get(&self, found: &SecretRef) -> Result<SecretString, StoreError> {
        let items = self.items.lock().unwrap();
        let value = items
            .iter()
            .find(|(item, _)| item == found)
            .map(|(_, value)| value.clone());
        value.map_or_else(
            || Err(StoreError::code("credential_missing")),
            |value| SecretString::new(value.into_bytes()),
        )
    }

    fn delete(&self, _: &SecretRef) -> Result<(), StoreError> {
        unreachable!("the row deletes nothing")
    }

    fn git_credential_helper(&self, _: &SecretRef, _: &str) -> Result<String, StoreError> {
        unreachable!("the row builds no git helper")
    }
}

/// Egress that keeps the config it is handed and sends nothing.
#[derive(Default)]
struct Keeping {
    config: Mutex<Vec<u8>>,
}

impl Egress for Keeping {
    fn allows(&self, _: &'static str, _: &str, _: &str, _: u16) -> Result<(), StoreError> {
        Ok(())
    }

    fn resolve(&self, _: &str, _: u16) -> Result<Vec<IpAddr>, StoreError> {
        Ok(vec![IpAddr::from([140, 82, 112, 6])])
    }

    fn send(&self, sealed: Sealed) -> Result<RawReply, StoreError> {
        *self.config.lock().unwrap() = sealed.config().to_vec();
        Ok(RawReply {
            status: 200,
            headers: Vec::new(),
            body: b"{}".to_vec(),
        })
    }
}

#[test]
fn the_curl_config_is_byte_equal_to_todays_github_request() {
    let url = url::Url::parse("https://api.github.com/user/repos").unwrap();
    let token = SecretString::new(TOKEN.as_bytes().to_vec()).unwrap();
    let today = crate::github::http::Request {
        url: url.clone(),
        accept: "application/vnd.github+json",
        token: Some(&token),
        form: None,
        headers: Vec::new(),
        allow_http: false,
    };
    let (_, config) = today.command().unwrap();
    let today = String::from_utf8(config).unwrap();
    let store = OneToken::default();
    let scopes = Scopes::new(&[("github", "github")]).unwrap();
    let handles = Mutex::new(Handles::new());
    let secrets = Secrets::for_request(
        &store,
        "dev.crazyagent.e0123456789ab",
        &scopes,
        &handles,
        Some(("github", SecretString::new(TOKEN.as_bytes().to_vec()).unwrap())),
    );
    let bearer = secrets.put_from_owner("github", "token").unwrap();
    let rules = CaptureRules::new(&[]).unwrap();
    let egress = Keeping::default();
    let requests = Requests::new(&secrets, &egress, &rules);
    let header = |name: &str, value: &str| Field {
        name: name.to_owned(),
        value: value.to_owned(),
    };
    requests
        .request(
            "github",
            Request {
                method: Method::Get,
                url: url.to_string(),
                headers: vec![
                    header("Accept", "application/vnd.github+json"),
                    header("X-GitHub-Api-Version", "2022-11-28"),
                ],
                bearer: Some(bearer),
                form: Vec::new(),
                secret_form: Vec::new(),
                json: None,
                reply_headers: Vec::new(),
                capture: Vec::new(),
            },
        )
        .unwrap();
    let kernel = String::from_utf8(egress.config.lock().unwrap().clone()).unwrap();
    let pinned = "resolve = \"api.github.com:443:140.82.112.6\"";
    assert_eq!(kernel.lines().filter(|line| *line == pinned).count(), 1);
    let mut kernel_lines: Vec<&str> = kernel.lines().filter(|line| *line != pinned).collect();
    let mut today_lines: Vec<&str> = today.lines().collect();
    assert_eq!(kernel_lines[0], today_lines[0]);
    kernel_lines.sort_unstable();
    today_lines.sort_unstable();
    assert_eq!(kernel_lines, today_lines);
}

#[test]
fn a_reply_header_other_than_the_scopes_is_refused_before_curl() {
    let store = OneToken::default();
    let scopes = Scopes::new(&[("github", "github")]).unwrap();
    let handles = Mutex::new(Handles::new());
    let secrets = Secrets::for_request(
        &store,
        "dev.crazyagent.e0123456789ab",
        &scopes,
        &handles,
        None,
    );
    let rules = CaptureRules::new(&[]).unwrap();
    let egress = CurlEgress::with_hosts(&[("github", &["127.0.0.1:9"])]);
    let requests = Requests::new(&secrets, &egress, &rules);
    let refused = requests.request(
        "github",
        Request {
            method: Method::Get,
            url: "http://127.0.0.1:9/v1".to_owned(),
            headers: Vec::new(),
            bearer: None,
            form: Vec::new(),
            secret_form: Vec::new(),
            json: None,
            reply_headers: vec!["X-OAuth-Scopes".to_owned(), "set-cookie".to_owned()],
            capture: Vec::new(),
        },
    );
    assert_eq!(
        refused,
        Err(StoreError::code("net_reply_header_unsupported"))
    );
}
