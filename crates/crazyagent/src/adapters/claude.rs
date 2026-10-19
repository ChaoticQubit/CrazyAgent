use super::events::{
    Capability, EventKind, HarnessEvent, Provenance, RateLimitStatus, Support, TokenUsage,
    TurnOutcome,
};
use super::{NativeAction, NativeHarness};
use crate::protocol::{CredentialRef, FRAME_BYTES_MAX, RequestId};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value, json};
use std::collections::BTreeSet;
use std::fmt;

const EVENTS_MAX: usize = 128;
const NATIVE_IDS_MAX: usize = 4096;
const REQUESTS_MAX: usize = 128;
const ID_BYTES_MAX: usize = 256;
const OUTPUT_BYTES_MAX: usize = 262_144;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    AwaitingInit,
    Idle,
    Active,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    InvalidInput,
    InvalidState,
    InvalidMessage,
    FrameTooLarge,
    Capacity,
    StaleMessage,
    Disconnected,
    Unsupported,
}

/// Projects bounded Claude structured stream messages into shared observations.
/// Inputs bind a launch action and account reference without executing either.
/// Outputs preserve observed identity and never qualify native admission.
pub struct ClaudeProtocol {
    phase: State,
    provenance: Provenance,
    native_session_id: Option<String>,
    active_request: Option<RequestId>,
    seen_requests: BTreeSet<RequestId>,
    seen_events: BTreeSet<String>,
    buffer: Vec<u8>,
}

impl ClaudeProtocol {
    pub fn new(action: NativeAction, account_ref: CredentialRef) -> Result<Self, ProtocolError> {
        let NativeAction::Launch {
            session_id,
            harness: NativeHarness::Claude,
            profile_ref,
            worktree_lease,
        } = action
        else {
            return Err(ProtocolError::Unsupported);
        };
        let protocol = Self {
            phase: State::AwaitingInit,
            provenance: Provenance {
                session_id,
                harness: NativeHarness::Claude,
                account_ref,
                profile_ref,
                worktree_lease,
            },
            native_session_id: None,
            active_request: None,
            seen_requests: BTreeSet::new(),
            seen_events: BTreeSet::new(),
            buffer: Vec::new(),
        };
        protocol.assert_consistent();
        Ok(protocol)
    }

    pub fn state(&self) -> State {
        self.assert_consistent();
        self.phase
    }

    pub fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::PromptInput | Capability::StructuredEvents => Support::ProtocolOnly,
            Capability::NativeAdmission | Capability::CancelChild | Capability::LiveRecovery => {
                Support::Unavailable
            }
        }
    }

    pub fn start_turn(
        &mut self,
        request_id: RequestId,
        text: &str,
    ) -> Result<Vec<u8>, ProtocolError> {
        self.assert_consistent();
        if !matches!(self.phase, State::AwaitingInit | State::Idle) || self.active_request.is_some()
        {
            return Err(ProtocolError::InvalidState);
        }
        if text.trim().is_empty() || text.contains('\0') {
            return Err(ProtocolError::InvalidInput);
        }
        if text.len() > FRAME_BYTES_MAX {
            return Err(ProtocolError::FrameTooLarge);
        }
        if self.seen_requests.contains(&request_id) {
            return Err(ProtocolError::StaleMessage);
        }
        if self.seen_requests.len() == REQUESTS_MAX {
            return Err(ProtocolError::Capacity);
        }
        let mut value = json!({
            "type":"user", "message":{"role":"user","content":text},
            "parent_tool_use_id":null
        });
        if let Some(session_id) = &self.native_session_id {
            value["session_id"] = json!(session_id);
        }
        let mut bytes = serde_json::to_vec(&value).map_err(|_| ProtocolError::InvalidInput)?;
        if bytes.len() >= FRAME_BYTES_MAX {
            return Err(ProtocolError::FrameTooLarge);
        }
        bytes.push(b'\n');
        self.seen_requests.insert(request_id.clone());
        self.active_request = Some(request_id);
        if self.native_session_id.is_some() {
            self.phase = State::Active;
        }
        self.assert_consistent();
        assert!(self.active_request.is_some());
        Ok(bytes)
    }

    pub fn receive(&mut self, bytes: &[u8]) -> Result<Vec<HarnessEvent>, ProtocolError> {
        self.assert_consistent();
        if self.phase == State::Failed {
            return Err(ProtocolError::InvalidState);
        }
        let result = self.decode(bytes);
        if result.is_err() {
            self.phase = State::Failed;
            self.buffer.clear();
        }
        self.assert_consistent();
        assert!(result.is_ok() || self.phase == State::Failed);
        result
    }

    pub fn finish(&mut self) -> Result<(), ProtocolError> {
        self.assert_consistent();
        if self.phase == State::Failed {
            return Err(ProtocolError::InvalidState);
        }
        self.phase = State::Failed;
        self.buffer.clear();
        self.assert_consistent();
        Err(ProtocolError::Disconnected)
    }

    fn assert_consistent(&self) {
        assert!(self.buffer.len() <= FRAME_BYTES_MAX);
        assert!(self.seen_events.len() <= NATIVE_IDS_MAX);
        assert!(self.seen_requests.len() <= REQUESTS_MAX);
        assert_eq!(self.provenance.harness, NativeHarness::Claude);
        if let Some(request) = &self.active_request {
            assert!(self.seen_requests.contains(request));
        }
        match self.phase {
            State::AwaitingInit => assert!(self.native_session_id.is_none()),
            State::Idle => {
                assert!(self.native_session_id.is_some());
                assert!(self.active_request.is_none());
            }
            State::Active => {
                assert!(self.native_session_id.is_some());
                assert!(self.active_request.is_some());
            }
            State::Failed => {}
        }
    }

    fn decode(&mut self, bytes: &[u8]) -> Result<Vec<HarnessEvent>, ProtocolError> {
        if bytes.len() > FRAME_BYTES_MAX {
            return Err(ProtocolError::FrameTooLarge);
        }
        let mut events = Vec::new();
        let mut frames = 0;
        let mut output_bytes = 0_usize;
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
            let StrictValue(value) =
                serde_json::from_slice(&self.buffer).map_err(|_| ProtocolError::InvalidMessage)?;
            self.buffer.clear();
            let mut nodes = 0;
            validate_shape(&value, 0, &mut nodes)?;
            for event in self.message(&value)? {
                if events.len() == EVENTS_MAX {
                    return Err(ProtocolError::Capacity);
                }
                output_bytes += serde_json::to_vec(&event)
                    .map_err(|_| ProtocolError::InvalidMessage)?
                    .len();
                if output_bytes > OUTPUT_BYTES_MAX {
                    return Err(ProtocolError::Capacity);
                }
                events.push(event);
            }
        }
        assert!(events.len() <= EVENTS_MAX);
        assert!(output_bytes <= OUTPUT_BYTES_MAX);
        Ok(events)
    }

    fn message(&mut self, value: &Value) -> Result<Vec<HarnessEvent>, ProtocolError> {
        let message_type = identifier(field(value, "type")?)?;
        let subtype = optional_text(value, "subtype")?;
        let session = identifier(field(value, "session_id")?)?;
        let uuid = identifier(field(value, "uuid")?)?;
        let native_user_message_uuid = optional_text(value, "user_message_uuid")?;
        let session_wide = message_type == "rate_limit_event";
        if self.seen_events.contains(uuid) {
            return Err(ProtocolError::StaleMessage);
        }
        if self.seen_events.len() == NATIVE_IDS_MAX {
            return Err(ProtocolError::Capacity);
        }
        let kinds = if message_type == "system" && subtype.as_deref() == Some("init") {
            if self.native_session_id.is_some() {
                return Err(ProtocolError::StaleMessage);
            }
            let model = identifier(field(value, "model")?)?.to_owned();
            let capabilities = string_list(value, "capabilities")?;
            self.native_session_id = Some(session.to_owned());
            self.phase = if self.active_request.is_some() {
                State::Active
            } else {
                State::Idle
            };
            vec![EventKind::Initialized {
                model,
                capabilities,
            }]
        } else {
            let native_session = self
                .native_session_id
                .as_deref()
                .ok_or(ProtocolError::InvalidState)?;
            if session != native_session {
                return Err(ProtocolError::StaleMessage);
            }
            match message_type {
                "assistant" => {
                    self.require_active()?;
                    assistant(value)?
                }
                "result" => {
                    self.require_active()?;
                    vec![result(value)?]
                }
                "rate_limit_event" => vec![rate_limit(value)?],
                _ => vec![EventKind::Unsupported {
                    message_type: message_type.to_owned(),
                    subtype,
                }],
            }
        };
        let native_session_id = self
            .native_session_id
            .as_ref()
            .ok_or(ProtocolError::InvalidState)?;
        let events = kinds
            .into_iter()
            .map(|kind| HarnessEvent {
                provenance: self.provenance.clone(),
                request_id: self.active_request.clone().filter(|_| !session_wide),
                native_session_id: native_session_id.clone(),
                native_event_id: uuid.to_owned(),
                native_user_message_uuid: native_user_message_uuid
                    .clone()
                    .filter(|_| !session_wide),
                kind,
            })
            .collect();
        self.seen_events.insert(uuid.to_owned());
        if message_type == "result" {
            self.active_request = None;
            self.phase = State::Idle;
        }
        self.assert_consistent();
        Ok(events)
    }

    fn require_active(&self) -> Result<(), ProtocolError> {
        if self.phase != State::Active {
            return Err(ProtocolError::InvalidState);
        }
        assert!(self.active_request.is_some());
        Ok(())
    }
}

fn assistant(value: &Value) -> Result<Vec<EventKind>, ProtocolError> {
    let message = field(value, "message")?;
    let message_id = identifier(field(message, "id")?)?;
    if field(message, "role")?.as_str() != Some("assistant") {
        return Err(ProtocolError::InvalidMessage);
    }
    let parent = optional_text(value, "parent_tool_use_id")?;
    if let Some(parent) = &parent {
        identifier(&json!(parent))?;
    }
    let blocks = field(message, "content")?
        .as_array()
        .ok_or(ProtocolError::InvalidMessage)?;
    if blocks.len() > EVENTS_MAX {
        return Err(ProtocolError::Capacity);
    }
    let mut events = Vec::new();
    if let Some(code) = optional_text(value, "error")? {
        events.push(EventKind::AssistantError { code });
    }
    for block in blocks {
        let kind = match identifier(field(block, "type")?)? {
            "text" => EventKind::AssistantText {
                message_id: message_id.to_owned(),
                parent_tool_use_id: parent.clone(),
                text: field(block, "text")?
                    .as_str()
                    .ok_or(ProtocolError::InvalidMessage)?
                    .to_owned(),
            },
            "tool_use" => {
                let input = field(block, "input")?;
                if !input.is_object() {
                    return Err(ProtocolError::InvalidMessage);
                }
                EventKind::ToolUse {
                    message_id: message_id.to_owned(),
                    parent_tool_use_id: parent.clone(),
                    tool_use_id: identifier(field(block, "id")?)?.to_owned(),
                    name: identifier(field(block, "name")?)?.to_owned(),
                    input: input.clone(),
                }
            }
            subtype => EventKind::Unsupported {
                message_type: "assistant.content".into(),
                subtype: Some(subtype.to_owned()),
            },
        };
        events.push(kind);
    }
    Ok(events)
}

fn result(value: &Value) -> Result<EventKind, ProtocolError> {
    let subtype = identifier(field(value, "subtype")?)?.to_owned();
    let is_error = field(value, "is_error")?
        .as_bool()
        .ok_or(ProtocolError::InvalidMessage)?;
    let terminal_reason = optional_text(value, "terminal_reason")?;
    let outcome = if is_error || subtype.starts_with("error_") {
        TurnOutcome::Failed
    } else if subtype == "success" && terminal_reason.as_deref().is_none_or(|s| s == "completed") {
        TurnOutcome::Success
    } else {
        TurnOutcome::Incomplete
    };
    let usage = match value.get("usage") {
        None | Some(Value::Null) => TokenUsage::default(),
        Some(usage) if usage.is_object() => TokenUsage {
            input_tokens: optional_count(usage, "input_tokens")?,
            output_tokens: optional_count(usage, "output_tokens")?,
            cache_read_input_tokens: optional_count(usage, "cache_read_input_tokens")?,
            cache_creation_input_tokens: optional_count(usage, "cache_creation_input_tokens")?,
        },
        _ => return Err(ProtocolError::InvalidMessage),
    };
    let estimated_cost_usd = match value.get("total_cost_usd") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let cost = value
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .ok_or(ProtocolError::InvalidMessage)?;
            Some(cost)
        }
    };
    Ok(EventKind::TurnFinished {
        outcome,
        subtype,
        result: optional_text(value, "result")?,
        errors: string_list(value, "errors")?,
        terminal_reason,
        usage,
        estimated_cost_usd,
    })
}

fn rate_limit(value: &Value) -> Result<EventKind, ProtocolError> {
    let info = field(value, "rate_limit_info")?;
    if !info.is_object() {
        return Err(ProtocolError::InvalidMessage);
    }
    let utilization = info
        .get("utilization")
        .map(|value| {
            value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or(ProtocolError::InvalidMessage)
        })
        .transpose()?;
    Ok(EventKind::RateLimitObserved {
        status: rate_limit_status(field(info, "status")?)?,
        resets_at_unix_secs: optional_count(info, "resetsAt")?,
        rate_limit_type: info.get("rateLimitType").map(rate_limit_type).transpose()?,
        utilization,
        overage_status: info
            .get("overageStatus")
            .map(rate_limit_status)
            .transpose()?,
        overage_resets_at_unix_secs: optional_count(info, "overageResetsAt")?,
        overage_disabled_reason: info
            .get("overageDisabledReason")
            .map(overage_disabled_reason)
            .transpose()?,
    })
}

fn rate_limit_status(value: &Value) -> Result<RateLimitStatus, ProtocolError> {
    match value.as_str().ok_or(ProtocolError::InvalidMessage)? {
        "allowed" => Ok(RateLimitStatus::Allowed),
        "allowed_warning" => Ok(RateLimitStatus::AllowedWarning),
        "rejected" => Ok(RateLimitStatus::Rejected),
        _ => Err(ProtocolError::Unsupported),
    }
}

fn rate_limit_type(value: &Value) -> Result<String, ProtocolError> {
    let value = value.as_str().ok_or(ProtocolError::InvalidMessage)?;
    match value {
        "five_hour"
        | "seven_day"
        | "seven_day_opus"
        | "seven_day_sonnet"
        | "seven_day_overage_included"
        | "overage" => Ok(value.to_owned()),
        _ => Err(ProtocolError::Unsupported),
    }
}

fn overage_disabled_reason(value: &Value) -> Result<String, ProtocolError> {
    let value = value.as_str().ok_or(ProtocolError::InvalidMessage)?;
    match value {
        "overage_not_provisioned"
        | "org_level_disabled"
        | "org_level_disabled_until"
        | "out_of_credits"
        | "seat_tier_level_disabled"
        | "member_level_disabled"
        | "seat_tier_zero_credit_limit"
        | "group_zero_credit_limit"
        | "member_zero_credit_limit"
        | "org_service_level_disabled"
        | "no_limits_configured"
        | "fetch_error"
        | "unknown" => Ok(value.to_owned()),
        _ => Err(ProtocolError::Unsupported),
    }
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, ProtocolError> {
    value.get(name).ok_or(ProtocolError::InvalidMessage)
}

fn identifier(value: &Value) -> Result<&str, ProtocolError> {
    let value = value.as_str().ok_or(ProtocolError::InvalidMessage)?;
    if value.is_empty()
        || value.len() > ID_BYTES_MAX
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(ProtocolError::InvalidMessage);
    }
    Ok(value)
}

fn optional_text(value: &Value, name: &str) -> Result<Option<String>, ProtocolError> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        _ => Err(ProtocolError::InvalidMessage),
    }
}

fn optional_count(value: &Value, name: &str) -> Result<Option<u64>, ProtocolError> {
    value
        .get(name)
        .map(|v| v.as_u64().ok_or(ProtocolError::InvalidMessage))
        .transpose()
}

fn string_list(value: &Value, name: &str) -> Result<Vec<String>, ProtocolError> {
    match value.get(name) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) if items.len() <= EVENTS_MAX => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or(ProtocolError::InvalidMessage)
            })
            .collect(),
        _ => Err(ProtocolError::InvalidMessage),
    }
}

fn validate_shape(value: &Value, depth: usize, nodes: &mut usize) -> Result<(), ProtocolError> {
    if depth > 32 || *nodes == 4096 {
        return Err(ProtocolError::Capacity);
    }
    *nodes += 1;
    match value {
        Value::Array(items) => {
            for item in items {
                validate_shape(item, depth + 1, nodes)?;
            }
        }
        Value::Object(items) => {
            for item in items.values() {
                validate_shape(item, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Number::from_f64(value)
            .map(|n| StrictValue(Value::Number(n)))
            .ok_or_else(|| E::custom("invalid finite number"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_owned())))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(StrictValue(value)) = seq.next_element()? {
            values.push(value);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut values = Map::new();
        while let Some((key, StrictValue(value))) = map.next_entry::<String, StrictValue>()? {
            if values.insert(key, value).is_some() {
                return Err(de::Error::custom("duplicate object key"));
            }
        }
        Ok(StrictValue(Value::Object(values)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SessionId;

    fn protocol() -> ClaudeProtocol {
        ClaudeProtocol::new(
            NativeAction::Launch {
                session_id: SessionId::new("internal-session").unwrap(),
                harness: NativeHarness::Claude,
                profile_ref: CredentialRef::new("profile").unwrap(),
                worktree_lease: RequestId::new("lease").unwrap(),
            },
            CredentialRef::new("account").unwrap(),
        )
        .unwrap()
    }

    #[test]
    #[should_panic]
    fn active_request_invariant_detects_an_unremembered_request() {
        let mut p = protocol();
        p.active_request = Some(RequestId::new("unremembered").unwrap());
        p.state();
    }

    #[test]
    #[should_panic]
    fn idle_invariant_detects_a_missing_native_session() {
        let mut p = protocol();
        p.phase = State::Idle;
        p.state();
    }

    #[test]
    #[should_panic]
    fn buffer_invariant_detects_overflow() {
        let mut p = protocol();
        p.buffer = vec![0; FRAME_BYTES_MAX + 1];
        p.state();
    }
}
