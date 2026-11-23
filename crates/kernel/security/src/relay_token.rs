use crate::error::StoreError;
use subtle::ConstantTimeEq;

pub const TOKEN_PREFIX: &str = "crazyagent-relay-";
pub const LOCAL_PLACEHOLDER: &str = "crazyagent-local";
pub const TOKEN_HEX: usize = 64;
/// The longest key the engine writes into an account's key file: a relay token.
pub const KEY_BYTES_MAX: usize = TOKEN_PREFIX.len() + TOKEN_HEX;

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

/// Tells whether a model key value is a CrazyAgent placeholder rather than a provider key:
/// the local placeholder for keyless endpoints, or a relay token (see [`is_relay_token`]).
/// Input: the value. Output: true for a placeholder.
pub fn is_placeholder(value: &str) -> bool {
    value == LOCAL_PLACEHOLDER || is_relay_token(value)
}

/// Tells whether a model key value is a relay token `crazyagent-relay-<64 lowercase hex>`,
/// the placeholder a harness on an engine key relay is given in place of the key.
/// Input: the value. Output: true for a relay token.
pub fn is_relay_token(value: &str) -> bool {
    value.strip_prefix(TOKEN_PREFIX).is_some_and(|hex| {
        hex.len() == TOKEN_HEX
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[cfg(test)]
mod tests;
