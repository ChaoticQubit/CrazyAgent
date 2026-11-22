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

#[cfg(test)]
mod tests;
