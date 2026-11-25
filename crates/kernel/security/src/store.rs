use crate::error::StoreError;
use crate::secret::SecretString;

pub const NAME_BYTES_MAX: usize = kernel_common::names::STORE_NAME_BYTES_MAX;
pub const REFERENCE_BYTES_MAX: usize = NAME_BYTES_MAX * 2 + 1;

/// Names one item in a credential store as `<service>:<account>`, both names checked by
/// [`checked_name`] and refused on the shapes in [`reference`]. It never holds the secret.
/// Made only by [`reference`] and by `TryFrom<&str>`, which run the same checks.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecretRef(String);

impl SecretRef {
    /// Lends the reference text. Output: `<service>:<account>`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for SecretRef {
    type Error = StoreError;

    /// Reads a reference back from its text, checking it in place with the same rules as
    /// [`reference`] and keeping the given text. Input: the text. Output: the reference, or
    /// `credential_reference_invalid` for anything [`reference`] would not have made.
    fn try_from(text: &str) -> Result<Self, StoreError> {
        let invalid = || StoreError::code("credential_reference_invalid");
        let (service, account) = text.split_once(':').ok_or_else(invalid)?;
        allowed(service, account, text).map_err(|_| invalid())?;
        Ok(SecretRef(text.to_owned()))
    }
}

/// The operating system's credential store behind one seam: macOS Keychain, Linux Secret
/// Service, Windows Credential Manager. Each store only touches items under the engine's
/// own namespace. References name an item; they never contain the secret.
pub trait Store: Send + Sync {
    /// Saves or replaces one secret. Inputs: a service under the store's namespace, an
    /// account name and the secret. Output: the reference that finds it again.
    fn put(
        &self,
        service: &str,
        account: &str,
        secret: &SecretString,
    ) -> Result<SecretRef, StoreError>;

    /// Reads one secret. Input: a reference from `put`. Output: the secret, or
    /// `credential_missing` when no item exists.
    fn get(&self, reference: &SecretRef) -> Result<SecretString, StoreError>;

    /// Deletes one secret. Input: a reference from `put`. Output: Ok also when it was gone.
    fn delete(&self, reference: &SecretRef) -> Result<(), StoreError>;

    /// Builds a git `credential.helper` value that answers `get` with the secret as the
    /// password for `username`, reading it from the store itself, so the secret never sits
    /// in an argument, an environment variable or a git config file.
    /// Inputs: a reference from `put` and the user name. Output: the helper value.
    fn git_credential_helper(
        &self,
        reference: &SecretRef,
        username: &str,
    ) -> Result<String, StoreError>;
}

/// Checks one service or account name with [`kernel_common::names::is_store_name`]: 1 to
/// 63 bytes of lowercase ASCII letters, digits, `.`, `-` and `_`, not starting with `.` or
/// `-`. Output: the name, or `credential_name_invalid`.
pub fn checked_name(name: &str) -> Result<&str, StoreError> {
    if kernel_common::names::is_store_name(name) {
        Ok(name)
    } else {
        Err(StoreError::code("credential_name_invalid"))
    }
}

/// Names one stored item as `<service>:<account>`, refusing the shapes the engine's wire
/// identity refuses, before any store writes: an account ending in `.`
/// ([`kernel_common::names::is_store_account`]), a Windows device name as the stem of the
/// joined reference, and a one-letter service, which reads as a drive on Windows and is
/// refused on every OS. Inputs: the service and account. Output: the reference, which holds
/// no secret, or `credential_name_invalid`.
pub fn reference(service: &str, account: &str) -> Result<SecretRef, StoreError> {
    checked_name(service)?;
    checked_name(account)?;
    let mut joined = String::with_capacity(service.len() + 1 + account.len());
    joined.push_str(service);
    joined.push(':');
    joined.push_str(account);
    allowed(service, account, &joined)?;
    Ok(SecretRef(joined))
}

/// Runs every rule a reference must pass on its two names and their joined text: both
/// names checked, an account not ending in `.`, no Windows device stem and no one-letter
/// service. Inputs: the service, the account and `<service>:<account>`. Output: Ok, or
/// `credential_name_invalid`.
fn allowed(service: &str, account: &str, joined: &str) -> Result<(), StoreError> {
    let invalid = || StoreError::code("credential_name_invalid");
    if !kernel_common::names::is_store_account(account) {
        return Err(invalid());
    }
    checked_name(service)?;
    checked_name(account)?;
    let drive = service.len() == 1 && service.as_bytes()[0].is_ascii_lowercase();
    if kernel_common::windows_device(joined) || drive {
        return Err(invalid());
    }
    assert_eq!(joined.len(), service.len() + 1 + account.len());
    assert!(!joined.ends_with(['.', ' ']));
    assert!(joined.len() <= REFERENCE_BYTES_MAX);
    Ok(())
}

/// Splits a reference made by `reference` back into service and account.
/// Output: both names, or `credential_reference_invalid` for any other shape.
pub fn parts(reference: &SecretRef) -> Result<(&str, &str), StoreError> {
    let invalid = || StoreError::code("credential_reference_invalid");
    let (service, account) = reference.as_str().split_once(':').ok_or_else(invalid)?;
    if account.contains(':') {
        return Err(invalid());
    }
    Ok((
        checked_name(service).map_err(|_| invalid())?,
        checked_name(account).map_err(|_| invalid())?,
    ))
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub mod os_keyring;
#[cfg(target_os = "windows")]
mod windows;

/// Opens this computer's OS keyring, Linux Secret Service or Windows Credential Manager,
/// for one engine namespace under `dev.crazyagent.`, with a git helper that runs this
/// executable's `git-credential` command. Input: the namespace. Output: the shared store,
/// or `credential_namespace_refused` / `credential_store_unavailable`.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn open_os_keyring(namespace: &str) -> Result<std::sync::Arc<dyn Store>, StoreError> {
    let executable =
        std::env::current_exe().map_err(|_| StoreError::code("credential_store_unavailable"))?;
    Ok(std::sync::Arc::new(os_keyring::OsKeyring::open(
        namespace,
        &executable,
    )?))
}

#[cfg(test)]
mod tests;
