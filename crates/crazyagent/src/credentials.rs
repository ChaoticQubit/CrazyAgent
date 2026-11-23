use kernel_security::error::StoreError;
use kernel_security::store::Store;
use std::sync::Arc;

#[cfg(target_os = "macos")]
#[path = "credential_store/macos.rs"]
mod macos;
#[cfg(all(test, target_os = "macos"))]
#[path = "credential_store/test_keychain.rs"]
pub(crate) mod test_keychain;
#[cfg(target_os = "macos")]
pub use macos::Keychain;

/// Opens this computer's credential store for one engine namespace under
/// `dev.crazyagent.`: the macOS Keychain here. Input: the namespace.
/// Output: the shared store, or `credential_store_unavailable`.
#[cfg(target_os = "macos")]
pub fn platform_store(namespace: &str) -> Result<Arc<dyn Store>, StoreError> {
    Ok(Arc::new(Keychain::new(namespace)?))
}

/// Opens Linux Secret Service or Windows Credential Manager for one engine namespace under
/// `dev.crazyagent.`. Input: the namespace. Output: the shared store, or
/// `credential_store_unavailable`.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn platform_store(namespace: &str) -> Result<Arc<dyn Store>, StoreError> {
    kernel_security::store::open_os_keyring(namespace)
}

/// Reports that this platform has no credential store seam yet.
/// Output: always `credential_store_unavailable`.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn platform_store(_namespace: &str) -> Result<Arc<dyn Store>, StoreError> {
    Err(StoreError::code("credential_store_unavailable"))
}
