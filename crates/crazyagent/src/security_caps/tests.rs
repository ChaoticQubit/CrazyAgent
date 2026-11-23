use std::collections::BTreeMap;
use std::sync::Mutex;

use bus_gateway::{Gateway, Grants, Refusals};
use kernel_security::error::StoreError;
use kernel_security::secret::SecretString;
use kernel_security::secrets::{Handles, Scopes, Secrets};
use kernel_security::store::{SecretRef, Store, reference};
use sdk::secrets::{self, Provide, Secret};
use sdk::{Capability, Refusal, Tier};

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
        Some(paste),
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
    let gateway = Gateway::new(&grants, &Absent, &Absent, &Unreached);
    assert_eq!(
        secrets::find(&gateway.bind("github"), "token".to_owned()).map(|found| found.is_some()),
        Err(absent)
    );
}
