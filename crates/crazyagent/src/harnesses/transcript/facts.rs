use super::*;

/// The facts of one Codex rollout line. Takes the line and whether it is the output of an
/// exec tool call ([`CodexExecCalls::sees`]), the only output whose header can name a
/// command left running. Returns the facts, none for a line that says nothing the session
/// follows.
pub(super) fn codex_facts(value: &Value, exec: bool) -> Vec<TranscriptEvent> {
    let payload = &value["payload"];
    match (value["type"].as_str(), payload["type"].as_str()) {
        (Some("event_msg"), Some("task_started")) => vec![TranscriptEvent::TurnStarted],
        (Some("event_msg"), Some("task_complete")) => {
            vec![TranscriptEvent::TurnFinished { success: true }]
        }
        (Some("event_msg"), Some("turn_aborted")) => {
            let mut facts = Vec::new();
            if payload["reason"] == "interrupted" {
                facts.push(TranscriptEvent::Interrupted);
            }
            facts.push(TranscriptEvent::TurnFinished { success: false });
            facts
        }
        (Some("event_msg"), Some("error" | "stream_error")) => vec![TranscriptEvent::TurnError {
            rate_limited: rate_limited(payload["message"].as_str().unwrap_or_default()),
        }],
        (Some("response_item"), Some("message")) if payload["role"] == "assistant" => {
            payload["content"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter(|item| item["type"] == "output_text")
                        .filter_map(|item| item["text"].as_str())
                        .filter(|text| !text.is_empty())
                        .map(line_text)
                        .collect()
                })
                .unwrap_or_default()
        }
        (
            Some("response_item"),
            Some("function_call" | "custom_tool_call" | "local_shell_call" | "web_search_call"),
        ) => vec![TranscriptEvent::ToolUse {
            name: payload["name"]
                .as_str()
                .or(payload["type"].as_str())
                .unwrap_or("tool")
                .to_owned(),
        }],
        (Some("response_item"), Some("function_call_output")) if exec => {
            output_texts(&payload["output"])
                .first()
                .copied()
                .and_then(codex_running)
                .map(|id| TranscriptEvent::BackgroundStarted { id })
                .into_iter()
                .collect()
        }
        (Some("event_msg"), Some("item_completed"))
            if payload["item"]["type"] == "CommandExecution"
                && payload["item"]["status"] != "in_progress" =>
        {
            payload["item"]["process_id"]
                .as_str()
                .and_then(codex_session_id)
                .map(|id| TranscriptEvent::BackgroundEnded { id })
                .into_iter()
                .collect()
        }
        _ => Vec::new(),
    }
}

/// The header line Codex 0.164 writes into an exec tool output while the command it started
/// still runs, followed by the session id of that command (core tools/context.rs
/// `response_header`).
const CODEX_RUNNING: &str = "Process running with session ID ";

/// The line that ends the header of a Codex exec tool output; every line after it is the
/// command's own output, which the command, the repository or fetched text controls.
const CODEX_OUTPUT: &str = "Output:";

/// The most lines a Codex 0.164 exec output header has: chunk id, wall time, exit code,
/// running session, original token count and `Output:`.
const CODEX_HEADER_LINES_MAX: usize = 6;

/// The Codex 0.164 tools whose output header names a command left running (unified exec).
const CODEX_EXEC_TOOLS: [&str; 2] = ["exec_command", "write_stdin"];

/// The most exec tool calls a reader waits for the output of at once; past it the oldest
/// is forgotten, and its output then starts no background command.
pub(super) const CODEX_EXEC_CALLS_MAX: usize = 64;

/// The longest Codex tool call id a reader keeps.
const CODEX_CALL_ID_BYTES_MAX: usize = 128;

/// The exec tool calls of a Codex rollout whose output is still to come, oldest first: only
/// the header Codex writes on an exec output can name a command left running, so the
/// output of any other tool, or text in an output's body, never starts one (PA9-F1).
#[derive(Debug, Default)]
pub(super) struct CodexExecCalls {
    waiting: std::collections::VecDeque<String>,
}

impl CodexExecCalls {
    /// Takes one rollout line: remembers an exec_command or write_stdin call by its call
    /// id, and tells whether the line is the output of a remembered call, which it then
    /// forgets. Takes the line. Returns true only for such an output.
    pub(super) fn sees(&mut self, value: &Value) -> bool {
        let payload = &value["payload"];
        let call = payload["call_id"]
            .as_str()
            .filter(|call| !call.is_empty() && call.len() <= CODEX_CALL_ID_BYTES_MAX);
        let Some(call) = call.filter(|_| value["type"] == "response_item") else {
            return false;
        };
        let exec = match payload["type"].as_str() {
            Some("function_call") => {
                let tool = payload["name"].as_str().unwrap_or_default();
                if CODEX_EXEC_TOOLS.contains(&tool) {
                    self.waiting.push_back(call.to_owned());
                    if self.waiting.len() > CODEX_EXEC_CALLS_MAX {
                        self.waiting.pop_front();
                    }
                }
                false
            }
            Some("function_call_output") => {
                match self.waiting.iter().position(|waiting| waiting == call) {
                    Some(at) => self.waiting.remove(at).is_some(),
                    None => false,
                }
            }
            _ => false,
        };
        assert!(self.waiting.len() <= CODEX_EXEC_CALLS_MAX);
        exec
    }
}

/// The session id the header of a Codex exec output names as still running. The header is
/// the lines before the first `Output:` line, at most `CODEX_HEADER_LINES_MAX`; the lines
/// after it are never read. Takes the output's first text. Returns the id, none for text
/// with no header or no running line in it.
fn codex_running(text: &str) -> Option<String> {
    let mut running = None;
    for line in text.lines().take(CODEX_HEADER_LINES_MAX) {
        if line == CODEX_OUTPUT {
            return running;
        }
        if let Some(id) = line.strip_prefix(CODEX_RUNNING) {
            running = codex_session_id(id);
        }
    }
    None
}

/// The most digits a Codex session id is read with.
const CODEX_SESSION_DIGITS_MAX: usize = 20;

/// The texts of a Codex tool output in the shapes a rollout keeps it: a string, a list of
/// content parts with text, or an object whose `content` is either. Takes the output.
/// Returns its texts, none for any other value.
fn output_texts(output: &Value) -> Vec<&str> {
    match output {
        Value::String(text) => vec![text.as_str()],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect(),
        Value::Object(_) if !output["content"].is_object() => output_texts(&output["content"]),
        _ => Vec::new(),
    }
}

/// A Codex background command's session id as the engine keeps it: digits only, at most
/// `CODEX_SESSION_DIGITS_MAX`. Takes the text after the marker. Returns the id, none for
/// any other text.
fn codex_session_id(text: &str) -> Option<String> {
    let id = text.trim();
    let digits = !id.is_empty()
        && id.len() <= CODEX_SESSION_DIGITS_MAX
        && id.bytes().all(|byte| byte.is_ascii_digit());
    digits.then(|| id.to_owned())
}

/// The texts of a Claude Code content value: the string itself, or each text block of an
/// array. Takes the value. Returns the texts, none for any other value.
pub(super) fn content_texts(content: &Value) -> Vec<&str> {
    match content {
        Value::String(text) => vec![text.as_str()],
        Value::Array(items) => items
            .iter()
            .filter(|item| item["type"] == "text")
            .filter_map(|item| item["text"].as_str())
            .collect(),
        _ => Vec::new(),
    }
}

/// Says whether a Claude Code user line is a prompt the model answers, the only user line
/// that starts a turn. A compact summary, a transcript-only line, a sub-agent's line and a
/// line that carries a tool result are none; neither is text that starts with one of
/// `CLAUDE_ASIDES`, which Claude Code writes for a local command, bash mode, a reminder or
/// a hook without calling the model. Takes the line and its texts. Returns the answer.
pub(super) fn claude_prompt(value: &Value, texts: &[&str]) -> bool {
    let marked = CLAUDE_NOT_PROMPTS.iter().any(|key| value[*key] == true);
    let result = value["message"]["content"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["type"] == "tool_result"));
    let typed = |text: &&str| {
        let text = text.trim_start();
        !text.is_empty() && !CLAUDE_ASIDES.iter().any(|tag| text.starts_with(tag))
    };
    !marked && !result && texts.iter().any(typed)
}

/// Says whether a Claude Code line is a user line that carries text yet starts no turn: not
/// a meta line, not an interrupt and no prompt. The model may still answer one, as it does a
/// typed command that runs a prompt. Takes the line. Returns the answer.
pub(super) fn claude_aside(value: &Value) -> bool {
    if value["type"] != "user" || value["isMeta"] == true {
        return false;
    }
    let texts = content_texts(&value["message"]["content"]);
    let interrupt = texts
        .iter()
        .any(|text| text.starts_with(CLAUDE_INTERRUPTED));
    !texts.is_empty() && !interrupt && !claude_prompt(value, &texts)
}

/// The facts of one Claude Code session line. A reply cut at the output cap does not end
/// the turn, since Claude Code asks the model to go on and ends a turn it gives up with its
/// own error line, and its text gains no line break, so the parts join as one answer.
/// Takes the line. Returns the facts.
pub(super) fn claude_facts(value: &Value) -> Vec<TranscriptEvent> {
    let message = &value["message"];
    match value["type"].as_str() {
        Some("attachment")
            if value["attachment"]["type"] == "queued_command"
                && value["attachment"]["commandMode"] == CLAUDE_NOTIFICATION_MODE =>
        {
            content_texts(&value["attachment"]["prompt"])
                .iter()
                .flat_map(|text| notified_tasks(text))
                .map(|id| TranscriptEvent::BackgroundEnded { id })
                .collect()
        }
        Some("user") if value["isMeta"] != true => {
            let texts = content_texts(&message["content"]);
            let result = &value["toolUseResult"];
            let settled = matches!(result["status"].as_str(), Some("completed" | "cancelled"));
            let mut facts: Vec<TranscriptEvent> = conversation_id(&result["backgroundTaskId"])
                .filter(|_| result["backgroundEndsWithFinalResponse"] != true)
                .map(|id| TranscriptEvent::BackgroundStarted { id })
                .into_iter()
                .chain(
                    conversation_id(&result["task_id"])
                        .into_iter()
                        .chain(conversation_id(&result["taskId"]).filter(|_| settled))
                        .map(|id| TranscriptEvent::BackgroundEnded { id }),
                )
                .collect();
            facts.extend(
                texts
                    .iter()
                    .flat_map(|text| notified_tasks(text))
                    .map(|id| TranscriptEvent::BackgroundEnded { id }),
            );
            if texts
                .iter()
                .any(|text| text.starts_with(CLAUDE_INTERRUPTED))
            {
                facts.extend([
                    TranscriptEvent::Interrupted,
                    TranscriptEvent::TurnFinished { success: false },
                ]);
            } else if claude_prompt(value, &texts) {
                facts.push(TranscriptEvent::TurnStarted);
            }
            facts
        }
        Some("assistant") if value["isApiErrorMessage"] == true => {
            let code = value["error"].as_str().unwrap_or_default();
            vec![
                TranscriptEvent::TurnError {
                    rate_limited: rate_limited(code),
                },
                TranscriptEvent::TurnFinished { success: false },
            ]
        }
        Some("assistant") => {
            let cut = message["stop_reason"] == "max_tokens";
            let mut facts = Vec::new();
            let mut said = false;
            for item in message["content"].as_array().into_iter().flatten() {
                match item["type"].as_str() {
                    Some("text") => {
                        if let Some(text) = item["text"].as_str().filter(|text| !text.is_empty()) {
                            said = true;
                            facts.push(if cut {
                                TranscriptEvent::Text {
                                    text: text.to_owned(),
                                }
                            } else {
                                line_text(text)
                            });
                        }
                    }
                    Some("tool_use") => facts.push(TranscriptEvent::ToolUse {
                        name: item["name"].as_str().unwrap_or("tool").to_owned(),
                    }),
                    _ => {}
                }
            }
            if said
                && matches!(
                    message["stop_reason"].as_str(),
                    Some("end_turn" | "stop_sequence")
                )
            {
                facts.push(TranscriptEvent::TurnFinished { success: true });
            }
            facts
        }
        Some("system") if value["subtype"] == "turn_duration" => {
            vec![TranscriptEvent::TurnFinished { success: true }]
        }
        _ => Vec::new(),
    }
}
