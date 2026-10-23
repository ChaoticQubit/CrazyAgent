use super::{Context, State};
use crate::engine::owner::{AccountSecret, ClaudeAuthChallenge, ClaudeAuthInput, NativeAuthQuery};
use crate::tui::app::UiEvent;
use std::fmt;

const INPUT_BYTES_MAX: usize = 8192;

#[derive(Default)]
struct Buffer(Vec<u8>);

impl Buffer {
    fn push(&mut self, character: char) -> Result<(), String> {
        if character.is_control() {
            return Err("Claude code must contain printable characters".into());
        }
        let mut bytes = [0; 4];
        let encoded = character.encode_utf8(&mut bytes);
        if self.0.len() + encoded.len() > INPUT_BYTES_MAX {
            bytes.fill(0);
            return Err("Claude code exceeds the private input limit".into());
        }
        self.0.extend_from_slice(encoded.as_bytes());
        bytes.fill(0);
        Ok(())
    }

    fn pop(&mut self) {
        let Some(index) = self.0.iter().rposition(|byte| byte & 0xc0 != 0x80) else {
            return;
        };
        self.0[index..].fill(0);
        self.0.truncate(index);
    }

    fn clear(&mut self) {
        self.0.fill(0);
        self.0.clear();
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.clear();
    }
}

impl fmt::Debug for Buffer {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("ClaudeCodeBuffer([REDACTED])")
    }
}

struct Observation {
    request: ClaudeAuthInput,
    challenge: ClaudeAuthChallenge,
}

pub(in crate::tui) struct Submission {
    pub request: ClaudeAuthInput,
    pub secret: AccountSecret,
}

pub(in crate::tui) struct Job {
    pub context: Context,
    pub submission: Submission,
}

pub(in crate::tui) struct Event {
    pub context: Context,
    pub request: ClaudeAuthInput,
    pub result: Result<(), String>,
}

#[derive(Default)]
pub(super) struct Input {
    observation: Option<Observation>,
    value: Buffer,
    queued: Option<Submission>,
    submitted: Option<ClaudeAuthInput>,
    pending: bool,
    notice: &'static str,
}

impl Input {
    pub(super) fn observe(
        &mut self,
        query: NativeAuthQuery,
        generation: u64,
        challenge: ClaudeAuthChallenge,
    ) -> Result<(), String> {
        if generation == 0 || challenge.validate().is_err() {
            self.clear();
            return Err("Claude challenge context is invalid".into());
        }
        let request = ClaudeAuthInput {
            query,
            generation,
            prompt_id: challenge.prompt_id().unwrap_or(0),
        };
        if let Some(observed) = &self.observation
            && same_prompt(&observed.request, &request)
        {
            if observed.challenge.url() != challenge.url() {
                self.clear();
                return Err("Claude challenge changed; read the original operation".into());
            }
            return Ok(());
        }
        self.clear();
        self.observation = Some(Observation { request, challenge });
        Ok(())
    }

    pub(super) fn reduce(&mut self, event: &UiEvent) -> Result<bool, String> {
        if matches!(event, UiEvent::Cancel | UiEvent::FocusNext | UiEvent::FocusPrevious) {
            let observed = self.observation.is_some();
            self.clear();
            return Ok(observed);
        }
        if !self.accepts_code() {
            return Ok(false);
        }
        match event {
            UiEvent::Character(character) => {
                if !self.spent() {
                    self.value.push(*character)?;
                }
            }
            UiEvent::Backspace => {
                if !self.spent() {
                    self.value.pop();
                }
            }
            UiEvent::Submit => self.submit()?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn submit(&mut self) -> Result<(), String> {
        if self.spent() || self.value.0.is_empty() {
            return Ok(());
        }
        assert!(self.queued.is_none() && !self.pending);
        let request = self
            .observation
            .as_ref()
            .expect("observed prompt required")
            .request
            .clone();
        let secret = AccountSecret::new(std::mem::take(&mut self.value.0))
            .map_err(|_| "Claude private input is invalid")?;
        self.submitted = Some(request.clone());
        self.queued = Some(Submission { request, secret });
        self.pending = true;
        self.notice = "Code submission pending; it will not be replayed";
        Ok(())
    }

    pub(super) fn request(&mut self) -> Option<Submission> {
        self.queued.take()
    }

    pub(super) fn complete(
        &mut self,
        request: &ClaudeAuthInput,
        result: Result<(), String>,
    ) -> bool {
        if !self.pending
            || self.submitted.as_ref().is_none_or(|sent| !same_request(sent, request))
        {
            return false;
        }
        self.pending = false;
        self.notice = if result.is_ok() {
            "Owner accepted code; read the original operation for actual sign-in progress"
        } else {
            "Code outcome unavailable; read the original operation; code will not be replayed"
        };
        true
    }

    pub(super) fn clear(&mut self) {
        self.value.clear();
        self.queued = None;
        self.observation = None;
        self.pending = false;
        self.notice = "";
    }

    pub(super) fn accepts_code(&self) -> bool {
        self.observation.as_ref().is_some_and(|observed| observed.challenge.accepts_code())
    }

    pub(super) fn loading(&self) -> bool {
        self.pending
    }

    fn spent(&self) -> bool {
        self.observation.as_ref().is_some_and(|observed| {
            self.submitted.as_ref().is_some_and(|sent| same_prompt(sent, &observed.request))
        })
    }

    pub(super) fn render(&self) -> String {
        let Some(observed) = &self.observation else {
            return String::new();
        };
        let mut text = format!("Claude browser sign-in\n{}", observed.challenge.url());
        if observed.challenge.accepts_code() {
            text.push_str("\nClaude fallback code: ");
            text.push_str(if self.spent() {
                "[submitted once]"
            } else if self.value.0.is_empty() {
                "[empty]"
            } else {
                "[hidden input]"
            });
            text.push_str("\nEnter submits once | Down selects read/cancel controls");
        }
        if !self.notice.is_empty() {
            text.push('\n');
            text.push_str(self.notice);
        }
        text
    }
}

fn same_prompt(left: &ClaudeAuthInput, right: &ClaudeAuthInput) -> bool {
    left.query.operation_id == right.query.operation_id
        && left.query.scope == right.query.scope
        && left.generation == right.generation
        && left.prompt_id == right.prompt_id
}

fn same_request(left: &ClaudeAuthInput, right: &ClaudeAuthInput) -> bool {
    same_prompt(left, right) && left.query.request_id == right.query.request_id
}

impl State {
    pub(in crate::tui) fn code_request(&mut self) -> Option<Job> {
        if !self.open || !self.connected || self.flight.is_some() || self.job.is_some() {
            return None;
        }
        let context = self.context.as_ref()?.clone();
        self.claude_code.request().map(|submission| Job { context, submission })
    }

    pub(in crate::tui) fn apply_code(&mut self, event: Event) -> Result<(), String> {
        if self.context.as_ref() != Some(&event.context) || !self.open || !self.connected {
            return Ok(());
        }
        if self.claude_code.complete(&event.request, event.result) {
            self.enqueue_query(false)?;
        }
        Ok(())
    }

    pub(super) fn observe_claude(&mut self, reply: &super::NativeAuthReply) {
        let Some(challenge) = &reply.claude_challenge else {
            self.claude_code.clear();
            return;
        };
        if reply.fact.phase != super::NativeAuthPhase::AwaitingOwner
            || reply.fact.action != (super::NativeAuthAction::SignIn {
                method: super::NativeAuthMethod::ClaudeOfficial,
            })
        {
            self.claude_code.clear();
            return;
        }
        let query = NativeAuthQuery {
            request_id: reply.request_id.clone(),
            scope: self.context.as_ref().expect("open authentication has context").scope(),
            operation_id: reply.fact.operation_id.clone(),
        };
        let had_prompt = self.claude_code.accepts_code();
        if self.claude_code.observe(query, reply.fact.generation, challenge.clone()).is_err() {
            self.notice = "Claude challenge changed; close and read the original operation";
        } else if !had_prompt && self.claude_code.accepts_code() {
            self.selected = 0;
        }
    }
}
