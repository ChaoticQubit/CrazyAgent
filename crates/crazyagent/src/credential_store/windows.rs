use super::os_keyring::OsKeyring;
use crate::credentials::{Store, StoreError};
use keyring_core::api::CredentialStoreApi;
use keyring_core::{CredentialStore, Entry};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Opens Windows Credential Manager for one engine namespace, with a git helper that runs
/// this executable's `git-credential` command. Input: the namespace. Output: the shared
/// store, or `credential_namespace_refused` / `credential_store_unavailable`.
/// Written for Windows and not yet run there.
pub(super) fn open(namespace: &str) -> Result<Arc<dyn Store>, StoreError> {
    let executable =
        std::env::current_exe().map_err(|_| StoreError::code("credential_store_unavailable"))?;
    Ok(Arc::new(OsKeyring::open(namespace, &executable)?))
}

impl OsKeyring {
    /// Opens Windows Credential Manager. Each item is a generic credential whose target
    /// name is its `<service>:<account>` reference, so every target starts with the
    /// engine's namespace, with Local persistence so it never roams to other machines.
    /// The Win32 calls sit in windows-native-keyring-store; this crate has no unsafe code.
    pub fn open(namespace: &str, executable: &Path) -> Result<Self, StoreError> {
        Self::new(namespace, executable, credential_manager, entry_for, None)
    }
}

fn credential_manager() -> keyring_core::Result<Arc<CredentialStore>> {
    let store: Arc<CredentialStore> = windows_native_keyring_store::Store::new()?;
    Ok(store)
}

fn entry_for(store: &CredentialStore, service: &str, account: &str) -> keyring_core::Result<Entry> {
    let target = format!("{service}:{account}");
    let modifiers = HashMap::from([("target", target.as_str()), ("persistence", "Local")]);
    store.build(service, account, Some(&modifiers))
}
