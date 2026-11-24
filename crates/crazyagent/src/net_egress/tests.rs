use std::collections::BTreeSet;
use std::sync::Mutex;

use super::*;
use crazyagent_plugin_check as plugin_check;
use kernel_security::net::{Egress, Field, Method, RawReply, Request, Requests, Sealed};
use kernel_security::secret::SecretString;
use kernel_security::secrets::{Handles, OwnerEntry, Scopes, Secrets};
use kernel_security::store::{SecretRef, Store, reference};
use std::net::IpAddr;

const TOKEN: &str = "ghp_net-egress-fixture-0123456789";

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
    type Sends = BTreeSet<(String, String, Vec<String>, Option<String>)>;
    let sends: Sends = manifests
        .iter()
        .flat_map(|manifest| {
            manifest.net.iter().flat_map(move |net| {
                net.send.iter().map(move |row| {
                    (
                        manifest.plugin.clone(),
                        row.field.clone(),
                        row.hosts.clone(),
                        row.keep_as.clone(),
                    )
                })
            })
        })
        .collect();
    let table: Sends = NET_SENDS
        .iter()
        .map(|(plugin, field, hosts, keep_as)| {
            (
                (*plugin).to_owned(),
                (*field).to_owned(),
                hosts.iter().map(|host| (*host).to_owned()).collect(),
                keep_as.map(str::to_owned),
            )
        })
        .collect();
    assert_eq!(table, sends);
    assert!(bundled_captures().is_ok());
    assert_eq!(
        bundled().allows("github", "https", "github.com", 443),
        Err(StoreError::code("net_host_refused"))
    );
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
    let (_, config) = today
        .command(|_, _| Ok(vec![IpAddr::from([140, 82, 112, 6])]))
        .unwrap();
    let today = String::from_utf8(config).unwrap();
    let store = OneToken::default();
    let scopes = Scopes::new(&[("github", "github")]).unwrap();
    let handles = Mutex::new(Handles::new());
    let secrets = Secrets::for_request(
        &store,
        "dev.crazyagent.e0123456789ab",
        &scopes,
        &handles,
        Some(OwnerEntry {
            plugin: "github",
            hosts: vec!["api.github.com".to_owned()],
            value: SecretString::new(TOKEN.as_bytes().to_vec()).unwrap(),
        }),
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
fn github_requests_go_through_the_kernel_exchange() {
    let mut folders = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/github")];
    let mut read = 0;
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                read += 1;
                let text = std::fs::read_to_string(&path).unwrap();
                assert!(!text.contains("fn exchange("), "{}", path.display());
            }
        }
    }
    assert!(read > 10);
}
