use crate::protocol::{CredentialRef, RequestId, SessionId};
use serde::{Deserialize, Serialize};

/// The longest line a harness may write to the engine, its own newline left out. Codex's
/// app server and Claude Code's stream put a whole tool result in one line; Codex keeps at
/// most 1 MiB of a command's output (a 20 MB command gave a 1,049,335-byte `item/completed`
/// line) and JSON spends at most six bytes on one output byte, so 8 MiB holds such a line.
pub const LINE_BYTES_MAX: usize = 8 * 1024 * 1024;

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
