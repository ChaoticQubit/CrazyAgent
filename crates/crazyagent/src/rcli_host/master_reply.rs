use super::master::{RcliMasterCompletion, RcliMasterStatus};
use super::master_launch::error;
use super::{Event, EventBody, TurnStatus};
use crate::protocol::{ProtocolError, RequestId};
use std::time::{Duration, Instant};

const REPLY_BYTES_MAX: usize = 1_048_576;

struct Message {
    id: RequestId,
    text: String,
    finished: bool,
    discarded: bool,
}

pub(super) struct TurnCapture {
    pub(super) id: RequestId,
    pub(super) deadline: Instant,
    pub(super) cancelling: bool,
    messages: Vec<Message>,
    retained_bytes: usize,
    observed_bytes: usize,
}

impl TurnCapture {
    pub(super) fn new(id: RequestId) -> Self {
        let capture = Self {
            id, deadline: Instant::now() + Duration::from_secs(120), cancelling: false,
            messages: Vec::new(), retained_bytes: 0, observed_bytes: 0,
        };
        assert!(capture.messages.is_empty());
        assert_eq!(capture.retained_bytes, 0);
        capture
    }

    pub(super) fn accept(&mut self, event: &Event, frame_bytes: usize) -> Result<Option<RcliMasterCompletion>, ProtocolError> {
        assert!(self.messages.len() <= 128);
        assert!(self.retained_bytes <= REPLY_BYTES_MAX);
        assert!(frame_bytes > 4);
        assert!(frame_bytes <= crate::protocol::FRAME_BYTES_MAX + 4);
        self.observed_bytes += frame_bytes;
        if self.observed_bytes > REPLY_BYTES_MAX { return Err(error("rcli_master_output_limit")); }
        match &event.0.event {
            EventBody::MessageStarted { turn_id, message_id } => {
                self.scope(turn_id)?;
                if self.messages.len() == 128 || self.messages.iter().any(|message| message.id == *message_id) {
                    return Err(error("rcli_master_message_order"));
                }
                self.messages.push(Message { id: message_id.clone(), text: String::new(), finished: false, discarded: false });
            }
            EventBody::MessageDelta { turn_id, message_id, text } => self.delta(turn_id, message_id, text)?,
            EventBody::MessageReset { turn_id, message_id } => self.reset(turn_id, message_id, false)?,
            EventBody::MessageDiscarded { turn_id, message_id } => self.reset(turn_id, message_id, true)?,
            EventBody::MessageFinished { turn_id, message_id } => {
                let message = self.message(turn_id, message_id)?;
                if message.finished || message.discarded { return Err(error("rcli_master_message_order")); }
                message.finished = true;
            }
            EventBody::Completed { turn_id, status } => {
                self.scope(turn_id)?;
                let reply = if matches!(status, TurnStatus::Succeeded) { Some(self.reply()?) } else { None };
                let status = match status {
                    TurnStatus::Succeeded => RcliMasterStatus::Succeeded,
                    TurnStatus::Cancelled => RcliMasterStatus::Cancelled,
                    TurnStatus::Failed => RcliMasterStatus::Failed,
                };
                return Ok(Some(RcliMasterCompletion { turn_id: turn_id.clone(), status, reply }));
            }
            EventBody::Input { turn_id, .. } => {
                self.scope(turn_id)?;
                return Err(error("rcli_master_input_required"));
            }
            EventBody::Output { turn_id, .. } => self.scope(turn_id)?,
            EventBody::Ready {} => return Err(error("rcli_master_message_order")),
            _ => {}
        }
        assert!(self.retained_bytes <= REPLY_BYTES_MAX);
        Ok(None)
    }

    fn scope(&self, turn_id: &RequestId) -> Result<(), ProtocolError> {
        if *turn_id != self.id { return Err(error("rcli_master_turn_scope")); }
        Ok(())
    }

    fn message(&mut self, turn_id: &RequestId, message_id: &RequestId) -> Result<&mut Message, ProtocolError> {
        self.scope(turn_id)?;
        self.messages.iter_mut().find(|message| message.id == *message_id)
            .ok_or_else(|| error("rcli_master_message_order"))
    }

    fn delta(&mut self, turn_id: &RequestId, message_id: &RequestId, text: &str) -> Result<(), ProtocolError> {
        let next = self.retained_bytes.checked_add(text.len())
            .filter(|bytes| *bytes <= REPLY_BYTES_MAX)
            .ok_or_else(|| error("rcli_master_output_limit"))?;
        let message = self.message(turn_id, message_id)?;
        if message.finished || message.discarded { return Err(error("rcli_master_message_order")); }
        message.text.push_str(text);
        self.retained_bytes = next;
        assert!(self.retained_bytes <= REPLY_BYTES_MAX);
        assert_eq!(self.retained_bytes, self.messages.iter().map(|message| message.text.len()).sum::<usize>());
        Ok(())
    }

    fn reset(&mut self, turn_id: &RequestId, message_id: &RequestId, discard: bool) -> Result<(), ProtocolError> {
        let message = self.message(turn_id, message_id)?;
        if message.discarded || !discard && message.finished { return Err(error("rcli_master_message_order")); }
        let removed = message.text.len();
        message.text.clear();
        message.discarded = discard;
        self.retained_bytes -= removed;
        assert!(self.retained_bytes <= REPLY_BYTES_MAX);
        assert_eq!(self.retained_bytes, self.messages.iter().map(|message| message.text.len()).sum::<usize>());
        Ok(())
    }

    fn reply(&self) -> Result<String, ProtocolError> {
        let mut reply = String::new();
        for message in self.messages.iter().filter(|message| !message.discarded) {
            if !message.finished { return Err(error("rcli_master_capture_incomplete")); }
            if message.text.is_empty() { continue; }
            let separator = if reply.is_empty() { "" } else { "\n\n" };
            if reply.len() + separator.len() + message.text.len() > REPLY_BYTES_MAX {
                return Err(error("rcli_master_output_limit"));
            }
            reply.push_str(separator);
            reply.push_str(&message.text);
        }
        if reply.trim().is_empty() { return Err(error("rcli_master_empty_reply")); }
        assert!(!reply.is_empty());
        assert!(reply.len() <= REPLY_BYTES_MAX);
        Ok(reply)
    }
}

pub(super) fn has_turn(event: &Event) -> bool {
    matches!(event.0.event,
        EventBody::MessageStarted { .. } | EventBody::MessageDelta { .. }
        | EventBody::MessageReset { .. } | EventBody::MessageDiscarded { .. }
        | EventBody::MessageFinished { .. } | EventBody::Output { .. }
        | EventBody::Input { .. } | EventBody::Completed { .. }
    )
}
