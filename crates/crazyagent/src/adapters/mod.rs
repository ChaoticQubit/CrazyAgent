use crate::protocol::{CredentialRef, RequestId, SessionId};
use serde::{Deserialize, Serialize};

/// The longest line a harness may write to the engine, its own newline left out. Codex's
/// app server and Claude Code's stream put a whole tool result in one line; Codex keeps at
/// most 1 MiB of a command's output (a 20 MB command gave a 1,049,335-byte `item/completed`
/// line) and JSON spends at most six bytes on one output byte, so 8 MiB holds such a line.
pub const LINE_BYTES_MAX: usize = 8 * 1024 * 1024;

/// The largest text or JSON field one harness event keeps whole: a protocol frame, 64 KiB.
/// Real messages, deltas and tool calls fit it; a larger one, such as a tool call that writes
/// a whole file, is clipped or summarized so that one large event never fails a turn.
pub const FIELD_BYTES_MAX: usize = crate::protocol::FRAME_BYTES_MAX;
const CLIP_NOTE_BYTES_MAX: usize = 64;

/// Clips harness text to at most `FIELD_BYTES_MAX` bytes at a character boundary, ending a
/// clipped text with a line that says how many bytes it left out. Input: the text. Output:
/// the text, whole when it fits.
pub(crate) fn clip_text(text: &str) -> String {
    if text.len() <= FIELD_BYTES_MAX {
        return text.to_owned();
    }
    let kept = text.floor_char_boundary(FIELD_BYTES_MAX - CLIP_NOTE_BYTES_MAX);
    format!(
        "{}\n[{} more bytes not shown]",
        &text[..kept],
        text.len() - kept
    )
}

/// Keeps a harness's JSON field whole up to `FIELD_BYTES_MAX` serialized bytes. A larger one
/// becomes an object of its `kept` members and `omitted_bytes`, its serialized size. Inputs:
/// the value and the members to keep. Output: the value, or its summary.
pub(crate) fn summarize(value: &serde_json::Value, kept: &[&str]) -> serde_json::Value {
    let bytes = serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len());
    if bytes <= FIELD_BYTES_MAX {
        return value.clone();
    }
    let mut summary: serde_json::Map<String, serde_json::Value> = kept
        .iter()
        .filter_map(|name| Some(((*name).to_owned(), value.get(*name)?.clone())))
        .collect();
    summary.insert("omitted_bytes".into(), bytes.into());
    serde_json::Value::Object(summary)
}

pub mod claude;
pub mod codex;
pub mod codex_events;
pub mod events;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeHarness {
    Rcli,
    Claude,
    Codex,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeAction {
    Launch {
        session_id: SessionId,
        harness: NativeHarness,
        profile_ref: CredentialRef,
        worktree_lease: RequestId,
    },
    CancelChild {
        session_id: SessionId,
        native_child_id: SessionId,
    },
}
