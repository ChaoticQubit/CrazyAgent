use crate::protocol::CredentialRef;
use kernel_security::error::StoreError;
use std::sync::Arc;

#[cfg(target_os = "linux")]
#[path = "credential_store/linux.rs"]
mod linux;
#[cfg(target_os = "macos")]
#[path = "credential_store/macos.rs"]
mod macos;
#[cfg(all(test, target_os = "macos"))]
#[path = "credential_store/test_keychain.rs"]
pub(crate) mod test_keychain;
#[cfg(target_os = "windows")]
#[path = "credential_store/windows.rs"]
mod windows;
#[cfg(target_os = "macos")]
pub use macos::Keychain;
#[cfg(any(target_os = "linux", target_os = "windows"))]
#[path = "credential_store/os_keyring.rs"]
mod os_keyring;
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub use os_keyring::{GIT_HELPER_COMMAND, OsKeyring, answer_git_credential};

pub use kernel_security::secret::{SECRET_BYTES_MAX, SecretString, wipe};
pub const NAME_BYTES_MAX: usize = 63;

/// Opens this computer's credential store for one engine namespace under
/// `dev.crazyagent.`: the macOS Keychain here. Input: the namespace.
/// Output: the shared store, or `credential_store_unavailable`.
#[cfg(target_os = "macos")]
pub fn platform_store(namespace: &str) -> Result<Arc<dyn Store>, StoreError> {
    Ok(Arc::new(Keychain::new(namespace)?))
}

/// Opens Linux Secret Service for one engine namespace under `dev.crazyagent.`.
/// Input: the namespace. Output: the shared store, or `credential_store_unavailable`.
#[cfg(target_os = "linux")]
pub fn platform_store(namespace: &str) -> Result<Arc<dyn Store>, StoreError> {
    linux::open(namespace)
}

/// Opens Windows Credential Manager for one engine namespace under `dev.crazyagent.`.
/// Input: the namespace. Output: the shared store, or `credential_store_unavailable`.
#[cfg(target_os = "windows")]
pub fn platform_store(namespace: &str) -> Result<Arc<dyn Store>, StoreError> {
    windows::open(namespace)
}

/// Reports that this platform has no credential store seam yet.
/// Output: always `credential_store_unavailable`.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn platform_store(_namespace: &str) -> Result<Arc<dyn Store>, StoreError> {
    Err(StoreError::code("credential_store_unavailable"))
}

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// One part of a text that holds a secret, as [`sized`] writes it.
pub enum Piece<'a> {
    /// Written as it is.
    Plain(&'a str),
    /// Written for a double-quoted curl config value: `\` and `"` take a backslash.
    Escaped(&'a str),
    /// Written as `application/x-www-form-urlencoded` writes one name or value.
    Form(&'a str),
    /// Written as two lowercase hex digits per byte.
    Hex(&'a [u8]),
}

/// Writes the pieces into one buffer of exactly their size, so no growth or temporary
/// frees a copy of a secret; the caller wipes the result after use. Input: the pieces.
/// Output: the bytes, or none when an `Escaped` piece holds a control byte, found before
/// anything is written.
pub fn sized(pieces: &[Piece<'_>]) -> Option<Vec<u8>> {
    let mut size = 0;
    for piece in pieces {
        size += match piece {
            Piece::Plain(text) => text.len(),
            Piece::Escaped(text) => {
                if text.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
                    return None;
                }
                let escapes = text.bytes().filter(|byte| matches!(byte, b'\\' | b'"'));
                text.len() + escapes.count()
            }
            Piece::Form(text) => url::form_urlencoded::byte_serialize(text.as_bytes())
                .map(str::len)
                .sum(),
            Piece::Hex(bytes) => bytes.len() * 2,
        };
    }
    let mut buffer = Vec::with_capacity(size);
    let capacity = buffer.capacity();
    for piece in pieces {
        match piece {
            Piece::Plain(text) => buffer.extend_from_slice(text.as_bytes()),
            Piece::Escaped(text) => {
                for byte in text.bytes() {
                    if matches!(byte, b'\\' | b'"') {
                        buffer.push(b'\\');
                    }
                    buffer.push(byte);
                }
            }
            Piece::Form(text) => {
                for part in url::form_urlencoded::byte_serialize(text.as_bytes()) {
                    buffer.extend_from_slice(part.as_bytes());
                }
            }
            Piece::Hex(bytes) => {
                for byte in *bytes {
                    buffer.push(HEX_DIGITS[usize::from(byte >> 4)]);
                    buffer.push(HEX_DIGITS[usize::from(byte & 0x0f)]);
                }
            }
        }
    }
    assert_eq!(buffer.len(), size);
    assert_eq!(buffer.capacity(), capacity);
    Some(buffer)
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

/// Serializes a value as compact JSON into one buffer of exactly its size, measured by a
/// first pass, so no growth frees a partial copy of a secret the value holds; the caller
/// wipes the result after use. Input: the value. Output: the bytes, or serde_json's error.
#[cfg(unix)]
pub(crate) fn sized_json(value: &impl serde::Serialize) -> serde_json::Result<Vec<u8>> {
    let mut counted = Counted(0);
    serde_json::to_writer(&mut counted, value)?;
    let mut buffer = Vec::with_capacity(counted.0);
    let capacity = buffer.capacity();
    if let Err(problem) = serde_json::to_writer(&mut buffer, value) {
        wipe(&mut buffer);
        return Err(problem);
    }
    assert_eq!(buffer.len(), counted.0);
    assert_eq!(buffer.capacity(), capacity);
    Ok(buffer)
}

/// A writer that only counts the bytes written to it, for [`sized_json`]'s first pass.
#[cfg(unix)]
struct Counted(usize);

#[cfg(unix)]
impl std::io::Write for Counted {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
