use crate::error::StoreError;
use std::fmt;

pub const SECRET_BYTES_MAX: usize = 1024;
pub const TOKEN_BYTES_MAX: usize = 16_384;
/// The most bytes of one stored item: a secret with the header of host keys stored ahead
/// of it.
pub const STORED_BYTES_MAX: usize = 1_536;
/// The most bytes Windows Credential Manager keeps in one credential blob.
pub const CREDENTIAL_BLOB_BYTES_MAX: usize = 2_560;

const _: () = assert!(SECRET_BYTES_MAX < STORED_BYTES_MAX);
const _: () = assert!(STORED_BYTES_MAX < CREDENTIAL_BLOB_BYTES_MAX);

/// A secret held only in memory: one to 1024 visible ASCII bytes, or up to 16 KiB for a
/// provider's bearer token, zeroed on drop and redacted from Debug. Input: the raw bytes.
/// Output: the secret, or `credential_secret_invalid` when empty, too long, or holding
/// spaces, control bytes or non-ASCII bytes.
pub struct SecretString(Vec<u8>);

impl SecretString {
    pub fn new(bytes: Vec<u8>) -> Result<Self, StoreError> {
        Self::within(bytes, SECRET_BYTES_MAX)
    }

    /// Holds a provider's bearer token that is sent and never stored, such as a ChatGPT
    /// access token of about 2 KB. Input: the raw bytes. Output: the secret of up to
    /// 16 KiB, or `credential_secret_invalid`.
    pub fn bearer(bytes: Vec<u8>) -> Result<Self, StoreError> {
        Self::within(bytes, TOKEN_BYTES_MAX)
    }

    /// Holds one stored item as the store reads or writes it, such as a secret with its
    /// header of host keys. A space may open the item, as the mark of that header, and
    /// nowhere else. Input: the raw bytes. Output: the item of up to `STORED_BYTES_MAX`
    /// bytes, or `credential_secret_invalid`.
    pub fn stored(bytes: Vec<u8>) -> Result<Self, StoreError> {
        let start = usize::from(bytes.first() == Some(&b' '));
        Self::checked(bytes, start, STORED_BYTES_MAX)
    }

    /// Checks the bytes against the rules above with `bytes_max` as the length bound.
    /// Inputs: the raw bytes and the bound. Output: the secret, or the refusal.
    fn within(bytes: Vec<u8>, bytes_max: usize) -> Result<Self, StoreError> {
        Self::checked(bytes, 0, bytes_max)
    }

    /// Checks the bytes from `start` on for visible ASCII, at least one of them, and the
    /// whole length against `bytes_max`. Inputs: the raw bytes, the first byte the rule
    /// covers and the bound. Output: the secret, or `credential_secret_invalid`.
    fn checked(mut bytes: Vec<u8>, start: usize, bytes_max: usize) -> Result<Self, StoreError> {
        assert!(start <= 1);
        if bytes.len() <= start
            || bytes.len() > bytes_max
            || bytes[start..]
                .iter()
                .any(|byte| !(0x21..=0x7e).contains(byte))
        {
            wipe_owned(&mut bytes);
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
        wipe_owned(&mut self.0);
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("SecretString([REDACTED])")
    }
}

/// Overwrites a secret buffer with zeros through `zeroize`, whose volatile writes the
/// compiler may not drop as dead stores before the buffer is freed. Every secret buffer is
/// wiped here. Input: the buffer, of bytes or of wider units. Output: none; its length stays.
pub fn wipe<Unit: zeroize::DefaultIsZeroes>(buffer: &mut [Unit]) {
    zeroize::Zeroize::zeroize(buffer);
}

/// Overwrites an owned secret buffer with zeros over its whole capacity, so bytes a caller
/// cut off past its length are wiped too, and empties it while keeping its allocation.
/// Input: the buffer. Output: none; its length is zero.
pub fn wipe_owned(buffer: &mut Vec<u8>) {
    zeroize::Zeroize::zeroize(buffer);
    assert!(buffer.is_empty());
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
            Piece::Form(text) => form_urlencoded::byte_serialize(text.as_bytes())
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
                for part in form_urlencoded::byte_serialize(text.as_bytes()) {
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

/// Serializes a value as compact JSON into one buffer of exactly its size, measured by a
/// first pass and written by the second through a fixed slice that cannot grow, so no
/// growth frees a partial copy of a secret the value holds; the caller wipes the result.
/// Public in the kernel crate only because its callers stay in crazyagent. Input: the
/// value. Output: the bytes, or serde_json's error when the value changed between passes.
#[cfg(unix)]
pub fn sized_json(value: &impl serde::Serialize) -> serde_json::Result<Vec<u8>> {
    let mut counted = Counted(0);
    serde_json::to_writer(&mut counted, value)?;
    let mut buffer = vec![0_u8; counted.0];
    let capacity = buffer.capacity();
    let mut room: &mut [u8] = &mut buffer;
    let written = serde_json::to_writer(&mut room, value);
    let left = room.len();
    if let Err(problem) = written {
        wipe_owned(&mut buffer);
        return Err(problem);
    }
    if left != 0 {
        wipe_owned(&mut buffer);
        let changed = std::io::Error::other("the value changed between the two passes");
        return Err(serde_json::Error::io(changed));
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

#[cfg(test)]
mod tests;
