use serde::{Deserialize, Serialize};

/// The environment variable that carries a launch's [`LineageToken`] to every process it
/// starts.
pub const LINEAGE_VARIABLE: &str = "CRAZYAGENT_LINEAGE";
const TOKEN_BYTES: usize = 16;

/// A random value every process of one launch inherits in [`LINEAGE_VARIABLE`], so the
/// launch's stop finds a process that left its session and lost its parent while it kept
/// its environment (SECAUDIT-SETSID-LEFTOVER M1). Not a secret: any process of the owner's
/// user can read it, so it only lets a same-user process get itself named or, under End them
/// (DEC-147.4), ended; an agent only ever learns its own launch's token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LineageToken(String);

impl LineageToken {
    /// Draws a new token. Output: the token, or none when the system gives no randomness.
    pub fn new() -> Option<Self> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).ok()?;
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert!(Self::valid(&token));
        Some(Self(token))
    }

    /// Tells whether `text` has a token's shape: `TOKEN_BYTES` bytes as lowercase hex.
    /// Input: the text. Output: true only then.
    pub fn valid(text: &str) -> bool {
        text.len() == TOKEN_BYTES * 2
            && text
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }

    /// The token as its variable's value. Output: the hex text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LineageToken {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if Self::valid(&text) {
            Ok(Self(text))
        } else {
            Err("lineage_token_invalid")
        }
    }
}

impl From<LineageToken> for String {
    fn from(token: LineageToken) -> Self {
        token.0
    }
}

pub const TOKEN_PREFIX: &str = "crazyagent-relay-";
pub const LOCAL_PLACEHOLDER: &str = "crazyagent-local";
pub const TOKEN_HEX: usize = 64;

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
