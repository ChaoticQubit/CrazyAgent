use crate::error::StoreError;
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
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let token = format!("{TOKEN_PREFIX}{hex}");
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

#[cfg(test)]
mod tests;
