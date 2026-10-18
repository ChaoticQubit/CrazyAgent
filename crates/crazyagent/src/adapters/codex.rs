use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

pub const FRAME_BYTES_MAX: usize = 65_536;
const EVENTS_MAX: usize = 128;
const APPROVALS_MAX: usize = 16;
const SERVER_IDS_MAX: usize = 1024;
const TURNS_MAX: usize = 128;
const ID_BYTES_MAX: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    New,
    Initializing,
    Initialized,
    Ready,
    ThreadStarting,
    Idle,
    TurnStarting,
    Active,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    InvalidState,
    InvalidInput,
    InvalidMessage,
    FrameTooLarge,
    Capacity,
    StaleMessage,
    Disconnected,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(untagged)]
pub enum WireId {
    Number(i64),
    Text(String),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    InProgress,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalKind {
    Command,
    FileChange,
    Permissions,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Initialized,
    ThreadStarted {
        thread_id: String,
    },
    TurnStarted {
        turn_id: String,
    },
    TurnCompleted {
        turn_id: String,
        status: TurnStatus,
    },
    InterruptAcknowledged {
        turn_id: String,
    },
    RequestFailed {
        id: i64,
        method: &'static str,
        code: i64,
        message: String,
    },
    AgentMessageDelta {
        turn_id: String,
        item_id: String,
        delta: String,
    },
    TurnError {
        turn_id: String,
        message: String,
        will_retry: bool,
    },
    ApprovalRequired {
        id: WireId,
        kind: ApprovalKind,
        params: Value,
    },
    UnsupportedRequest {
        id: WireId,
        method: String,
    },
    UnsupportedNotification {
        method: String,
    },
}

#[derive(Clone, Debug)]
enum Operation {
    Initialize,
    ThreadStart { cwd: String },
    TurnStart,
    Interrupt { turn_id: String },
}

impl Operation {
    fn method(&self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::ThreadStart { .. } => "thread/start",
            Self::TurnStart => "turn/start",
            Self::Interrupt { .. } => "turn/interrupt",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    #[serde(default, deserialize_with = "present")]
    id: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    method: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    params: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    result: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    error: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    trace: Option<Value>,
}

fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct Turn {
    id: String,
    status: TurnStatus,
    items: Vec<Value>,
}

/// Tracks a bounded projection of the pinned stable Codex wire protocol.
/// Byte inputs and returned frames describe protocol facts only.
/// This object grants no execution, authentication or recovery authority.
pub struct CodexProtocol {
    phase: State,
    next_id: i64,
    pending: Option<(i64, Operation)>,
    buffer: Vec<u8>,
    thread_id: Option<String>,
    turn: Option<Turn>,
    seen_turns: BTreeSet<String>,
    seen_server_ids: BTreeSet<WireId>,
    server_requests: BTreeMap<WireId, Option<ApprovalKind>>,
}

impl Default for CodexProtocol {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexProtocol {
    pub fn new() -> Self {
        let protocol = Self {
            phase: State::New,
            next_id: 1,
            pending: None,
            buffer: Vec::new(),
            thread_id: None,
            turn: None,
            seen_turns: BTreeSet::new(),
            seen_server_ids: BTreeSet::new(),
            server_requests: BTreeMap::new(),
        };
        protocol.assert_consistent();
        assert_eq!(protocol.phase, State::New);
        protocol
    }

    pub fn state(&self) -> State {
        self.assert_consistent();
        self.phase
    }

    pub fn initialize(&mut self) -> Result<Vec<u8>, ProtocolError> {
        self.require(State::New)?;
        let bytes = self.request(
            Operation::Initialize,
            json!({
                "clientInfo":{"name":"crazyagent","version":env!("CARGO_PKG_VERSION")},
                "capabilities":{"experimentalApi":false,"requestAttestation":false}
            }),
        )?;
        self.phase = State::Initializing;
        self.assert_consistent();
        assert!(self.pending.is_some());
        Ok(bytes)
    }

    pub fn initialized(&mut self) -> Result<Vec<u8>, ProtocolError> {
        self.require(State::Initialized)?;
        let bytes = encode(json!({"method":"initialized"}))?;
        self.phase = State::Ready;
        self.assert_consistent();
        assert!(self.pending.is_none());
        Ok(bytes)
    }

    pub fn start_thread(&mut self, cwd: &Path) -> Result<Vec<u8>, ProtocolError> {
        self.require(State::Ready)?;
        let cwd = valid_path(cwd).ok_or(ProtocolError::InvalidInput)?;
        let bytes = self.request(
            Operation::ThreadStart { cwd: cwd.into() },
            json!({
                "cwd":cwd,"approvalPolicy":"on-request",
                "approvalsReviewer":"user","sandbox":"read-only"
            }),
        )?;
        self.phase = State::ThreadStarting;
        self.assert_consistent();
        assert!(self.turn.is_none());
        Ok(bytes)
    }

    pub fn start_turn(&mut self, text: &str) -> Result<Vec<u8>, ProtocolError> {
        self.require(State::Idle)?;
        if text.trim().is_empty() || text.contains('\0') {
            return Err(ProtocolError::InvalidInput);
        }
        if text.len() > FRAME_BYTES_MAX {
            return Err(ProtocolError::FrameTooLarge);
        }
        if self.seen_turns.len() == TURNS_MAX {
            return Err(ProtocolError::Capacity);
        }
        let bytes = self.request(
            Operation::TurnStart,
            json!({
                "threadId":self.thread_id,
                "input":[{"type":"text","text":text,"text_elements":[]}]
            }),
        )?;
        self.turn = None;
        self.phase = State::TurnStarting;
        self.assert_consistent();
        assert!(self.server_requests.is_empty());
        Ok(bytes)
    }

    pub fn interrupt(&mut self) -> Result<Vec<u8>, ProtocolError> {
        self.require(State::Active)?;
        let turn_id = self
            .turn
            .as_ref()
            .expect("active turn invariant")
            .id
            .clone();
        let bytes = self.request(
            Operation::Interrupt {
                turn_id: turn_id.clone(),
            },
            json!({
                "threadId":self.thread_id,"turnId":turn_id
            }),
        )?;
        self.assert_consistent();
        assert_eq!(self.phase, State::Active);
        Ok(bytes)
    }

    pub fn deny_request(&mut self, id: &WireId) -> Result<Vec<u8>, ProtocolError> {
        self.assert_consistent();
        if self.phase == State::Failed {
            return Err(ProtocolError::InvalidState);
        }
        let kind = self
            .server_requests
            .get(id)
            .ok_or(ProtocolError::StaleMessage)?;
        let value = match kind {
            Some(ApprovalKind::Permissions) => {
                json!({"id":id,"result":{"permissions":{},"scope":"turn"}})
            }
            Some(_) => json!({"id":id,"result":{"decision":"decline"}}),
            None => json!({"id":id,"error":{
                "code":-32601,"message":"unsupported native request"
            }}),
        };
        let bytes = encode(value)?;
        let removed = self.server_requests.remove(id);
        assert!(removed.is_some());
        self.assert_consistent();
        Ok(bytes)
    }

    pub fn receive(&mut self, bytes: &[u8]) -> Result<Vec<Event>, ProtocolError> {
        self.assert_consistent();
        if self.phase == State::Failed {
            return Err(ProtocolError::InvalidState);
        }
        let result = self.decode(bytes);
        if result.is_err() {
            self.phase = State::Failed;
        }
        self.assert_consistent();
        assert!(result.is_ok() || self.phase == State::Failed);
        result
    }

    pub fn finish(&mut self) -> Result<(), ProtocolError> {
        self.assert_consistent();
        self.phase = State::Failed;
        assert_eq!(self.phase, State::Failed);
        Err(ProtocolError::Disconnected)
    }

    fn assert_consistent(&self) {
        assert!(self.next_id > 0);
        assert!(self.buffer.len() <= FRAME_BYTES_MAX);
        assert!(self.seen_turns.len() <= TURNS_MAX);
        assert!(self.seen_server_ids.len() <= SERVER_IDS_MAX);
        assert!(self.server_requests.len() <= APPROVALS_MAX);
        assert!(
            self.server_requests
                .keys()
                .all(|id| self.seen_server_ids.contains(id))
        );
        if let Some((id, _)) = &self.pending {
            assert!(*id > 0 && *id < self.next_id);
        }
        if let Some(turn) = &self.turn {
            assert!(self.thread_id.is_some());
            assert!(self.seen_turns.contains(&turn.id));
            assert!(
                turn.status == TurnStatus::InProgress
                    || self.server_requests.values().all(Option::is_none)
            );
        }
        if self.phase == State::Active {
            assert_eq!(
                self.turn.as_ref().map(|turn| turn.status),
                Some(TurnStatus::InProgress)
            );
        }
        if self.phase == State::Idle {
            assert!(self.thread_id.is_some());
            assert!(
                self.turn
                    .as_ref()
                    .is_none_or(|turn| turn.status != TurnStatus::InProgress)
            );
        }
    }

    fn require(&self, phase: State) -> Result<(), ProtocolError> {
        self.assert_consistent();
        if self.phase != phase
            || self.pending.is_some()
            || (phase != State::Active && !self.server_requests.is_empty())
        {
            return Err(ProtocolError::InvalidState);
        }
        assert_eq!(self.phase, phase);
        assert!(self.pending.is_none());
        Ok(())
    }

    fn request(&mut self, operation: Operation, params: Value) -> Result<Vec<u8>, ProtocolError> {
        assert!(self.pending.is_none());
        assert!(self.next_id > 0);
        let next = self.next_id.checked_add(1).ok_or(ProtocolError::Capacity)?;
        let bytes = encode(json!({"id":self.next_id,"method":operation.method(),"params":params}))?;
        self.pending = Some((self.next_id, operation));
        self.next_id = next;
        self.assert_consistent();
        Ok(bytes)
    }

    fn decode(&mut self, bytes: &[u8]) -> Result<Vec<Event>, ProtocolError> {
        if bytes.len() > FRAME_BYTES_MAX {
            return Err(ProtocolError::FrameTooLarge);
        }
        let mut events = Vec::new();
        let mut frames = 0;
        for byte in bytes {
            if *byte != b'\n' {
                if self.buffer.len() == FRAME_BYTES_MAX {
                    return Err(ProtocolError::FrameTooLarge);
                }
                self.buffer.push(*byte);
                continue;
            }
            if frames == EVENTS_MAX {
                return Err(ProtocolError::Capacity);
            }
            frames += 1;
            let envelope: Envelope =
                serde_json::from_slice(&self.buffer).map_err(|_| ProtocolError::InvalidMessage)?;
            self.buffer.clear();
            if let Some(event) = self.message(envelope)? {
                events.push(event);
            }
            assert!(events.len() <= frames);
        }
        assert!(events.len() <= EVENTS_MAX);
        assert!(self.buffer.len() <= FRAME_BYTES_MAX);
        Ok(events)
    }

    fn message(&mut self, envelope: Envelope) -> Result<Option<Event>, ProtocolError> {
        if let Some(method) = envelope.method {
            if envelope.result.is_some() || envelope.error.is_some() {
                return Err(ProtocolError::InvalidMessage);
            }
            let method = identifier(&method)?;
            let params = envelope.params.unwrap_or(Value::Null);
            if let Some(id) = envelope.id {
                return self.server_request(wire_id(id)?, method, params).map(Some);
            }
            if envelope.trace.is_some() {
                return Err(ProtocolError::InvalidMessage);
            }
            return self.notification(method, params);
        }
        if envelope.params.is_some()
            || envelope.trace.is_some()
            || envelope.result.is_some() == envelope.error.is_some()
        {
            return Err(ProtocolError::InvalidMessage);
        }
        let id = envelope.id.ok_or(ProtocolError::InvalidMessage)?;
        self.response(wire_id(id)?, envelope.result, envelope.error)
    }

    fn response(
        &mut self,
        id: WireId,
        result: Option<Value>,
        error: Option<Value>,
    ) -> Result<Option<Event>, ProtocolError> {
        let (expected, operation) = self.pending.clone().ok_or(ProtocolError::StaleMessage)?;
        if id != WireId::Number(expected) {
            return Err(ProtocolError::StaleMessage);
        }
        let event = if let Some(error) = error {
            self.remote_error(expected, &operation, error)?
        } else {
            self.success(&operation, result.ok_or(ProtocolError::InvalidMessage)?)?
        };
        self.pending = None;
        self.assert_consistent();
        assert!(self.pending.is_none());
        Ok(event)
    }

    fn remote_error(
        &mut self,
        id: i64,
        operation: &Operation,
        error: Value,
    ) -> Result<Option<Event>, ProtocolError> {
        let code = error
            .get("code")
            .and_then(Value::as_i64)
            .ok_or(ProtocolError::InvalidMessage)?;
        let message = message_text(&error["message"])?;
        self.phase = match operation {
            Operation::Initialize => State::New,
            Operation::ThreadStart { .. } if self.thread_id.is_none() => State::Ready,
            Operation::TurnStart if self.turn.is_none() => State::Idle,
            Operation::Interrupt { .. } => self.phase,
            _ => return Err(ProtocolError::InvalidMessage),
        };
        assert!(self.pending.is_some());
        self.assert_consistent();
        Ok(Some(Event::RequestFailed {
            id,
            method: operation.method(),
            code,
            message,
        }))
    }

    fn success(
        &mut self,
        operation: &Operation,
        result: Value,
    ) -> Result<Option<Event>, ProtocolError> {
        if !result.is_object() {
            return Err(ProtocolError::InvalidMessage);
        }
        match operation {
            Operation::Initialize => {
                for key in ["userAgent", "platformFamily", "platformOs"] {
                    identifier(&result[key])?;
                }
                let home = result["codexHome"]
                    .as_str()
                    .ok_or(ProtocolError::InvalidMessage)?;
                valid_path(Path::new(home)).ok_or(ProtocolError::InvalidMessage)?;
                self.phase = State::Initialized;
                Ok(Some(Event::Initialized))
            }
            Operation::ThreadStart { cwd } => {
                if result["cwd"] != *cwd
                    || result["approvalPolicy"] != "on-request"
                    || result["approvalsReviewer"] != "user"
                    || result["sandbox"]["type"] != "readOnly"
                    || result["sandbox"]
                        .get("networkAccess")
                        .is_some_and(|v| v != false)
                {
                    return Err(ProtocolError::InvalidMessage);
                }
                identifier(&result["model"])?;
                identifier(&result["modelProvider"])?;
                let event = self.observe_thread(&result["thread"], cwd)?;
                self.phase = State::Idle;
                Ok(event)
            }
            Operation::TurnStart => self.observe_turn(&result["turn"], true),
            Operation::Interrupt { turn_id } => {
                if !result.as_object().is_some_and(|object| object.is_empty()) {
                    return Err(ProtocolError::InvalidMessage);
                }
                Ok(Some(Event::InterruptAcknowledged {
                    turn_id: turn_id.clone(),
                }))
            }
        }
    }

    fn observe_thread(
        &mut self,
        thread: &Value,
        cwd: &str,
    ) -> Result<Option<Event>, ProtocolError> {
        let id = identifier(&thread["id"])?;
        if thread["cwd"] != cwd
            || !thread["turns"].as_array().is_some_and(Vec::is_empty)
            || thread["status"]["type"] != "idle"
        {
            return Err(ProtocolError::InvalidMessage);
        }
        if let Some(existing) = &self.thread_id {
            return if *existing == id {
                Ok(None)
            } else {
                Err(ProtocolError::StaleMessage)
            };
        }
        self.thread_id = Some(id.clone());
        assert!(self.turn.is_none());
        assert_eq!(self.thread_id.as_ref(), Some(&id));
        Ok(Some(Event::ThreadStarted { thread_id: id }))
    }

    fn observe_turn(
        &mut self,
        value: &Value,
        response: bool,
    ) -> Result<Option<Event>, ProtocolError> {
        let turn: Turn =
            serde_json::from_value(value.clone()).map_err(|_| ProtocolError::InvalidMessage)?;
        identifier(&json!(turn.id))?;
        if turn.items.len() > EVENTS_MAX {
            return Err(ProtocolError::Capacity);
        }
        if let Some(existing) = &self.turn {
            if existing.id != turn.id {
                return Err(ProtocolError::StaleMessage);
            }
            if response && (turn.status == TurnStatus::InProgress || turn.status == existing.status)
            {
                return Ok(None);
            }
            if existing.status != TurnStatus::InProgress {
                return Err(ProtocolError::StaleMessage);
            }
            if turn.status == TurnStatus::InProgress {
                return Ok(None);
            }
        } else {
            if !matches!(self.pending, Some((_, Operation::TurnStart)))
                || self.seen_turns.contains(&turn.id)
            {
                return Err(ProtocolError::StaleMessage);
            }
            if self.seen_turns.len() == TURNS_MAX {
                return Err(ProtocolError::Capacity);
            }
            let inserted = self.seen_turns.insert(turn.id.clone());
            assert!(inserted);
        }
        let event = if turn.status == TurnStatus::InProgress {
            self.phase = State::Active;
            Event::TurnStarted {
                turn_id: turn.id.clone(),
            }
        } else {
            self.phase = State::Idle;
            self.server_requests.clear();
            Event::TurnCompleted {
                turn_id: turn.id.clone(),
                status: turn.status,
            }
        };
        self.turn = Some(turn);
        self.assert_consistent();
        assert!(self.turn.is_some());
        Ok(Some(event))
    }

    fn notification(
        &mut self,
        method: String,
        params: Value,
    ) -> Result<Option<Event>, ProtocolError> {
        match method.as_str() {
            "thread/started" => {
                let Some((_, Operation::ThreadStart { cwd })) = self.pending.clone() else {
                    return Err(ProtocolError::StaleMessage);
                };
                self.observe_thread(&params["thread"], &cwd)
            }
            "turn/started" | "turn/completed" => {
                self.check_thread(&params)?;
                let status = &params["turn"]["status"];
                if (method == "turn/started") != (status == "inProgress") {
                    return Err(ProtocolError::InvalidMessage);
                }
                self.observe_turn(&params["turn"], false)
            }
            "item/agentMessage/delta" => {
                let turn_id = self.check_active(&params)?;
                let item_id = identifier(&params["itemId"])?;
                let delta = message_text(&params["delta"])?;
                Ok(Some(Event::AgentMessageDelta {
                    turn_id,
                    item_id,
                    delta,
                }))
            }
            "error" => {
                let turn_id = self.check_active(&params)?;
                let message = message_text(&params["error"]["message"])?;
                let will_retry = params["willRetry"]
                    .as_bool()
                    .ok_or(ProtocolError::InvalidMessage)?;
                Ok(Some(Event::TurnError {
                    turn_id,
                    message,
                    will_retry,
                }))
            }
            _ => Ok(Some(Event::UnsupportedNotification { method })),
        }
    }

    fn check_thread(&self, params: &Value) -> Result<(), ProtocolError> {
        let id = identifier(&params["threadId"])?;
        if self.thread_id.as_ref() != Some(&id) {
            return Err(ProtocolError::StaleMessage);
        }
        self.assert_consistent();
        assert!(self.thread_id.is_some());
        Ok(())
    }

    fn check_active(&self, params: &Value) -> Result<String, ProtocolError> {
        self.check_thread(params)?;
        let id = identifier(&params["turnId"])?;
        if !self
            .turn
            .as_ref()
            .is_some_and(|turn| turn.id == id && turn.status == TurnStatus::InProgress)
        {
            return Err(ProtocolError::StaleMessage);
        }
        assert_eq!(self.phase, State::Active);
        assert!(self.turn.is_some());
        Ok(id)
    }

    fn server_request(
        &mut self,
        id: WireId,
        method: String,
        params: Value,
    ) -> Result<Event, ProtocolError> {
        if self.seen_server_ids.contains(&id) {
            return Err(ProtocolError::StaleMessage);
        }
        if self.server_requests.len() == APPROVALS_MAX
            || self.seen_server_ids.len() == SERVER_IDS_MAX
        {
            return Err(ProtocolError::Capacity);
        }
        let kind = match method.as_str() {
            "item/commandExecution/requestApproval" => Some(ApprovalKind::Command),
            "item/fileChange/requestApproval" => Some(ApprovalKind::FileChange),
            "item/permissions/requestApproval" => Some(ApprovalKind::Permissions),
            _ => None,
        };
        if kind.is_some() {
            self.check_active(&params)?;
            identifier(&params["itemId"])?;
            params["startedAtMs"]
                .as_i64()
                .ok_or(ProtocolError::InvalidMessage)?;
        }
        if kind == Some(ApprovalKind::Permissions) {
            let cwd = params["cwd"]
                .as_str()
                .ok_or(ProtocolError::InvalidMessage)?;
            valid_path(Path::new(cwd)).ok_or(ProtocolError::InvalidMessage)?;
            if !params["permissions"].is_object() {
                return Err(ProtocolError::InvalidMessage);
            }
        }
        let inserted = self.seen_server_ids.insert(id.clone());
        let previous = self.server_requests.insert(id.clone(), kind);
        assert!(inserted);
        assert!(previous.is_none());
        match kind {
            Some(kind) => Ok(Event::ApprovalRequired { id, kind, params }),
            None => Ok(Event::UnsupportedRequest { id, method }),
        }
    }
}

fn encode(value: Value) -> Result<Vec<u8>, ProtocolError> {
    let mut bytes = serde_json::to_vec(&value).map_err(|_| ProtocolError::InvalidInput)?;
    if bytes.len() >= FRAME_BYTES_MAX {
        return Err(ProtocolError::FrameTooLarge);
    }
    bytes.push(b'\n');
    assert!(bytes.len() <= FRAME_BYTES_MAX);
    assert_eq!(bytes.last(), Some(&b'\n'));
    Ok(bytes)
}

fn identifier(value: &Value) -> Result<String, ProtocolError> {
    let text = value.as_str().ok_or(ProtocolError::InvalidMessage)?;
    if text.trim().is_empty() || text.len() > ID_BYTES_MAX || text.chars().any(char::is_control) {
        return Err(ProtocolError::InvalidMessage);
    }
    Ok(text.into())
}

fn message_text(value: &Value) -> Result<String, ProtocolError> {
    let text = value.as_str().ok_or(ProtocolError::InvalidMessage)?;
    if text.len() > FRAME_BYTES_MAX || text.contains('\0') {
        return Err(ProtocolError::InvalidMessage);
    }
    Ok(text.into())
}

fn wire_id(value: Value) -> Result<WireId, ProtocolError> {
    match value {
        Value::String(_) => identifier(&value).map(WireId::Text),
        Value::Number(number) => number
            .as_i64()
            .map(WireId::Number)
            .ok_or(ProtocolError::InvalidMessage),
        _ => Err(ProtocolError::InvalidMessage),
    }
}

fn valid_path(path: &Path) -> Option<&str> {
    let text = path.to_str()?;
    if !path.is_absolute()
        || text.len() > 4096
        || text.chars().any(char::is_control)
        || path.components().count() > 64
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return None;
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "assertion failed: self.next_id > 0")]
    fn sequence_invariant_detects_a_corrupt_counter() {
        let mut protocol = CodexProtocol::new();
        protocol.next_id = 0;
        protocol.assert_consistent();
    }

    #[test]
    #[should_panic(expected = "assertion failed: *id > 0 && *id < self.next_id")]
    fn pending_identity_invariant_detects_a_reused_counter() {
        let mut protocol = CodexProtocol::new();
        protocol.initialize().unwrap();
        protocol.next_id = 1;
        protocol.assert_consistent();
    }

    #[test]
    #[should_panic(expected = "assertion failed: self.seen_turns.contains(&turn.id)")]
    fn turn_membership_invariant_detects_an_untracked_turn() {
        let mut protocol = CodexProtocol::new();
        protocol.thread_id = Some("thread-unit".into());
        protocol.turn = Some(Turn {
            id: "orphan-unit".into(),
            status: TurnStatus::InProgress,
            items: Vec::new(),
        });
        protocol.assert_consistent();
    }

    #[test]
    #[should_panic(expected = "assertion failed: self.server_requests.keys()")]
    fn request_membership_invariant_detects_an_untracked_native_id() {
        let mut protocol = CodexProtocol::new();
        protocol
            .server_requests
            .insert(WireId::Text("orphan-unit".into()), None);
        protocol.assert_consistent();
    }

    #[test]
    fn exhausted_counter_never_wraps_or_reserves_a_request() {
        let mut protocol = CodexProtocol::new();
        protocol.next_id = i64::MAX;
        assert_eq!(protocol.initialize(), Err(ProtocolError::Capacity));
        assert_eq!(protocol.state(), State::New);
        assert!(protocol.pending.is_none());
    }

    #[test]
    fn remembered_native_ids_have_a_connection_lifetime_bound() {
        let mut protocol = CodexProtocol::new();
        for id in 0..SERVER_IDS_MAX {
            protocol.seen_server_ids.insert(WireId::Number(id as i64));
        }
        assert_eq!(
            protocol.server_request(
                WireId::Number(SERVER_IDS_MAX as i64),
                "unsupported".into(),
                json!({})
            ),
            Err(ProtocolError::Capacity)
        );
        assert_eq!(protocol.seen_server_ids.len(), SERVER_IDS_MAX);
        assert!(protocol.server_requests.is_empty());
    }
}
