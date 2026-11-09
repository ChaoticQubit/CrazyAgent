use super::capture::Tokens;
use serde_json::Value;

/// The third-party notices of the code and data this engine carries.
pub const THIRD_PARTY_NOTICES: &str = include_str!("../../../../THIRD_PARTY_NOTICES.md");

/// What one transcript message or rollout record reports. Not filled in yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Usage {
    pub key: Option<String>,
    pub exact: bool,
    pub model: String,
    pub tokens: Tokens,
    pub one_hour: u64,
    pub web_searches: u64,
    pub fast: bool,
    pub estimated_chars: u64,
    pub at_ms: Option<i64>,
}

impl Usage {
    /// Not written yet. Input: the later line.
    pub(super) fn merge(&mut self, _later: Usage) {}

    /// Not written yet. Output: the usage unchanged.
    pub(super) fn settled(self) -> Usage {
        self
    }
}

/// Not written yet. Input: the parsed line. Output: none.
pub(super) fn claude_usage(_value: &Value) -> Option<Usage> {
    None
}

/// Not written yet. Input: the parsed line. Output: no records.
pub(super) fn advisor_usages(_value: &Value) -> Vec<Usage> {
    Vec::new()
}

/// Not written yet. Input: the text. Output: none.
pub(super) fn timestamp_ms(_text: &str) -> Option<i64> {
    None
}

#[cfg(test)]
mod tests;
