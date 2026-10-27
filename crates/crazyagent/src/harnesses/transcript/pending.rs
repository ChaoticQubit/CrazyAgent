use super::{CLAUDE_INTERRUPTED, conversation_id, open_plain, plain_below, rollout_id};
use crate::harnesses::terminal_session::RecordSource;
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const SUMMARY_BYTES_MAX: usize = 200;
pub const PENDING_MAX: usize = 16;
const TOOL_BYTES_MAX: usize = 64;
const OPEN_MAX: usize = 256;
const TAIL_BYTES_MAX: u64 = 4 * 1024 * 1024;
const SUMMARY_FIELDS: [&str; 9] = [
    "command",
    "cmd",
    "file_path",
    "notebook_path",
    "path",
    "url",
    "pattern",
    "query",
    "description",
];

/// One command an agent started in its harness whose result its session record never got,
/// so whether and how far it ran is unknown: the tool's name and what it was asked to do,
/// each one plain line, the summary at most `SUMMARY_BYTES_MAX` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingCommand {
    pub tool: String,
    pub summary: String,
}

/// What one record line says about commands: a new turn began, a command started with
/// its call id, or the command with that call id got its result.
enum Step {
    Turn,
    Call(String, PendingCommand),
    Result(String),
}

/// Names the commands the last turn of a harness conversation started without a recorded
/// result, for the note an interrupted agent resumes with. Reads at most the record's last
/// `TAIL_BYTES_MAX` bytes, and only the record the source names: the Claude Code transcript
/// itself, or a Codex rollout plainly below the sessions folder whose first line names
/// `cwd`; never a link. Takes the source, the canonical working folder and the record.
/// Returns at most the newest `PENDING_MAX` commands, oldest first; none when unreadable.
pub fn unknown(source: &RecordSource, cwd: &Path, record: &Path) -> Vec<PendingCommand> {
    let Some(tail) = opened(source, cwd, record).and_then(tail) else {
        return Vec::new();
    };
    let mut open: Vec<(String, PendingCommand)> = Vec::new();
    for entry in tail.split(|byte| *byte == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(entry) else {
            continue;
        };
        let steps = match source {
            RecordSource::CodexRollouts { .. } => codex_steps(&value),
            RecordSource::ClaudeSession { .. } => claude_steps(&value),
        };
        for step in steps {
            match step {
                Step::Turn => open.clear(),
                Step::Call(id, _) if open.iter().any(|(known, _)| *known == id) => {}
                Step::Call(id, command) => {
                    if open.len() == OPEN_MAX {
                        open.remove(0);
                    }
                    open.push((id, command));
                }
                Step::Result(id) => open.retain(|(known, _)| *known != id),
            }
        }
        assert!(open.len() <= OPEN_MAX);
    }
    let older = open.len().saturating_sub(PENDING_MAX);
    let pending: Vec<PendingCommand> = open
        .into_iter()
        .skip(older)
        .map(|(_, command)| command)
        .collect();
    assert!(pending.len() <= PENDING_MAX);
    pending
}

/// Opens the record only where `TranscriptReader::resumed` would follow it.
/// Takes the source, the canonical working folder and the record.
/// Returns the file, or none for a record elsewhere, of another folder, a link or missing.
fn opened(source: &RecordSource, cwd: &Path, record: &Path) -> Option<File> {
    match source {
        RecordSource::CodexRollouts { sessions } if plain_below(sessions, record) => {
            let file = open_plain(record)?;
            rollout_id(&file, &cwd.to_string_lossy())?;
            Some(file)
        }
        RecordSource::ClaudeSession { file } if file == record => open_plain(record),
        _ => None,
    }
}

/// Reads the last `TAIL_BYTES_MAX` bytes of a record, without the cut first line when the
/// record is longer. Takes the open record. Returns the bytes, or none on a read failure.
fn tail(mut file: File) -> Option<Vec<u8>> {
    let length = file.metadata().ok()?.len();
    let skip = length.saturating_sub(TAIL_BYTES_MAX);
    file.seek(SeekFrom::Start(skip)).ok()?;
    let mut tail = Vec::new();
    file.take(TAIL_BYTES_MAX).read_to_end(&mut tail).ok()?;
    if skip > 0 {
        let first = tail
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(tail.len(), |end| end + 1);
        tail.drain(..first);
    }
    assert!(tail.len() as u64 <= TAIL_BYTES_MAX);
    Some(tail)
}

/// Reads one Claude Code transcript line: a typed prompt that is not an interrupt begins a
/// turn, `tool_use` items start commands and `tool_result` items end them. Meta and
/// sub-agent (sidechain) lines say nothing. Takes the line. Returns its steps in order.
fn claude_steps(value: &Value) -> Vec<Step> {
    if value["isSidechain"] == true {
        return Vec::new();
    }
    let content = &value["message"]["content"];
    match value["type"].as_str() {
        Some("user") if value["isMeta"] != true => {
            let (texts, results): (Vec<&str>, Vec<Step>) = match content {
                Value::String(text) => (vec![text.as_str()], Vec::new()),
                Value::Array(items) => (
                    items
                        .iter()
                        .filter(|item| item["type"] == "text")
                        .filter_map(|item| item["text"].as_str())
                        .collect(),
                    items
                        .iter()
                        .filter(|item| item["type"] == "tool_result")
                        .filter_map(|item| conversation_id(&item["tool_use_id"]))
                        .map(Step::Result)
                        .collect(),
                ),
                _ => (Vec::new(), Vec::new()),
            };
            let mut steps = results;
            if !texts.is_empty()
                && !texts
                    .iter()
                    .any(|text| text.starts_with(CLAUDE_INTERRUPTED))
            {
                steps.push(Step::Turn);
            }
            steps
        }
        Some("assistant") => content
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["type"] == "tool_use")
            .filter_map(|item| call(&item["id"], &item["name"], summary(&item["input"])))
            .collect(),
        _ => Vec::new(),
    }
}

/// Reads one Codex rollout line: `task_started` begins a turn, function, custom tool and
/// local shell calls start commands and their outputs end them. Takes the line.
/// Returns its steps.
fn codex_steps(value: &Value) -> Vec<Step> {
    let payload = &value["payload"];
    let step = match (value["type"].as_str(), payload["type"].as_str()) {
        (Some("event_msg"), Some("task_started")) => Some(Step::Turn),
        (Some("response_item"), Some("function_call")) => {
            let said = match &payload["arguments"] {
                Value::String(arguments) => serde_json::from_str::<Value>(arguments).map_or_else(
                    |_| line(arguments, SUMMARY_BYTES_MAX),
                    |input| summary(&input),
                ),
                other => summary(other),
            };
            call(&payload["call_id"], &payload["name"], said)
        }
        (Some("response_item"), Some("custom_tool_call")) => call(
            &payload["call_id"],
            &payload["name"],
            summary(&payload["input"]),
        ),
        (Some("response_item"), Some("local_shell_call")) => {
            let id = if payload["call_id"].is_string() {
                &payload["call_id"]
            } else {
                &payload["id"]
            };
            call(id, &Value::from("local_shell"), summary(&payload["action"]))
        }
        (Some("response_item"), Some("function_call_output" | "custom_tool_call_output")) => {
            conversation_id(&payload["call_id"]).map(Step::Result)
        }
        _ => None,
    };
    step.into_iter().collect()
}

/// A started command, kept only with a short plain call id to match its result by.
/// Takes the call id, the tool name and the summary. Returns the step, or none.
fn call(id: &Value, tool: &Value, summary: String) -> Option<Step> {
    let mut tool = line(tool.as_str().unwrap_or_default(), TOOL_BYTES_MAX);
    if tool.is_empty() {
        tool = String::from("tool");
    }
    Some(Step::Call(
        conversation_id(id)?,
        PendingCommand { tool, summary },
    ))
}

/// What a command was asked to do: its first `SUMMARY_FIELDS` field (a text, or a list of
/// words joined by spaces), else the input text, else the input as compact JSON.
/// Takes the input. Returns one plain line of at most `SUMMARY_BYTES_MAX` bytes.
fn summary(input: &Value) -> String {
    let field = SUMMARY_FIELDS.iter().find_map(|name| match &input[*name] {
        Value::String(text) => Some(text.clone()),
        Value::Array(words) => Some(
            words
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    });
    let text = match (field, input) {
        (Some(text), _) => text,
        (None, Value::String(text)) => text.clone(),
        (None, Value::Null) => String::new(),
        (None, other) => other.to_string(),
    };
    line(&text, SUMMARY_BYTES_MAX)
}

/// Makes a harness's text one plain line: every control character and run of white space
/// becomes one space, so no escape or paste end reaches the prompt it is typed into, cut to
/// at most `max` bytes on a character boundary with "..." marking a cut.
/// Takes the text and bound. Returns the line, empty when nothing visible is left.
fn line(text: &str, max: usize) -> String {
    assert!(max > 3);
    let visible: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut joined = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.len() > max {
        let mut end = max - 3;
        while !joined.is_char_boundary(end) {
            end -= 1;
        }
        joined.truncate(end);
        joined.truncate(joined.trim_end().len());
        joined.push_str("...");
    }
    assert!(joined.len() <= max);
    assert!(!joined.chars().any(char::is_control));
    joined
}

#[cfg(all(test, unix))]
mod tests;
