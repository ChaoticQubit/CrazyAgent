use crate::error::StoreError;
use crate::secret::SecretString;

pub const NAME_BYTES_MAX: usize = 63;
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

    /// Reads a reference back from its text. Input: the text. Output: the reference, or
    /// `credential_reference_invalid` for anything [`reference`] would not have made.
    fn try_from(text: &str) -> Result<Self, StoreError> {
        let invalid = || StoreError::code("credential_reference_invalid");
        let (service, account) = text.split_once(':').ok_or_else(invalid)?;
        let found = reference(service, account).map_err(|_| invalid())?;
        assert_eq!(found.as_str(), text);
        Ok(found)
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

/// Checks one service or account name: 1 to 63 bytes of lowercase ASCII letters, digits,
/// `.`, `-` and `_`, not starting with `.` or `-`. Output: the name, or `credential_name_invalid`.
pub fn checked_name(name: &str) -> Result<&str, StoreError> {
    if name.is_empty()
        || name.len() > NAME_BYTES_MAX
        || name.starts_with(['.', '-'])
        || name.contains("..")
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-_".contains(&byte)
        })
    {
        return Err(StoreError::code("credential_name_invalid"));
    }
    Ok(name)
}

/// Names one stored item as `<service>:<account>`, refusing the shapes the engine's wire
/// identity refuses, before any store writes: a trailing `.`, a Windows device name as the
/// stem, and on Windows a one-letter service, which reads as a drive. Inputs: the service
/// and account. Output: the reference, which holds no secret, or `credential_name_invalid`.
pub fn reference(service: &str, account: &str) -> Result<SecretRef, StoreError> {
    let joined = format!("{}:{}", checked_name(service)?, checked_name(account)?);
    let drive = cfg!(windows) && service.len() == 1 && service.as_bytes()[0].is_ascii_lowercase();
    if joined.ends_with(['.', ' ']) || kernel_common::windows_device(&joined) || drive {
        return Err(StoreError::code("credential_name_invalid"));
    }
    assert!(joined.len() <= REFERENCE_BYTES_MAX);
    Ok(SecretRef(joined))
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

#[cfg(test)]
mod tests;
