use super::os_keyring::OsKeyring;
use crate::error::StoreError;
use keyring_core::{CredentialStore, Entry};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

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
