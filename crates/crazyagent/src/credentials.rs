use crate::protocol::CredentialRef;
use std::fmt;

pub const SECRET_BYTES_MAX: usize = 1024;
pub const NAME_BYTES_MAX: usize = 63;

/// A secret held only in memory: one to 1024 visible ASCII bytes, zeroed on drop and
/// redacted from Debug. Input: the raw bytes. Output: the secret, or `credential_secret_invalid`
/// when empty, too long, or holding spaces, control bytes or non-ASCII bytes.
pub struct SecretString(Vec<u8>);

impl SecretString {
    pub fn new(mut bytes: Vec<u8>) -> Result<Self, StoreError> {
        if bytes.is_empty()
            || bytes.len() > SECRET_BYTES_MAX
            || bytes.iter().any(|byte| !(0x21..=0x7e).contains(byte))
        {
            bytes.fill(0);
            return Err(StoreError::code("credential_secret_invalid"));
        }
        Ok(Self(bytes))
    }

    /// Lends the secret as text for exactly one use, such as one request header.
    /// Output: the visible ASCII the secret was made from.
    pub fn expose(&self) -> &str {
        std::str::from_utf8(&self.0).expect("secret holds visible ASCII only")
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("SecretString([REDACTED])")
    }
}

/// A credential store failure as a stable snake_case code; it never carries secret text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

impl StoreError {
    pub fn code(code: &str) -> Self {
        Self(code.to_owned())
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.0)
    }
}

impl From<StoreError> for crate::engine::EngineError {
    fn from(error: StoreError) -> Self {
        crate::engine::EngineError(error.0)
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
    ) -> Result<CredentialRef, StoreError>;

    /// Reads one secret. Input: a reference from `put`. Output: the secret, or
    /// `credential_missing` when no item exists.
    fn get(&self, reference: &CredentialRef) -> Result<SecretString, StoreError>;

    /// Deletes one secret. Input: a reference from `put`. Output: Ok also when it was gone.
    fn delete(&self, reference: &CredentialRef) -> Result<(), StoreError>;

    /// Builds a git `credential.helper` value that answers `get` with the secret as the
    /// password for `username`, reading it from the store itself, so the secret never sits
    /// in an argument, an environment variable or a git config file.
    /// Inputs: a reference from `put` and the user name. Output: the helper value.
    fn git_credential_helper(
        &self,
        reference: &CredentialRef,
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

/// Names one stored item as `<service>:<account>`. Inputs: checked service and account.
/// Output: the reference, which holds no secret.
pub fn reference(service: &str, account: &str) -> Result<CredentialRef, StoreError> {
    let reference = format!("{}:{}", checked_name(service)?, checked_name(account)?);
    CredentialRef::new(reference).map_err(|_| StoreError::code("credential_name_invalid"))
}

/// Splits a reference made by `reference` back into service and account.
/// Output: both names, or `credential_reference_invalid` for any other shape.
pub fn parts(reference: &CredentialRef) -> Result<(&str, &str), StoreError> {
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
