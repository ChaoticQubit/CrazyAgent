use crate::adapters::NativeAction;
use crate::adapters::codex::{AuthControl, CodexProtocol, Event, ProtocolError, State};
use crate::adapters::codex_events::CodexEventBridge;
use crate::adapters::events::ObservedEvent;
use crate::engine::EngineError;
use crate::protocol::{CredentialRef, RequestId};
use std::path::Path;

#[derive(Default)]
pub struct CodexHostBatch {
    pub events: Vec<ObservedEvent>,
    pub auth: Vec<AuthControl>,
    pub outbound: Vec<Vec<u8>>,
}

pub struct CodexHostProtocol {
    protocol: CodexProtocol,
    bridge: CodexEventBridge,
    request: Option<RequestId>,
}

impl CodexHostProtocol {
    pub fn new(action: NativeAction, account_ref: CredentialRef) -> Result<Self, EngineError> {
        let bridge = CodexEventBridge::new(action, account_ref)
            .map_err(|_| EngineError("codex_host_provenance".into()))?;
        Ok(Self {
            protocol: CodexProtocol::new(),
            bridge,
            request: None,
        })
    }

    pub fn initialize(&mut self) -> Result<Vec<u8>, EngineError> {
        self.protocol.initialize().map_err(protocol_error)
    }

    pub fn start_thread(&mut self, cwd: &Path) -> Result<Vec<u8>, EngineError> {
        self.protocol.start_thread(cwd).map_err(protocol_error)
    }

    /// Starts the master thread with the project's startup instructions as its developer
    /// instructions: read-only, or with `tools` a thread whose tools run without approval
    /// prompts or Codex's own sandbox inside the master's (DEC-093). Input: absolute project
    /// directory, the instruction text and the tools switch. Output: the encoded
    /// `thread/start` request, or a plain protocol code.
    pub fn start_instructed_thread(
        &mut self,
        cwd: &Path,
        instructions: &str,
        tools: bool,
    ) -> Result<Vec<u8>, EngineError> {
        if tools {
            self.protocol
                .start_instructed_tool_thread(cwd, instructions)
        } else {
            self.protocol.start_instructed_thread(cwd, instructions)
        }
        .map_err(protocol_error)
    }

    pub fn start_turn(
        &mut self,
        request_id: RequestId,
        text: &str,
    ) -> Result<Vec<u8>, EngineError> {
        let bytes = self.protocol.start_turn(text).map_err(protocol_error)?;
        assert!(self.request.is_none());
        self.request = Some(request_id);
        assert_eq!(self.protocol.state(), State::TurnStarting);
        Ok(bytes)
    }

    pub fn read_account(&mut self) -> Result<Vec<u8>, EngineError> {
        self.protocol.read_account(false).map_err(protocol_error)
    }

    pub fn interrupt(&mut self) -> Result<Vec<u8>, EngineError> {
        self.protocol.interrupt().map_err(protocol_error)
    }

    pub fn receive(&mut self, bytes: &[u8]) -> Result<CodexHostBatch, EngineError> {
        let events = self.protocol.receive(bytes).map_err(protocol_error)?;
        let mut batch = CodexHostBatch::default();
        for event in events {
            if let Event::AuthControl(auth) = event {
                batch.auth.push(auth);
                continue;
            }
            match &event {
                Event::Initialized => {
                    batch
                        .outbound
                        .push(self.protocol.initialized().map_err(protocol_error)?);
                }
                Event::ApprovalRequired { id, .. } | Event::UnsupportedRequest { id, .. } => {
                    batch
                        .outbound
                        .push(self.protocol.deny_request(id).map_err(protocol_error)?);
                }
                Event::NativeSubAgentStarted { thread_id, turn_id } => {
                    batch.outbound.push(
                        self.protocol
                            .interrupt_child(thread_id, turn_id)
                            .map_err(protocol_error)?,
                    );
                }
                _ => {}
            }
            let ended = matches!(
                event,
                Event::TurnCompleted { .. }
                    | Event::RequestFailed {
                        method: "turn/start",
                        ..
                    }
            );
            let observed = self
                .bridge
                .translate(event, self.request.clone())
                .map_err(|_| EngineError("codex_host_event_translation".into()))?;
            batch.events.push(observed);
            if ended {
                self.request = None;
            }
        }
        Ok(batch)
    }

    pub fn state(&self) -> State {
        self.protocol.state()
    }

    /// Names the frame the Codex protocol last refused, for the master's stream diagnostic.
    /// Input: none. Output: the name, or none before any refusal.
    pub fn refused(&self) -> Option<&str> {
        self.protocol.refused()
    }

    pub fn disconnect(&mut self) -> ObservedEvent {
        let _ = self.protocol.finish();
        self.bridge
            .translate_error(ProtocolError::Disconnected, self.request.take())
    }
}

fn protocol_error(error: ProtocolError) -> EngineError {
    let name = format!("{error:?}");
    let mut code = String::from("codex_host_protocol");
    for character in name.chars() {
        if character.is_ascii_uppercase() {
            code.push('_');
        }
        code.push(character.to_ascii_lowercase());
    }
    EngineError(code)
}

#[cfg(test)]
mod error_code_tests {
    use super::*;

    #[test]
    fn protocol_errors_become_plain_lowercase_codes() {
        assert_eq!(
            protocol_error(ProtocolError::StaleMessage).0,
            "codex_host_protocol_stale_message"
        );
        assert_eq!(
            protocol_error(ProtocolError::InvalidState).0,
            "codex_host_protocol_invalid_state"
        );
        assert_eq!(
            protocol_error(ProtocolError::Disconnected).0,
            "codex_host_protocol_disconnected"
        );
    }
}
