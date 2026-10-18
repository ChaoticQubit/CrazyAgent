use crate::protocol::{CredentialRef, RequestId, SessionId};
use serde::{Deserialize, Serialize};

pub mod codex;

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
