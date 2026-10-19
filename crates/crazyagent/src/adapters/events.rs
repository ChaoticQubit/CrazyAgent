use super::NativeHarness;
use crate::protocol::{CredentialRef, RequestId, SessionId};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Provenance {
    pub session_id: SessionId,
    pub harness: NativeHarness,
    pub account_ref: CredentialRef,
    pub profile_ref: CredentialRef,
    pub worktree_lease: RequestId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Capability {
    PromptInput,
    StructuredEvents,
    NativeAdmission,
    CancelChild,
    LiveRecovery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Support {
    ProtocolOnly,
    Unavailable,
}

/// Reports observed token counters for the main agent loop of one turn.
/// Missing native counters remain unknown and observed zero stays explicit.
/// Subagent and cumulative model totals are outside this measurement scope.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Success,
    Failed,
    Incomplete,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    Initialized {
        model: String,
        capabilities: Vec<String>,
    },
    AssistantText {
        message_id: String,
        parent_tool_use_id: Option<String>,
        text: String,
    },
    ToolUse {
        message_id: String,
        parent_tool_use_id: Option<String>,
        tool_use_id: String,
        name: String,
        input: Value,
    },
    AssistantError {
        code: String,
    },
    TurnFinished {
        outcome: TurnOutcome,
        subtype: String,
        result: Option<String>,
        errors: Vec<String>,
        terminal_reason: Option<String>,
        usage: TokenUsage,
        estimated_cost_usd: Option<f64>,
    },
    Unsupported {
        message_type: String,
        subtype: Option<String>,
    },
}

/// Carries a normalized observation with caller-bound local provenance.
/// Native session identity is observed from the harness initialization frame.
/// These records grant no execution, authentication or recovery authority.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HarnessEvent {
    pub provenance: Provenance,
    pub request_id: Option<RequestId>,
    pub native_session_id: String,
    pub native_event_id: String,
    pub native_user_message_uuid: Option<String>,
    pub kind: EventKind,
}
