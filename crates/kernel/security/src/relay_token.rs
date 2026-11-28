use crate::error::StoreError;
use crate::secret::HEX_DIGITS;
use kernel_common::relay_names::{TOKEN_HEX, TOKEN_PREFIX, is_relay_token};
use subtle::ConstantTimeEq;

/// The placeholder a harness on an engine key relay sends in place of the provider key:
/// `crazyagent-relay-` and 64 lowercase hex digits. It has no `Debug`, so it never prints.
pub struct RelayToken(String);

impl RelayToken {
    /// Mints `crazyagent-relay-` and 64 lowercase hex digits from the OS random source.
    /// Output: the token, or `model_relay_unavailable` when no randomness is available.
    pub fn mint() -> Result<Self, StoreError> {
        let mut bytes = [0_u8; TOKEN_HEX / 2];
        getrandom::fill(&mut bytes).map_err(|_| StoreError::code("model_relay_unavailable"))?;
        let token = encoded(&bytes);
        assert!(is_relay_token(&token));
        Ok(Self(token))
    }

    /// The token text the harness is handed. Output: the token.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Compares a presented header value in constant time (subtle). Input: the value.
    /// Output: true only for this token.
    pub fn matches(&self, presented: &str) -> bool {
        bool::from(presented.as_bytes().ct_eq(self.0.as_bytes()))
    }
}

/// Writes `crazyagent-relay-` and two lowercase hex digits per random byte into one buffer
/// of exactly the token's length, so minting makes one allocation. Input: the random bytes.
/// Output: the token text.
fn encoded(bytes: &[u8; TOKEN_HEX / 2]) -> String {
    let mut token = String::with_capacity(TOKEN_PREFIX.len() + TOKEN_HEX);
    token.push_str(TOKEN_PREFIX);
    for &byte in bytes {
        token.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        token.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    assert_eq!(token.len(), TOKEN_PREFIX.len() + TOKEN_HEX);
    token
}

#[cfg(test)]
mod tests;
