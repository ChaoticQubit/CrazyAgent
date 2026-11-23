use std::collections::BTreeMap;
use std::sync::Mutex;

use bus_gateway::{AllOn, Gateway, Grants, Refusals};
use kernel_security::error::StoreError;
use kernel_security::secret::SecretString;
use kernel_security::secrets::{Handles, OwnerEntry, Scopes, Secrets};
use kernel_security::store::{SecretRef, Store, reference};
use sdk::secrets::{self, Provide, Secret};
use sdk::{Capability, Refusal, Tier};
use sdk_host::{Bound, Providers};

use super::*;

/// The engine namespace the rows use.
const NAMESPACE: &str = "dev.crazyagent.e0123456789ab";

/// The in-memory store double: items by reference, or one failure every call answers.
#[derive(Default)]
struct FakeStore {
    items: Mutex<BTreeMap<SecretRef, Vec<u8>>>,
    failing: Option<&'static str>,
}

impl FakeStore {
    /// Answers the failure, if any. Output: nothing, or the failure.
    fn called(&self) -> Result<(), StoreError> {
        self.failing
            .map_or(Ok(()), |code| Err(StoreError::code(code)))
    }
}

impl Store for FakeStore {
    fn put(
        &self,
        service: &str,
        account: &str,
        secret: &SecretString,
    ) -> Result<SecretRef, StoreError> {
        self.called()?;
        let found = reference(service, account)?;
        self.items
            .lock()
            .unwrap()
            .insert(found.clone(), secret.expose().as_bytes().to_vec());
        Ok(found)
    }

    fn get(&self, found: &SecretRef) -> Result<SecretString, StoreError> {
        self.called()?;
        let bytes = self.items.lock().unwrap().get(found).cloned();
        bytes.map_or_else(
            || Err(StoreError::code("credential_missing")),
            SecretString::new,
        )
    }

    fn delete(&self, found: &SecretRef) -> Result<(), StoreError> {
        self.called()?;
        self.items.lock().unwrap().remove(found);
        Ok(())
    }

    fn git_credential_helper(&self, _: &SecretRef, _: &str) -> Result<String, StoreError> {
        unreachable!("the adapter never builds a git helper")
    }
}

/// A refusal sink the rows never reach.
struct Unreached;

impl Refusals for Unreached {
    fn refused(&self, plugin: &'static str, capability: &Capability, refusal: &Refusal) {
        panic!("{plugin} was refused {}: {refusal:?}", capability.name);
    }
}

/// The scopes of the rows: plugin `github` on leaf `github`.
fn scopes() -> Scopes {
    Scopes::new(&[("github", "github")]).expect("the scopes build")
}

#[test]
fn a_store_error_code_reaches_the_plugin_unchanged() {
    let (scopes, handles) = (scopes(), Mutex::new(Handles::new()));
    let failing = FakeStore {
        failing: Some("credential_store_unavailable"),
        ..FakeStore::default()
    };
    let caps = SecretsCaps(Secrets::for_request(
        &failing, NAMESPACE, &scopes, &handles, None,
    ));
    assert_eq!(
        caps.find("github", "token".to_owned())
            .map(|found| found.map(Secret::kernel_id)),
        Err(Refusal::Failed {
            code: "credential_store_unavailable".to_owned()
        })
    );
    let store = FakeStore::default();
    let caps = SecretsCaps(Secrets::for_request(
        &store, NAMESPACE, &scopes, &handles, None,
    ));
    for (name, code) in [
        ("Bad Name", "credential_name_invalid"),
        ("token", "secret_owner_entry_missing"),
    ] {
        assert_eq!(
            caps.put_from_owner("github", name.to_owned())
                .map(Secret::kernel_id),
            Err(Refusal::Rejected {
                code: code.to_owned()
            }),
            "{name}"
        );
    }
    assert_eq!(
        caps.forget("github", Secret::from_kernel(99)),
        Err(Refusal::Rejected {
            code: "secret_handle_refused".to_owned()
        })
    );
    assert_eq!(
        caps.find("nobody", "token".to_owned())
            .map(|found| found.map(Secret::kernel_id)),
        Err(Refusal::Rejected {
            code: "secret_scope_missing".to_owned()
        })
    );
}

#[test]
fn a_handle_crosses_the_adapter_as_the_kernel_id() {
    let (store, scopes, handles) = (FakeStore::default(), scopes(), Mutex::new(Handles::new()));
    let paste = SecretString::new(b"ghp_paste".to_vec()).unwrap();
    let caps = SecretsCaps(Secrets::for_request(
        &store,
        NAMESPACE,
        &scopes,
        &handles,
        Some(OwnerEntry {
            plugin: "github",
            hosts: vec!["api.github.com".to_owned()],
            value: paste,
        }),
    ));
    let put = caps.put_from_owner("github", "token".to_owned()).unwrap();
    let found = caps.find("github", "token".to_owned()).unwrap();
    assert_eq!(found.map(Secret::kernel_id), Some(put.kernel_id()));
    assert_eq!(caps.forget("github", put), Ok(()));
    assert_eq!(
        caps.find("github", "token".to_owned())
            .map(|found| found.map(Secret::kernel_id)),
        Ok(None)
    );
}

#[test]
fn the_quit_gateway_never_opens_the_store() {
    let absent = Refusal::Failed {
        code: "cap_provider_absent".to_owned(),
    };
    assert_eq!(
        Absent
            .put_from_owner("github", "token".to_owned())
            .map(Secret::kernel_id),
        Err(absent.clone())
    );
    assert_eq!(
        Absent
            .find("github", "token".to_owned())
            .map(|found| found.map(Secret::kernel_id)),
        Err(absent.clone())
    );
    assert_eq!(
        Absent.forget("github", Secret::from_kernel(1)),
        Err(absent.clone())
    );
    let grants = Grants::new(&[("github", Tier::T1, &["secrets.find"])]).unwrap();
    let gateway = Gateway::new(&grants, &Unreached, &AllOn);
    let providers = Providers {
        net: &Absent,
        process: &Absent,
        secrets: &Absent,
        settings: &Absent,
        service: &Absent,
        update: &Absent,
    };
    let host = Bound::new(gateway.bind("github"), &providers);
    assert_eq!(
        secrets::find(&host, "token".to_owned()).map(|found| found.is_some()),
        Err(absent)
    );
}

#[test]
fn a_net_refusal_keeps_its_code_and_kind() {
    for code in [
        "net_url_invalid",
        "net_header_name_invalid",
        "net_request_invalid",
        "net_host_refused",
    ] {
        assert_eq!(
            super::refusal(StoreError::code(code)),
            Refusal::Rejected {
                code: code.to_owned()
            }
        );
    }
    for code in [
        "net_capture_not_json",
        "credential_secret_invalid",
        "curl_failed",
    ] {
        assert_eq!(
            super::refusal(StoreError::code(code)),
            Refusal::Failed {
                code: code.to_owned()
            }
        );
    }
    let absent = net::Provide::request(
        &Absent,
        "github",
        net::Request {
            method: net::Method::Get,
            url: "https://api.github.com/user".to_owned(),
            headers: Vec::new(),
            bearer: None,
            form: Vec::new(),
            secret_form: Vec::new(),
            json: None,
            reply_headers: Vec::new(),
            capture: Vec::new(),
        },
    );
    assert!(matches!(
        absent,
        Err(Refusal::Failed { code }) if code == "cap_provider_absent"
    ));
}

/// The source files under `crates/crazyagent/src`, each with whether it is a test file:
/// named `tests.rs`, ending `_tests.rs`, or under a `tests` folder. Output: path, flag and
/// text per file.
fn crate_sources() -> Vec<(std::path::PathBuf, bool, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![root.clone()];
    let mut found = Vec::new();
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let relative = path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let test = name == "tests.rs"
                    || name.ends_with("_tests.rs")
                    || relative.split(['/', '\\']).any(|part| part == "tests");
                found.push((path.clone(), test, std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    assert!(found.len() > 100);
    found
}

#[test]
fn nothing_outside_tests_reaches_secrets_for_request_or_secrets_caps() {
    let sources = crate_sources();
    let count = |pattern: &str, test: bool| -> usize {
        sources
            .iter()
            .filter(|(_, is_test, _)| *is_test == test)
            .map(|(_, _, text)| text.matches(pattern).count())
            .sum()
    };
    for pattern in ["for_request(", "SecretsCaps(", "NetCaps("] {
        let outside: Vec<_> = sources
            .iter()
            .filter(|(_, test, text)| !*test && text.contains(pattern))
            .map(|(path, _, _)| path.display().to_string())
            .collect();
        assert!(outside.is_empty(), "{pattern} outside tests: {outside:?}");
    }
    assert!(count("for_request(", true) > 0);
    assert!(count("SecretsCaps(", true) > 0);
    assert!(count("pub struct NetCaps<'r>(", false) == 1);
}
