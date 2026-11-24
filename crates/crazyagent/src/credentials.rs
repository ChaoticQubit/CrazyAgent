use kernel_security::error::StoreError;
use kernel_security::store::Store;
use std::sync::Arc;

#[cfg(target_os = "macos")]
use kernel_security::store::macos::Keychain;
#[cfg(all(test, target_os = "macos"))]
#[path = "credential_store/macos_tests.rs"]
mod macos_tests;
#[cfg(all(test, target_os = "macos"))]
#[path = "credential_store/test_keychain.rs"]
pub(crate) mod test_keychain;

/// Opens this computer's credential store for one engine namespace under
/// `dev.crazyagent.`: the macOS Keychain here. Input: the namespace.
/// Output: the shared store, or `credential_store_unavailable`.
#[cfg(target_os = "macos")]
pub fn platform_store(namespace: &str) -> Result<Arc<dyn Store>, StoreError> {
    Ok(Arc::new(keychain(namespace)?))
}

/// Opens the macOS Keychain store for one engine namespace in the default keychain of
/// [`store_home`]: the owner's home, or a test keychain's home in lib tests. The namespace
/// is checked before any home is looked up. Input: the namespace. Output: the store, or
/// `credential_name_invalid` / `credential_namespace_refused` /
/// `credential_store_unavailable`.
#[cfg(target_os = "macos")]
pub fn keychain(namespace: &str) -> Result<Keychain, StoreError> {
    let namespace = kernel_security::store::macos::owned_namespace(namespace)?;
    Keychain::in_home(namespace, &store_home(namespace)?)
}

/// The home whose default keychain holds the store: the owner's `HOME`. Input: the
/// namespace, unused here. Output: the absolute home, or `credential_store_unavailable`.
#[cfg(all(target_os = "macos", not(test)))]
fn store_home(_namespace: &str) -> Result<std::path::PathBuf, StoreError> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or_else(|| StoreError::code("credential_store_unavailable"))
}

/// The home whose default keychain holds the store in lib tests: the temporary home a
/// test keychain serves for the namespace, never the owner's. Input: the namespace.
/// Output: that home; a namespace no test keychain serves is refused with a panic.
#[cfg(all(target_os = "macos", test))]
fn store_home(namespace: &str) -> Result<std::path::PathBuf, StoreError> {
    Ok(test_keychain::home_for(namespace).unwrap_or_else(|| {
        panic!("{namespace} has no temporary test keychain; lib tests never open the owner's")
    }))
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
