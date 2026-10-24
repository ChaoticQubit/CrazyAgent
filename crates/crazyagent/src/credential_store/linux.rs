use super::os_keyring::OsKeyring;
use crate::credentials::{Store, StoreError};
use keyring_core::{CredentialStore, Entry};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const STEP_TIME: Duration = Duration::from_secs(10);

/// Opens Linux Secret Service for one engine namespace, with a git helper that runs this
/// executable's `git-credential` command. Input: the namespace. Output: the shared store,
/// or `credential_namespace_refused` / `credential_store_unavailable`.
/// Written for Linux and not yet run there.
pub(super) fn open(namespace: &str) -> Result<Arc<dyn Store>, StoreError> {
    let executable =
        std::env::current_exe().map_err(|_| StoreError::code("credential_store_unavailable"))?;
    Ok(Arc::new(OsKeyring::open(namespace, &executable)?))
}

impl OsKeyring {
    /// Opens the Linux Secret Service (gnome-keyring, KWallet) over D-Bus in pure Rust.
    /// Items go into the default collection, found by their `service` and `username`
    /// attributes. Every call waits at most 10 s, so an unanswered unlock prompt never
    /// holds the engine. Inputs: the namespace and the executable git runs as the helper.
    pub fn open(namespace: &str, executable: &Path) -> Result<Self, StoreError> {
        Self::new(
            namespace,
            executable,
            secret_service,
            entry_for,
            Some(STEP_TIME),
        )
    }
}

fn secret_service() -> keyring_core::Result<Arc<CredentialStore>> {
    let store: Arc<CredentialStore> = zbus_secret_service_keyring_store::Store::new()?;
    Ok(store)
}

fn entry_for(store: &CredentialStore, service: &str, account: &str) -> keyring_core::Result<Entry> {
    store.build(service, account, None)
}
