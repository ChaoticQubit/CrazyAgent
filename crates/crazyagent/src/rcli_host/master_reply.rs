use super::master::{RcliMasterCompletion, RcliMasterStatus};
use super::master_launch::error;
use super::{Event, EventBody, TurnStatus};
use crate::protocol::{ProtocolError, RequestId};
use crate::usage::capture::Tokens;
use std::collections::BTreeMap;
use std::time::Instant;

/// The bytes one turn's capture keeps: the text of its messages and one entry for each
/// message. No count of messages and no total of frames ends a turn (OB-5 r2); a turn that
/// would keep more ends with the output limit code. It is the size of the engine's capture
/// file for one turn.
const KEPT_BYTES_MAX: usize = 8 * 1024 * 1024;

/// What the entry of one message costs beside its id and its text.
const MESSAGE_ENTRY_BYTES: usize = 64;

struct Message {
    text: String,
    finished: bool,
    discarded: bool,
}

pub(super) struct TurnCapture {
    pub(super) id: RequestId,
    /// When a cancel the host asked for must have ended; none until a cancel. A turn has no
    /// clock of its own: its host's run clock, the master time limit, bounds it (OB-5d).
    pub(super) deadline: Option<Instant>,
    pub(super) cancelling: bool,
    messages: Vec<Message>,
    index: BTreeMap<RequestId, usize>,
    text_bytes: usize,
    entry_bytes: usize,
    usage: Tokens,
    usage_through: u64,
}

impl TurnCapture {
    pub(super) fn new(id: RequestId) -> Self {
        let capture = Self {
            id,
            deadline: None,
            cancelling: false,
            messages: Vec::new(),
            index: BTreeMap::new(),
            text_bytes: 0,
            entry_bytes: 0,
            usage: Tokens::default(),
            usage_through: 0,
        };
        assert!(capture.messages.is_empty());
        assert_eq!(capture.kept(), 0);
        assert!(capture.deadline.is_none());
        capture
    }

    /// Takes one frame of the turn into the capture. Frames pass through uncounted: only
    /// what the capture keeps is bounded, by `KEPT_BYTES_MAX`. Inputs: the event and the
    /// bytes of its frame. Output: the completion once the turn ended, none before, or a
    /// plain code.
    pub(super) fn accept(
        &mut self,
        event: &Event,
        frame_bytes: usize,
    ) -> Result<Option<RcliMasterCompletion>, ProtocolError> {
        assert_eq!(self.index.len(), self.messages.len());
        assert!(self.kept() <= KEPT_BYTES_MAX);
        assert!(frame_bytes > 4);
        assert!(frame_bytes <= crate::protocol::FRAME_BYTES_MAX + 4);
        match &event.0.event {
            EventBody::MessageStarted {
                turn_id,
                message_id,
            } => {
                self.scope(turn_id)?;
                self.start(message_id)?;
            }
            EventBody::MessageDelta {
                turn_id,
                message_id,
                text,
            } => self.delta(turn_id, message_id, text)?,
            EventBody::MessageReset {
                turn_id,
                message_id,
            } => self.reset(turn_id, message_id, false)?,
            EventBody::MessageDiscarded {
                turn_id,
                message_id,
            } => self.reset(turn_id, message_id, true)?,
            EventBody::MessageFinished {
                turn_id,
                message_id,
            } => {
                let message = self.message(turn_id, message_id)?;
                if message.finished || message.discarded {
                    return Err(error("rcli_master_message_order"));
                }
                message.finished = true;
            }
            EventBody::Completed { turn_id, status } => {
                self.scope(turn_id)?;
                let reply = if matches!(status, TurnStatus::Succeeded) {
                    Some(self.reply()?)
                } else {
                    None
                };
                let status = match status {
                    TurnStatus::Succeeded => RcliMasterStatus::Succeeded,
                    TurnStatus::Cancelled => RcliMasterStatus::Cancelled,
                    TurnStatus::Failed => RcliMasterStatus::Failed,
                };
                return Ok(Some(RcliMasterCompletion {
                    turn_id: turn_id.clone(),
                    status,
                    reply,
                }));
            }
            EventBody::Input { turn_id, .. } => {
                self.scope(turn_id)?;
                return Err(error("rcli_master_input_required"));
            }
            EventBody::Output { turn_id, .. } => self.scope(turn_id)?,
            EventBody::Ready {} => return Err(error("rcli_master_message_order")),
            EventBody::UsageObservation { usage, .. } if event.0.sequence > self.usage_through => {
                self.usage_through = event.0.sequence;
                self.usage = self.usage.plus(usage.tokens());
            }
            _ => {}
        }
        assert!(self.kept() <= KEPT_BYTES_MAX);
        Ok(None)
    }

    /// The bytes the capture keeps now: the text of its messages and their entries.
    /// Input: the capture. Output: the bytes.
    fn kept(&self) -> usize {
        self.text_bytes + self.entry_bytes
    }

    /// Opens a message of the turn, however many came before it. Input: the message id.
    /// Output: Ok, the order code for an id the turn already used, or the output limit code
    /// when its entry no longer fits the kept bytes.
    fn start(&mut self, message_id: &RequestId) -> Result<(), ProtocolError> {
        if self.index.contains_key(message_id) {
            return Err(error("rcli_master_message_order"));
        }
        let entry = message_id.as_str().len() + MESSAGE_ENTRY_BYTES;
        if self.kept() + entry > KEPT_BYTES_MAX {
            return Err(error("rcli_master_output_limit"));
        }
        let previous = self.index.insert(message_id.clone(), self.messages.len());
        assert!(previous.is_none());
        self.messages.push(Message {
            text: String::new(),
            finished: false,
            discarded: false,
        });
        self.entry_bytes += entry;
        assert_eq!(self.index.len(), self.messages.len());
        assert!(self.kept() <= KEPT_BYTES_MAX);
        Ok(())
    }

    /// Hands over the tokens the turn's usage frames reported since the last call, each
    /// provider call's frame counted once by its sequence, so a repeated or older frame adds
    /// nothing. Output: the count, zero when no new frame came.
    pub(super) fn take_usage(&mut self) -> Tokens {
        std::mem::take(&mut self.usage)
    }

    fn scope(&self, turn_id: &RequestId) -> Result<(), ProtocolError> {
        if *turn_id != self.id {
            return Err(error("rcli_master_turn_scope"));
        }
        Ok(())
    }

    fn message(
        &mut self,
        turn_id: &RequestId,
        message_id: &RequestId,
    ) -> Result<&mut Message, ProtocolError> {
        self.scope(turn_id)?;
        let at = *self
            .index
            .get(message_id)
            .ok_or_else(|| error("rcli_master_message_order"))?;
        assert!(at < self.messages.len());
        Ok(&mut self.messages[at])
    }

    fn delta(
        &mut self,
        turn_id: &RequestId,
        message_id: &RequestId,
        text: &str,
    ) -> Result<(), ProtocolError> {
        let next = self
            .kept()
            .checked_add(text.len())
            .filter(|bytes| *bytes <= KEPT_BYTES_MAX)
            .ok_or_else(|| error("rcli_master_output_limit"))?;
        let message = self.message(turn_id, message_id)?;
        if message.finished || message.discarded {
            return Err(error("rcli_master_message_order"));
        }
        message.text.push_str(text);
        let grown = message.text.len();
        self.text_bytes += text.len();
        assert_eq!(self.kept(), next);
        assert!(grown <= self.text_bytes);
        Ok(())
    }

    fn reset(
        &mut self,
        turn_id: &RequestId,
        message_id: &RequestId,
        discard: bool,
    ) -> Result<(), ProtocolError> {
        let message = self.message(turn_id, message_id)?;
        if message.discarded || !discard && message.finished {
            return Err(error("rcli_master_message_order"));
        }
        let removed = std::mem::take(&mut message.text).len();
        message.discarded = discard;
        assert!(removed <= self.text_bytes);
        self.text_bytes -= removed;
        assert!(self.kept() <= KEPT_BYTES_MAX);
        Ok(())
    }

    fn reply(&self) -> Result<String, ProtocolError> {
        let mut reply = String::new();
        for message in self.messages.iter().filter(|message| !message.discarded) {
            if !message.finished {
                return Err(error("rcli_master_capture_incomplete"));
            }
            if message.text.is_empty() {
                continue;
            }
            let separator = if reply.is_empty() { "" } else { "\n\n" };
            reply.push_str(separator);
            reply.push_str(&message.text);
        }
        if reply.trim().is_empty() {
            return Err(error("rcli_master_empty_reply"));
        }
        assert!(!reply.is_empty());
        assert!(reply.len() <= self.text_bytes + 2 * self.messages.len());
        Ok(reply)
    }
}

pub(super) fn has_turn(event: &Event) -> bool {
    matches!(
        event.0.event,
        EventBody::MessageStarted { .. }
            | EventBody::MessageDelta { .. }
            | EventBody::MessageReset { .. }
            | EventBody::MessageDiscarded { .. }
            | EventBody::MessageFinished { .. }
            | EventBody::Output { .. }
            | EventBody::Input { .. }
            | EventBody::Completed { .. }
    )
}
