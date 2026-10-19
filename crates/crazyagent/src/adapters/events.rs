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
    ProtocolInitialized,
    ThreadStarted,
    TurnStarted,
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
    AssistantDelta {
        item_id: String,
        text: String,
    },
    InterruptAcknowledged,
    RequestFailed {
        id: NativeRequestId,
        method: String,
        code: i64,
        message: String,
    },
    TurnError {
        message: String,
        will_retry: bool,
    },
    ApprovalRequired {
        id: NativeRequestId,
        approval: Approval,
        params: Value,
    },
    UnsupportedRequest {
        id: NativeRequestId,
        method: String,
    },
    Disconnected,
    ProtocolFailure {
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum NativeRequestId {
    Number(i64),
    Text(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    Command,
    FileChange,
    Permissions,
}

/// Carries observed harness facts with caller-bound local provenance.
/// Missing native identifiers remain unknown rather than aliasing local IDs.
/// These records grant no execution, authentication or approval authority.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ObservedEvent {
    pub provenance: Provenance,
    pub request_id: Option<RequestId>,
    pub native_session_id: Option<String>,
    pub native_thread_id: Option<String>,
    pub native_turn_id: Option<String>,
    pub native_event_id: Option<String>,
    pub native_user_message_uuid: Option<String>,
    pub kind: EventKind,
}

impl From<HarnessEvent> for ObservedEvent {
    fn from(event: HarnessEvent) -> Self {
        Self {
            provenance: event.provenance,
            request_id: event.request_id,
            native_session_id: Some(event.native_session_id),
            native_thread_id: None,
            native_turn_id: None,
            native_event_id: Some(event.native_event_id),
            native_user_message_uuid: event.native_user_message_uuid,
            kind: event.kind,
        }
    }
}
