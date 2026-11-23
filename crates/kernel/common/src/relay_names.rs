pub const TOKEN_PREFIX: &str = "crazyagent-relay-";
pub const LOCAL_PLACEHOLDER: &str = "crazyagent-local";
pub const TOKEN_HEX: usize = 64;
/// The longest key the engine writes into an account's key file: a relay token.
pub const KEY_BYTES_MAX: usize = TOKEN_PREFIX.len() + TOKEN_HEX;

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
