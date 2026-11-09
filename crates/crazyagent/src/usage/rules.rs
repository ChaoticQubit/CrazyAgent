use super::capture::{LINE_TOKENS_MAX, Tokens};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The third-party notices of the code and data this engine carries.
pub const THIRD_PARTY_NOTICES: &str = include_str!("../../../../THIRD_PARTY_NOTICES.md");

const KEY_BYTES_MAX: usize = 128;
const MODEL_BYTES_MAX: usize = 128;
const CHARS_PER_TOKEN: u64 = 4;
const ADVISORS_PER_LINE_MAX: usize = 16;

/// What one transcript message or rollout record reports. A record with a key counts once
/// across files; `exact` says a repeat counts nothing rather than only its growth. Rules
/// ported from CodeBurn (MIT, THIRD_PARTY_NOTICES.md).
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
    /// Folds a later line of the same message in: the largest counts, the sum of content
    /// characters, the earliest time and the last model named. Input: the later line.
    pub(super) fn merge(&mut self, later: Usage) {
        assert_eq!(self.key, later.key, "only lines of one message merge");
        self.tokens = self.tokens.largest(later.tokens);
        self.one_hour = self.one_hour.max(later.one_hour);
        self.web_searches = self.web_searches.max(later.web_searches);
        self.fast |= later.fast;
        self.estimated_chars = self.estimated_chars.saturating_add(later.estimated_chars);
        self.at_ms = match (self.at_ms, later.at_ms) {
            (Some(one), Some(other)) => Some(one.min(other)),
            (one, other) => one.or(other),
        };
        if !later.model.is_empty() {
            self.model = later.model;
        }
    }

    /// Applies the output estimate of a placeholder message, which never lowers the reported
    /// output. Output: the usage to count.
    pub(super) fn settled(mut self) -> Usage {
        self.tokens.output = self
            .tokens
            .output
            .max(self.estimated_chars.div_ceil(CHARS_PER_TOKEN))
            .min(LINE_TOKENS_MAX);
        self
    }
}

/// A model name safe to store and show: printable ASCII within the bound, else empty.
/// Input: the name a transcript gave. Output: the name to keep.
pub(super) fn model_name(value: &Value) -> String {
    value
        .as_str()
        .filter(|name| {
            name.len() <= MODEL_BYTES_MAX && name.bytes().all(|byte| byte.is_ascii_graphic())
        })
        .unwrap_or("")
        .to_owned()
}

/// Whether a Claude Code line is a sub-agent row whose usage kept only the first streamed
/// chunk, as builds before 2.1.307 wrote them. Input: the parsed line. Output: the answer.
fn placeholder(value: &Value) -> bool {
    let stopped = value["message"]["stop_reason"]
        .as_str()
        .is_some_and(|stop| !stop.is_empty());
    if value["isSidechain"] != true || stopped {
        return false;
    }
    let Some(version) = value["version"].as_str() else {
        return false;
    };
    let mut parts = version.split('.').map(|part| part.parse::<u64>().ok());
    let (Some(Some(major)), Some(Some(minor)), Some(Some(patch))) =
        (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if major != 2 {
        return major < 2;
    }
    if minor != 1 {
        return minor < 1;
    }
    patch < 307
}

/// Counts the output characters of a message's content: text, and each tool call's input.
/// Input: the content blocks. Output: the characters.
fn content_chars(content: &Value) -> u64 {
    let Some(blocks) = content.as_array() else {
        return 0;
    };
    blocks.iter().fold(0_u64, |chars, block| {
        let more = match block["type"].as_str() {
            Some("text") => block["text"]
                .as_str()
                .map_or(0, |text| text.chars().count()),
            Some("tool_use" | "server_tool_use") => {
                let input = if block["input"].is_null() {
                    serde_json::json!({})
                } else {
                    block["input"].clone()
                };
                input.to_string().chars().count()
            }
            _ => 0,
        };
        chars.saturating_add(more as u64)
    })
}

/// Reads one usage object's counts. Input: the object. Output: the tokens, the part of the
/// cache write held one hour and the web searches, each capped.
fn reported(usage: &Value) -> (Tokens, u64, u64) {
    let count = |key: &str| usage[key].as_u64().unwrap_or(0);
    let one_hour = usage["cache_creation"]["ephemeral_1h_input_tokens"]
        .as_u64()
        .unwrap_or(0);
    let tokens = Tokens {
        input: count("input_tokens"),
        output: count("output_tokens"),
        cache_read: count("cache_read_input_tokens"),
        cache_write: count("cache_creation_input_tokens").max(one_hour),
    }
    .at_most(LINE_TOKENS_MAX);
    let web_searches = usage["server_tool_use"]["web_search_requests"]
        .as_u64()
        .unwrap_or(0);
    (
        tokens,
        one_hour.min(LINE_TOKENS_MAX),
        web_searches.min(LINE_TOKENS_MAX),
    )
}

/// The key of a Claude message. Input: the message. Output: the key, None for an id outside
/// the bound.
fn message_key(message: &Value) -> Option<String> {
    message["id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= KEY_BYTES_MAX)
        .map(|id| format!("claude:{id}"))
}

/// Reads the advisor calls a Claude Code line reports. The line's own usage leaves them out,
/// so each counts as its own record under the advisor's model. Rules ported from CodeBurn
/// (MIT, THIRD_PARTY_NOTICES.md). Input: the parsed line. Output: at most
/// `ADVISORS_PER_LINE_MAX` records.
pub(super) fn advisor_usages(value: &Value) -> Vec<Usage> {
    let message = &value["message"];
    let usage = &message["usage"];
    let Some(iterations) = usage["iterations"]
        .as_array()
        .filter(|_| value["type"] == "assistant")
    else {
        return Vec::new();
    };
    let base = message_key(message);
    iterations
        .iter()
        .filter(|iteration| iteration["type"] == "advisor_message")
        .filter_map(|iteration| {
            let named = model_name(&iteration["model"]);
            let model = if named.is_empty() {
                model_name(&message["model"])
            } else {
                named
            };
            (!model.is_empty()).then_some((iteration, model))
        })
        .take(ADVISORS_PER_LINE_MAX)
        .enumerate()
        .map(|(index, (iteration, model))| {
            let (tokens, one_hour, web_searches) = reported(iteration);
            Usage {
                key: base.as_ref().map(|key| format!("{key}:advisor:{index}")),
                exact: false,
                model,
                tokens,
                one_hour,
                web_searches,
                fast: if iteration["speed"].is_string() {
                    iteration["speed"] == "fast"
                } else {
                    usage["speed"] == "fast"
                },
                estimated_chars: 0,
                at_ms: value["timestamp"].as_str().and_then(timestamp_ms),
            }
        })
        .collect()
}

/// Reads one Claude Code transcript line. Input: the parsed line. Output: its usage, None
/// for a line that reports none. Message ids outside the bound count as having no id.
pub(super) fn claude_usage(value: &Value) -> Option<Usage> {
    let message = &value["message"];
    let usage = &message["usage"];
    if value["type"] != "assistant" || !usage.is_object() {
        return None;
    }
    let (tokens, one_hour, web_searches) = reported(usage);
    Some(Usage {
        key: message_key(message),
        exact: false,
        model: model_name(&message["model"]),
        tokens,
        one_hour,
        web_searches,
        fast: usage["speed"] == "fast",
        estimated_chars: if placeholder(value) {
            content_chars(&message["content"])
        } else {
            0
        },
        at_ms: value["timestamp"].as_str().and_then(timestamp_ms),
    })
}

/// Reads a transcript's UTC timestamp, `YYYY-MM-DDTHH:MM:SS[.fraction]Z`. Input: the text.
/// Output: milliseconds since the epoch, None for anything else.
pub(super) fn timestamp_ms(text: &str) -> Option<i64> {
    let number = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let shape = text.as_bytes();
    if shape.len() < 20
        || shape[4] != b'-'
        || shape[7] != b'-'
        || shape[10] != b'T'
        || shape[13] != b':'
        || shape[16] != b':'
        || shape.last() != Some(&b'Z')
    {
        return None;
    }
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let fraction = match shape[19] {
        b'Z' => 0,
        b'.' => {
            let digits = text.get(20..text.len() - 1)?;
            let first = digits.get(..digits.len().min(3))?;
            first.parse::<i64>().ok()? * 10_i64.pow(3 - first.len() as u32)
        }
        _ => return None,
    };
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let of_era = shifted.rem_euclid(400);
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let days = era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719_468;
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000 + fraction)
}

/// What a rollout's earlier lines settled. Not filled in yet.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexState {}

#[cfg(test)]
mod tests;
