use crate::protocol::{FRAME_BYTES_MAX, ProtocolError, RequestId, SessionId, encode_frame};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
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
        let raw = RawEvent::deserialize(deserializer)
            .map_err(|_| D::Error::custom("invalid host event"))?;
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
