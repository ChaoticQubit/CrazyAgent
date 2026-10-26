use super::codex::{ApprovalKind, Event, FRAME_BYTES_MAX, ProtocolError, TurnStatus, WireId};
use super::events::{
    Approval, Capability, EventKind, NativeRequestId, ObservedEvent, Provenance, Support,
    TokenUsage, TurnOutcome,
};
use super::{NativeAction, NativeHarness};
use crate::protocol::{CredentialRef, RequestId};
use std::io::{self, Write};

const OUTPUT_BYTES_MAX: usize = 262_144;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BridgeError {
    Unsupported,
    FrameTooLarge,
    InvalidEvent,
}

/// Projects existing Codex parser events into common harness observations.
/// Local provenance comes from the launch action and supplied account reference.
/// No native execution, credential admission or approval authority is granted.
pub struct CodexEventBridge {
    provenance: Provenance,
    native_thread_id: Option<String>,
}

impl CodexEventBridge {
    pub fn new(action: NativeAction, account_ref: CredentialRef) -> Result<Self, BridgeError> {
        let NativeAction::Launch {
            session_id,
            harness: NativeHarness::Codex,
            profile_ref,
            worktree_lease,
        } = action
        else {
            return Err(BridgeError::Unsupported);
        };
        let bridge = Self {
            provenance: Provenance {
                session_id,
                harness: NativeHarness::Codex,
                account_ref,
                profile_ref,
                worktree_lease,
            },
            native_thread_id: None,
        };
        bridge.assert_consistent();
        Ok(bridge)
    }

    pub fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::PromptInput | Capability::StructuredEvents => Support::ProtocolOnly,
            Capability::NativeAdmission | Capability::CancelChild | Capability::LiveRecovery => {
                Support::Unavailable
            }
        }
    }

    /// Translates one event produced by the existing Codex protocol parser.
    /// The request reference must come from the caller's actual dispatch mapping.
    /// Missing correlation stays unknown and output is checked before state changes.
    pub fn translate(
        &mut self,
        event: Event,
        request_id: Option<RequestId>,
    ) -> Result<ObservedEvent, BridgeError> {
        self.assert_consistent();
        let mut observed = self.observation(request_id, EventKind::ProtocolInitialized);
        observed.kind = match event {
            Event::AuthControl(_) => return Err(BridgeError::Unsupported),
            Event::Initialized => EventKind::ProtocolInitialized,
            Event::AccountRateLimitsRead {
                id,
                account_id,
                ordinary_usage_allowed,
            } => {
                if id <= 0 {
                    return Err(BridgeError::InvalidEvent);
                }
                observed.native_thread_id = None;
                EventKind::AccountRateLimitsRead {
                    id: NativeRequestId::Number(id),
                    account_id,
                    ordinary_usage_allowed,
                }
            }
            Event::ThreadStarted { thread_id } => {
                if thread_id.is_empty()
                    || thread_id.len() > FRAME_BYTES_MAX
                    || self
                        .native_thread_id
                        .as_ref()
                        .is_some_and(|id| id != &thread_id)
                {
                    return Err(BridgeError::InvalidEvent);
                }
                observed.native_thread_id = Some(thread_id);
                EventKind::ThreadStarted
            }
            Event::TurnStarted { turn_id } => {
                observed.native_turn_id = Some(turn_id);
                EventKind::TurnStarted
            }
            Event::TurnCompleted { turn_id, status } => {
                let (outcome, status) = match status {
                    TurnStatus::Completed => (TurnOutcome::Success, "completed"),
                    TurnStatus::Interrupted => (TurnOutcome::Incomplete, "interrupted"),
                    TurnStatus::Failed => (TurnOutcome::Failed, "failed"),
                    TurnStatus::InProgress => return Err(BridgeError::InvalidEvent),
                };
                observed.native_turn_id = Some(turn_id);
                EventKind::TurnFinished {
                    outcome,
                    subtype: status.into(),
                    result: None,
                    errors: Vec::new(),
                    terminal_reason: Some(status.into()),
                    usage: TokenUsage::default(),
                    estimated_cost_usd: None,
                }
            }
            Event::InterruptAcknowledged { turn_id } => {
                observed.native_turn_id = Some(turn_id);
                EventKind::InterruptAcknowledged
            }
            Event::RequestFailed {
                id,
                method,
                code,
                message,
            } => {
                if auth_control_method(method) {
                    return Err(BridgeError::Unsupported);
                }
                if method == "account/rateLimits/read" {
                    observed.native_thread_id = None;
                }
                EventKind::RequestFailed {
                    id: NativeRequestId::Number(id),
                    method: method.into(),
                    code,
                    message,
                }
            }
            Event::AgentMessageDelta {
                turn_id,
                item_id,
                delta,
            } => {
                observed.native_turn_id = Some(turn_id);
                EventKind::AssistantDelta {
                    item_id,
                    text: delta,
                }
            }
            Event::TurnError {
                turn_id,
                message,
                will_retry,
            } => {
                observed.native_turn_id = Some(turn_id);
                EventKind::TurnError {
                    message,
                    will_retry,
                }
            }
            Event::ApprovalRequired { id, kind, params } => {
                observed.native_turn_id = params
                    .get("turnId")
                    .and_then(|id| id.as_str())
                    .map(str::to_owned);
                EventKind::ApprovalRequired {
                    id: native_request_id(id),
                    approval: match kind {
                        ApprovalKind::Command => Approval::Command,
                        ApprovalKind::FileChange => Approval::FileChange,
                        ApprovalKind::Permissions => Approval::Permissions,
                    },
                    params,
                }
            }
            Event::UnsupportedRequest { id, method } => {
                if auth_control_method(&method) {
                    return Err(BridgeError::Unsupported);
                }
                EventKind::UnsupportedRequest {
                    id: native_request_id(id),
                    method,
                }
            }
            Event::UnsupportedNotification { method } => {
                if auth_control_method(&method) {
                    return Err(BridgeError::Unsupported);
                }
                EventKind::Unsupported {
                    message_type: "notification".into(),
                    subtype: Some(method),
                }
            }
            Event::NativeSubAgentStarted { .. } => EventKind::Unsupported {
                message_type: "notification".into(),
                subtype: Some("native_sub_agent".into()),
            },
        };
        serde_json::to_writer(
            &mut OutputLimit {
                remaining: OUTPUT_BYTES_MAX,
            },
            &observed,
        )
        .map_err(|_| BridgeError::FrameTooLarge)?;
        if observed.native_thread_id.is_some() {
            self.native_thread_id = observed.native_thread_id.clone();
        }
        self.assert_consistent();
        Ok(observed)
    }

    pub fn translate_error(
        &self,
        error: ProtocolError,
        request_id: Option<RequestId>,
    ) -> ObservedEvent {
        self.assert_consistent();
        let kind = match error {
            ProtocolError::Disconnected => EventKind::Disconnected,
            error => EventKind::ProtocolFailure {
                code: format!("{error:?}"),
            },
        };
        self.observation(request_id, kind)
    }

    fn observation(&self, request_id: Option<RequestId>, kind: EventKind) -> ObservedEvent {
        ObservedEvent {
            provenance: self.provenance.clone(),
            request_id,
            native_session_id: None,
            native_thread_id: self.native_thread_id.clone(),
            native_turn_id: None,
            native_event_id: None,
            native_user_message_uuid: None,
            kind,
        }
    }

    fn assert_consistent(&self) {
        assert_eq!(self.provenance.harness, NativeHarness::Codex);
        assert!(
            self.native_thread_id
                .as_ref()
                .is_none_or(|id| !id.is_empty() && id.len() <= FRAME_BYTES_MAX)
        );
    }
}

fn auth_control_method(method: &str) -> bool {
    matches!(
        method,
        "account/login/start"
            | "account/login/completed"
            | "account/read"
            | "account/login/cancel"
            | "account/logout"
    )
}

fn native_request_id(id: WireId) -> NativeRequestId {
    match id {
        WireId::Number(id) => NativeRequestId::Number(id),
        WireId::Text(id) => NativeRequestId::Text(id),
    }
}

struct OutputLimit {
    remaining: usize,
}

impl Write for OutputLimit {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        assert!(self.remaining <= OUTPUT_BYTES_MAX);
        if bytes.len() > self.remaining {
            return Err(io::Error::other("shared event output limit"));
        }
        self.remaining -= bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SessionId;

    #[test]
    #[should_panic]
    fn rejects_corrupted_harness_provenance() {
        let bridge = CodexEventBridge {
            provenance: Provenance {
                session_id: SessionId::new("session").unwrap(),
                harness: NativeHarness::Claude,
                account_ref: CredentialRef::new("account").unwrap(),
                profile_ref: CredentialRef::new("profile").unwrap(),
                worktree_lease: RequestId::new("lease").unwrap(),
            },
            native_thread_id: None,
        };
        bridge.assert_consistent();
    }

    #[test]
    fn output_limit_accepts_boundary_and_rejects_the_next_byte() {
        let mut output = OutputLimit { remaining: 1 };
        assert_eq!(output.write(b"x").unwrap(), 1);
        assert!(output.write(b"y").is_err());
        assert_eq!(output.remaining, 0);
    }

    #[test]
    #[should_panic]
    fn rejects_corrupted_output_accounting() {
        let mut output = OutputLimit {
            remaining: OUTPUT_BYTES_MAX + 1,
        };
        let _ = output.write(b"x");
    }

    #[test]
    #[should_panic]
    fn rejects_corrupted_thread_identity_bound() {
        let bridge = CodexEventBridge {
            provenance: Provenance {
                session_id: SessionId::new("session").unwrap(),
                harness: NativeHarness::Codex,
                account_ref: CredentialRef::new("account").unwrap(),
                profile_ref: CredentialRef::new("profile").unwrap(),
                worktree_lease: RequestId::new("lease").unwrap(),
            },
            native_thread_id: Some("x".repeat(FRAME_BYTES_MAX + 1)),
        };
        bridge.assert_consistent();
    }

    #[test]
    fn nonterminal_status_cannot_be_translated_into_completion() {
        let action = NativeAction::Launch {
            session_id: SessionId::new("session").unwrap(),
            harness: NativeHarness::Codex,
            profile_ref: CredentialRef::new("profile").unwrap(),
            worktree_lease: RequestId::new("lease").unwrap(),
        };
        let mut bridge =
            CodexEventBridge::new(action, CredentialRef::new("account").unwrap()).unwrap();
        assert_eq!(
            bridge.translate(
                Event::TurnCompleted {
                    turn_id: "turn".into(),
                    status: TurnStatus::InProgress,
                },
                None,
            ),
            Err(BridgeError::InvalidEvent)
        );
    }
}
