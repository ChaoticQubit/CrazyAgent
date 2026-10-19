use crate::protocol::{FRAME_BYTES_MAX, ProtocolError, RequestId, SessionId, encode_frame};
use serde::de::Error as _;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Number, Value};
use std::fmt;
use std::io::Read;

pub const HOST_PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Control(RawControl);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RawControl {
    protocol_version: u16,
    session_id: SessionId,
    request_id: RequestId,
    control: ControlBody,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ControlBody {
    Prompt {
        turn_id: RequestId,
        text: String,
    },
    Steer {
        turn_id: RequestId,
        text: String,
    },
    Cancel {
        turn_id: RequestId,
    },
    Answer {
        turn_id: RequestId,
        prompt_id: RequestId,
        answers: Vec<String>,
    },
    Shutdown {},
}

impl RawControl {
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol_version != HOST_PROTOCOL_VERSION {
            return Err(ProtocolError("unsupported host control version".into()));
        }
        match &self.control {
            ControlBody::Prompt { text, .. } | ControlBody::Steer { text, .. } => {
                bounded_text(text, 16384)?;
                if text.trim().is_empty() {
                    return Err(ProtocolError("empty host prompt".into()));
                }
            }
            ControlBody::Answer { answers, .. } => {
                if answers.is_empty() || answers.len() > 8 {
                    return Err(ProtocolError("host answer count out of bounds".into()));
                }
                for answer in answers {
                    bounded_text(answer, 1024)?;
                }
            }
            ControlBody::Cancel { .. } | ControlBody::Shutdown {} => {}
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for Control {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawControl::deserialize(deserializer)
            .map_err(|_| D::Error::custom("invalid host control"))?;
        raw.validate().map_err(D::Error::custom)?;
        Ok(Self(raw))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Event(RawEvent);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RawEvent {
    protocol_version: u16,
    session_id: SessionId,
    instance_id: RequestId,
    sequence: u64,
    event: EventBody,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EventBody {
    Ready {},
    MessageStarted {
        turn_id: RequestId,
        message_id: RequestId,
    },
    MessageDelta {
        turn_id: RequestId,
        message_id: RequestId,
        text: String,
    },
    MessageReset {
        turn_id: RequestId,
        message_id: RequestId,
    },
    MessageDiscarded {
        turn_id: RequestId,
        message_id: RequestId,
    },
    MessageFinished {
        turn_id: RequestId,
        message_id: RequestId,
    },
    Output {
        turn_id: RequestId,
        text: String,
    },
    Input {
        turn_id: RequestId,
        prompt_id: RequestId,
        question: String,
        choices: Vec<String>,
    },
    Completed {
        turn_id: RequestId,
        status: TurnStatus,
    },
    ToolObservation {
        scope: ToolScope,
        event: ToolObservation,
    },
    UsageObservation {
        scope: UnattributedScope,
        provenance: UsageProvenance,
        accounting: UsageAccounting,
        usage: Usage,
    },
    ObservationGap {
        scope: UnattributedScope,
        source_type: String,
        reason: GapReason,
        #[serde(
            default,
            deserialize_with = "deserialize_present",
            skip_serializing_if = "Option::is_none"
        )]
        tool_call_id: Option<String>,
    },
}

const SDK_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ToolScope {
    ExplicitSdkTurn,
    TurnUnattributed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum UnattributedScope {
    TurnUnattributed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum UsageProvenance {
    SdkReported,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum UsageAccounting {
    #[serde(rename = "UNKNOWN")]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum GapReason {
    UnsupportedEvent,
    UnqualifiedProgress,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum AgentMode {
    Worker,
    Rlm,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ChildRunning {
    Running,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ChildResult {
    Done,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum DetailKind {
    Added,
    Removed,
    Neutral,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all_fields = "camelCase", deny_unknown_fields)]
enum ToolObservation {
    #[serde(rename = "tool:requested")]
    Requested {
        turn_id: String,
        calls: Vec<ToolCall>,
    },
    #[serde(rename = "tool:pending")]
    Pending {
        tool_call_id: String,
        name: String,
        input_summary: String,
    },
    #[serde(rename = "tool:start")]
    Start {
        tool_call_id: String,
        name: String,
        input_summary: String,
        #[serde(
            default,
            deserialize_with = "deserialize_present",
            skip_serializing_if = "Option::is_none"
        )]
        agent_mode: Option<AgentMode>,
    },
    #[serde(rename = "tool:retracted")]
    Retracted { tool_call_id: String },
    #[serde(rename = "tool:result")]
    Result {
        tool_call_id: String,
        name: String,
        title: String,
        #[serde(
            default,
            deserialize_with = "deserialize_present",
            skip_serializing_if = "Option::is_none"
        )]
        detail: Option<String>,
        #[serde(
            default,
            deserialize_with = "deserialize_present",
            skip_serializing_if = "Option::is_none"
        )]
        detail_lines: Option<Vec<DetailLine>>,
        #[serde(
            default,
            deserialize_with = "deserialize_present",
            skip_serializing_if = "Option::is_none"
        )]
        agent_output: Option<Box<AgentOutput>>,
    },
    #[serde(rename = "tool:error")]
    Error {
        tool_call_id: String,
        name: String,
        error: String,
    },
    #[serde(rename = "agent:child_tool_start")]
    ChildStart {
        agent_tool_call_id: String,
        call: ChildCall,
    },
    #[serde(rename = "agent:child_tool_result")]
    ChildResult {
        agent_tool_call_id: String,
        child_tool_call_id: String,
        status: ChildResult,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ToolCall {
    id: String,
    name: String,
    input: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChildCall {
    id: String,
    name: String,
    input_summary: String,
    status: ChildRunning,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DetailLine {
    key: String,
    display_value: String,
    highlighted: bool,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    kind: Option<DetailKind>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentOutput {
    mode: AgentMode,
    report: String,
    status: String,
    rounds: u64,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    agent: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    trace_path: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    run_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    log_path: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    children: Option<Vec<AgentChild>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentChild {
    agent: String,
    status: String,
    rounds: u64,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    run_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    children: Option<Vec<AgentChild>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Usage {
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    input_tokens: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    output_tokens: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    total_tokens: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    cached_tokens: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    cache_write_tokens: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    cost_usd: Option<Number>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    context_tokens: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    context_limit: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    model: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    provider: Option<String>,
}

fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn observation_error() -> ProtocolError {
    ProtocolError("invalid host observation".into())
}

fn optional_text(value: &str, maximum: usize) -> Result<(), ProtocolError> {
    if value.len() > maximum {
        return Err(observation_error());
    }
    Ok(())
}

fn safe_counter(value: u64) -> Result<(), ProtocolError> {
    if value > SDK_SAFE_INTEGER {
        return Err(observation_error());
    }
    Ok(())
}

impl Usage {
    fn validate(&self) -> Result<(), ProtocolError> {
        for counter in [
            self.input_tokens,
            self.output_tokens,
            self.total_tokens,
            self.cached_tokens,
            self.cache_write_tokens,
            self.context_tokens,
            self.context_limit,
        ]
        .into_iter()
        .flatten()
        {
            safe_counter(counter)?;
        }
        if let Some(cost) = &self.cost_usd {
            let value = cost.as_f64().ok_or_else(observation_error)?;
            if !value.is_finite() || !(0.0..=SDK_SAFE_INTEGER as f64).contains(&value) {
                return Err(observation_error());
            }
        }
        for text in [&self.model, &self.provider].into_iter().flatten() {
            optional_text(text, 1024)?;
        }
        Ok(())
    }
}

impl AgentChild {
    fn validate(&self) -> Result<(), ProtocolError> {
        optional_text(&self.agent, 4096)?;
        bounded_text(&self.status, 256)?;
        safe_counter(self.rounds)?;
        if let Some(value) = &self.run_id {
            optional_text(value, 4096)?;
        }
        validate_children(&self.children)
    }
}

fn validate_children(children: &Option<Vec<AgentChild>>) -> Result<(), ProtocolError> {
    if let Some(children) = children {
        if children.len() > 32 {
            return Err(observation_error());
        }
        for child in children {
            child.validate()?;
        }
    }
    Ok(())
}

impl AgentOutput {
    fn validate(&self) -> Result<(), ProtocolError> {
        optional_text(&self.report, 16384)?;
        bounded_text(&self.status, 256)?;
        safe_counter(self.rounds)?;
        for text in [&self.agent, &self.trace_path, &self.run_id, &self.log_path]
            .into_iter()
            .flatten()
        {
            optional_text(text, 4096)?;
        }
        validate_children(&self.children)
    }
}

impl ToolObservation {
    fn validate(&self, scope: &ToolScope) -> Result<(), ProtocolError> {
        if matches!(self, Self::Requested { .. }) != (*scope == ToolScope::ExplicitSdkTurn) {
            return Err(observation_error());
        }
        let value = serde_json::to_value(self).map_err(|_| observation_error())?;
        ObservationBudget::default().visit(&value, 0)?;
        match self {
            Self::Requested { turn_id, calls } => {
                bounded_text(turn_id, 256)?;
                if calls.is_empty() || calls.len() > 32 {
                    return Err(observation_error());
                }
                for call in calls {
                    bounded_text(&call.id, 256)?;
                    bounded_text(&call.name, 1024)?;
                }
            }
            Self::Pending {
                tool_call_id,
                name,
                input_summary,
            }
            | Self::Start {
                tool_call_id,
                name,
                input_summary,
                ..
            } => {
                bounded_text(tool_call_id, 256)?;
                bounded_text(name, 1024)?;
                optional_text(input_summary, 16384)?;
            }
            Self::Retracted { tool_call_id } => bounded_text(tool_call_id, 256)?,
            Self::Result {
                tool_call_id,
                name,
                title,
                detail,
                detail_lines,
                agent_output,
            } => {
                bounded_text(tool_call_id, 256)?;
                bounded_text(name, 1024)?;
                optional_text(title, 16384)?;
                if let Some(detail) = detail {
                    optional_text(detail, 16384)?;
                }
                if let Some(lines) = detail_lines {
                    if lines.len() > 32 {
                        return Err(observation_error());
                    }
                    for line in lines {
                        optional_text(&line.key, 1024)?;
                        optional_text(&line.display_value, 16384)?;
                    }
                }
                if let Some(output) = agent_output {
                    output.validate()?;
                }
            }
            Self::Error {
                tool_call_id,
                name,
                error,
            } => {
                bounded_text(tool_call_id, 256)?;
                bounded_text(name, 1024)?;
                optional_text(error, 16384)?;
            }
            Self::ChildStart {
                agent_tool_call_id,
                call,
            } => {
                bounded_text(agent_tool_call_id, 256)?;
                bounded_text(&call.id, 256)?;
                bounded_text(&call.name, 1024)?;
                optional_text(&call.input_summary, 16384)?;
            }
            Self::ChildResult {
                agent_tool_call_id,
                child_tool_call_id,
                ..
            } => {
                bounded_text(agent_tool_call_id, 256)?;
                bounded_text(child_tool_call_id, 256)?;
            }
        }
        Ok(())
    }
}

fn validate_gap(
    source: &str,
    reason: &GapReason,
    tool_id: &Option<String>,
) -> Result<(), ProtocolError> {
    bounded_text(source, 128)?;
    match reason {
        GapReason::UnqualifiedProgress if source == "tool:progress" => {
            bounded_text(tool_id.as_deref().ok_or_else(observation_error)?, 256)
        }
        GapReason::UnsupportedEvent
            if tool_id.is_none()
                && !matches!(
                    source,
                    "usage"
                        | "tool:requested"
                        | "tool:pending"
                        | "tool:start"
                        | "tool:retracted"
                        | "tool:result"
                        | "tool:error"
                        | "agent:child_tool_start"
                        | "agent:child_tool_result"
                        | "tool:progress"
                ) =>
        {
            Ok(())
        }
        _ => Err(observation_error()),
    }
}

#[derive(Default)]
struct ObservationBudget {
    nodes: usize,
    bytes: usize,
}

impl ObservationBudget {
    fn charge(&mut self, bytes: usize) -> Result<(), ProtocolError> {
        assert!(self.bytes <= 32768);
        if bytes > 32768 - self.bytes {
            return Err(observation_error());
        }
        self.bytes += bytes;
        Ok(())
    }

    fn visit(&mut self, value: &Value, depth: usize) -> Result<(), ProtocolError> {
        assert!(self.nodes <= 1024);
        assert!(self.bytes <= 32768);
        assert!(depth <= 9);
        if self.nodes == 1024 || depth > 8 {
            return Err(observation_error());
        }
        self.nodes += 1;
        match value {
            Value::Null => self.charge(4),
            Value::Bool(value) => self.charge(if *value { 4 } else { 5 }),
            Value::String(value) => {
                optional_text(value, 16384)?;
                self.charge(quoted_json_bytes(value))
            }
            Value::Number(value) => self.charge(json_number_bytes(value)?),
            Value::Array(values) => {
                if values.len() > 64 {
                    return Err(observation_error());
                }
                self.charge(2 + values.len().saturating_sub(1))?;
                for value in values {
                    self.visit(value, depth + 1)?;
                }
                Ok(())
            }
            Value::Object(values) => {
                if values.len() > 32 {
                    return Err(observation_error());
                }
                self.charge(2 + values.len().saturating_sub(1))?;
                for (key, value) in values {
                    optional_text(key, 256)?;
                    self.charge(quoted_json_bytes(key) + 1)?;
                    self.visit(value, depth + 1)?;
                }
                Ok(())
            }
        }
    }
}

fn quoted_json_bytes(value: &str) -> usize {
    2 + value
        .bytes()
        .map(|byte| match byte {
            b'"' | b'\\' | 8 | 9 | 10 | 12 | 13 => 2,
            0..=31 => 6,
            _ => 1,
        })
        .sum::<usize>()
}

fn json_number_bytes(value: &Number) -> Result<usize, ProtocolError> {
    let number = value.as_f64().ok_or_else(observation_error)?;
    if !number.is_finite() || number.abs() > SDK_SAFE_INTEGER as f64 {
        return Err(observation_error());
    }
    if number == 0.0 {
        return Ok(1);
    }
    let magnitude = number.abs();
    let bytes = if magnitude < 0.000001 {
        format!("{magnitude:e}").len()
    } else {
        magnitude.to_string().len()
    };
    Ok(bytes + usize::from(number.is_sign_negative()))
}

struct FrameSeed<'a> {
    nodes: &'a mut usize,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for FrameSeed<'_> {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        assert!(*self.nodes <= 1032);
        if self.depth > 10 || *self.nodes == 1032 {
            return Err(D::Error::custom("host event JSON bounds exceeded"));
        }
        *self.nodes += 1;
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for FrameSeed<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded host event JSON without duplicate keys")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid host number"))
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
        if value.len() > 16384 {
            return Err(E::custom("host event string exceeds limit"));
        }
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Value, E> {
        if value.len() > 16384 {
            return Err(E::custom("host event string exceeds limit"));
        }
        Ok(Value::String(value))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(FrameSeed {
            nodes: &mut *self.nodes,
            depth: self.depth + 1,
        })? {
            if values.len() == 64 {
                return Err(A::Error::custom("host event array exceeds limit"));
            }
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if key.len() > 256 || values.len() == 32 || values.contains_key(&key) {
                return Err(A::Error::custom("invalid host event object key"));
            }
            let value = map.next_value_seed(FrameSeed {
                nodes: &mut *self.nodes,
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TurnStatus {
    Succeeded,
    Cancelled,
    Failed,
}

impl RawEvent {
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol_version != HOST_PROTOCOL_VERSION || self.sequence == 0 {
            return Err(ProtocolError(
                "invalid host event version or sequence".into(),
            ));
        }
        match &self.event {
            EventBody::Output { text, .. } | EventBody::MessageDelta { text, .. } => {
                bounded_text(text, 16384)?;
            }
            EventBody::Input {
                question, choices, ..
            } => {
                bounded_text(question, 4096)?;
                if choices.len() > 8 {
                    return Err(ProtocolError("host choice count out of bounds".into()));
                }
                for choice in choices {
                    bounded_text(choice, 1024)?;
                }
            }
            EventBody::ToolObservation { scope, event } => event.validate(scope)?,
            EventBody::UsageObservation { usage, .. } => usage.validate()?,
            EventBody::ObservationGap {
                source_type,
                reason,
                tool_call_id,
                ..
            } => {
                validate_gap(source_type, reason, tool_call_id)?;
            }
            EventBody::Ready {}
            | EventBody::MessageStarted { .. }
            | EventBody::MessageReset { .. }
            | EventBody::MessageDiscarded { .. }
            | EventBody::MessageFinished { .. }
            | EventBody::Completed { .. } => {}
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut nodes = 0;
        let value = FrameSeed {
            nodes: &mut nodes,
            depth: 0,
        }
        .deserialize(deserializer)?;
        let raw =
            RawEvent::deserialize(value).map_err(|_| D::Error::custom("invalid host event"))?;
        raw.validate().map_err(D::Error::custom)?;
        Ok(Self(raw))
    }
}

#[derive(Debug)]
pub struct EventSequence {
    session_id: SessionId,
    instance_id: RequestId,
    last: u64,
}

impl EventSequence {
    pub fn new(session_id: SessionId, instance_id: RequestId) -> Self {
        Self {
            session_id,
            instance_id,
            last: 0,
        }
    }

    pub fn last_sequence(&self) -> u64 {
        self.last
    }

    pub fn accept(&mut self, event: &Event) -> Result<(), ProtocolError> {
        let value = &event.0;
        value.validate()?;
        if value.session_id != self.session_id || value.instance_id != self.instance_id {
            return Err(ProtocolError("host stream scope mismatch".into()));
        }
        let next = self
            .last
            .checked_add(1)
            .ok_or_else(|| ProtocolError("host sequence exhausted".into()))?;
        let ready = matches!(value.event, EventBody::Ready {});
        if value.sequence != next || ready != (self.last == 0) {
            return Err(ProtocolError("host stream order mismatch".into()));
        }
        self.last = next;
        Ok(())
    }
}

pub fn encode_control(value: &Control) -> Result<Vec<u8>, ProtocolError> {
    value.0.validate()?;
    encode_frame(value)
}

pub fn read_event(reader: &mut impl Read) -> Result<Event, ProtocolError> {
    let mut header = [0; 4];
    reader
        .read_exact(&mut header)
        .map_err(|_| ProtocolError("incomplete host event header".into()))?;
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > FRAME_BYTES_MAX {
        return Err(ProtocolError("invalid host event length".into()));
    }
    let mut payload = vec![0; size];
    reader
        .read_exact(&mut payload)
        .map_err(|_| ProtocolError("incomplete host event body".into()))?;
    decode_event_payload(&payload, size)
}

fn decode_event_payload(payload: &[u8], declared: usize) -> Result<Event, ProtocolError> {
    assert_eq!(payload.len(), declared);
    assert!(!payload.is_empty());
    assert!(payload.len() <= FRAME_BYTES_MAX);
    serde_json::from_slice(payload).map_err(|_| ProtocolError("invalid host event body".into()))
}

fn bounded_text(value: &str, maximum: usize) -> Result<(), ProtocolError> {
    if value.is_empty() || value.len() > maximum {
        return Err(ProtocolError("host text byte count out of bounds".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn observation_budget_invariants_have_real_tripping_checks() {
        for (nodes, bytes, depth) in [(1025, 0, 0), (0, 32769, 0), (0, 0, 10)] {
            assert!(
                std::panic::catch_unwind(|| {
                    ObservationBudget { nodes, bytes }.visit(&json!(0), depth)
                })
                .is_err()
            );
        }
        assert!(
            std::panic::catch_unwind(|| {
                ObservationBudget {
                    nodes: 0,
                    bytes: 32769,
                }
                .charge(0)
            })
            .is_err()
        );
        assert!(
            std::panic::catch_unwind(|| {
                let mut nodes = 1033;
                FrameSeed {
                    nodes: &mut nodes,
                    depth: 0,
                }
                .deserialize(json!(0))
            })
            .is_err()
        );
    }

    #[test]
    fn compact_json_bytes_match_javascript_escape_and_number_forms() {
        assert_eq!(quoted_json_bytes("\"\\\n\u{0000}é"), 16);
        for (source, expected) in [
            ("0.0", 1),
            ("-0.0", 1),
            ("1.0", 1),
            ("1e-6", 8),
            ("1e-7", 4),
            ("-1e-7", 5),
            ("0.0125", 6),
            ("9007199254740991", 16),
            ("5e-324", 6),
        ] {
            let value: Number = serde_json::from_str(source).unwrap();
            assert_eq!(json_number_bytes(&value).unwrap(), expected, "{source}");
        }
    }

    #[test]
    fn complete_body_preconditions_have_real_tripping_checks() {
        let bytes = serde_json::to_vec(&json!({
            "protocol_version":1,"session_id":"session-a","instance_id":"instance-a",
            "sequence":1,"event":{"kind":"ready"}
        }))
        .unwrap();
        assert!(decode_event_payload(&bytes, bytes.len()).is_ok());
        for (body, declared) in [
            (bytes.clone(), bytes.len() + 1),
            (Vec::new(), 0),
            (vec![0; FRAME_BYTES_MAX + 1], FRAME_BYTES_MAX + 1),
        ] {
            assert!(std::panic::catch_unwind(|| decode_event_payload(&body, declared)).is_err());
        }
    }

    #[test]
    fn outbound_control_rechecks_internal_construction_and_stream_overflow_is_rejected() {
        let mut raw: RawControl = serde_json::from_value(json!({
            "protocol_version":1,"session_id":"session-a","request_id":"request-a",
            "control":{"kind":"prompt","turn_id":"turn-a","text":"Valid"}
        }))
        .unwrap();
        assert!(encode_control(&Control(raw.clone())).is_ok());
        raw.protocol_version = 2;
        assert!(encode_control(&Control(raw)).is_err());
        let value: Event = serde_json::from_value(json!({
            "protocol_version":1,"session_id":"session-a","instance_id":"instance-a",
            "sequence":u64::MAX,
            "event":{"kind":"completed","turn_id":"turn-a","status":"failed"}
        }))
        .unwrap();
        let mut stream =
            EventSequence::new(value.0.session_id.clone(), value.0.instance_id.clone());
        stream.last = u64::MAX;
        assert!(stream.accept(&value).is_err());
        assert_eq!(stream.last_sequence(), u64::MAX);
    }
}
