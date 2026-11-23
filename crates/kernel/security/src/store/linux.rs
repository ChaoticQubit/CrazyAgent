use super::os_keyring::OsKeyring;
use crate::error::StoreError;
use keyring_core::{CredentialStore, Entry};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const STEP_TIME: Duration = Duration::from_secs(10);

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
