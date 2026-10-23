use super::types::ClaudeAuthChallenge;
use crate::engine::EngineError;

const BYTES_MAX: usize = 65536;
const PREFIX: &str = "If the browser didn't open, visit: ";
const PROMPT: &str = "Paste code here if prompted > ";

#[derive(Default)]
pub(super) struct Transcript {
    input: Vec<u8>,
    emitted: Option<ClaudeAuthChallenge>,
}

impl Transcript {
    pub(super) fn push(&mut self, bytes: &[u8]) -> Result<Option<ClaudeAuthChallenge>, EngineError> {
        if bytes.len() > BYTES_MAX.saturating_sub(self.input.len()) {
            return Err(EngineError("native_auth_claude_output_bound".into()));
        }
        self.input.extend_from_slice(bytes);
        let plain = presentation(&self.input)?;
        let text = match std::str::from_utf8(&plain) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() => return Ok(None),
            Err(_) => return Err(EngineError("native_auth_claude_output_invalid".into())),
        };
        let Some((_, tail)) = text.split_once(PREFIX) else { return Ok(None) };
        let Some(end) = tail.find(['\r', '\n']) else { return Ok(None) };
        let challenge = ClaudeAuthChallenge::new(tail[..end].into(), tail[end..].contains(PROMPT))?;
        if self.emitted.as_ref().is_some_and(|previous| previous.url() != challenge.url()) {
            return Err(EngineError("native_auth_claude_challenge_changed".into()));
        }
        if self.emitted.as_ref() == Some(&challenge) { return Ok(None) }
        self.emitted = Some(challenge.clone());
        Ok(Some(challenge))
    }
}

fn presentation(input: &[u8]) -> Result<Vec<u8>, EngineError> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] != 0x1b {
            if input[index] < b' ' && !matches!(input[index], b'\r' | b'\n' | b'\t') {
                return Err(EngineError("native_auth_claude_output_invalid".into()));
            }
            output.push(input[index]);
            index += 1;
            continue;
        }
        let Some(kind) = input.get(index + 1) else { break };
        if *kind == b'[' {
            let Some(end) = input[index + 2..].iter().position(|byte| (0x40..=0x7e).contains(byte)) else { break };
            index += end + 3;
        } else if *kind == b']' {
            let Some(end) = input[index + 2..].windows(2).position(|pair| pair == b"\x1b\\") else { break };
            if !input[index + 2..].starts_with(b"8;;") {
                return Err(EngineError("native_auth_claude_output_invalid".into()));
            }
            index += end + 4;
        } else {
            return Err(EngineError("native_auth_claude_output_invalid".into()));
        }
    }
    Ok(output)
}
