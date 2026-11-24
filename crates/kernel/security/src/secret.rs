use crate::error::StoreError;
use std::fmt;

pub const SECRET_BYTES_MAX: usize = 1024;
pub const TOKEN_BYTES_MAX: usize = 16_384;

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

    /// Checks the bytes against the rules above with `bytes_max` as the length bound.
    /// Inputs: the raw bytes and the bound. Output: the secret, or the refusal.
    fn within(mut bytes: Vec<u8>, bytes_max: usize) -> Result<Self, StoreError> {
        if bytes.is_empty()
            || bytes.len() > bytes_max
            || bytes.iter().any(|byte| !(0x21..=0x7e).contains(byte))
        {
            wipe(&mut bytes);
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
        wipe(&mut self.0);
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
/// first pass, so no growth frees a partial copy of a secret the value holds; the caller
/// wipes the result after use. Public in the kernel crate only because its callers stay in
/// crazyagent. Input: the value. Output: the bytes, or serde_json's error.
#[cfg(unix)]
pub fn sized_json(value: &impl serde::Serialize) -> serde_json::Result<Vec<u8>> {
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

#[cfg(test)]
mod tests;
