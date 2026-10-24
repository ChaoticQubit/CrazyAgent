use super::interactive::Transcript;
use super::launch::{LaunchError, failure, rate_limited};
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const READ_BYTES_MAX: usize = 1024 * 1024;
pub const LINE_BYTES_MAX: usize = 16 * 1024 * 1024;
pub const EVENTS_PER_READ_MAX: usize = 4096;
const ROLLOUTS_SCANNED_MAX: usize = 512;
const ROLLOUT_HEAD_BYTES_MAX: usize = 256 * 1024;
const ROLLOUT_CLOCK_SLACK: Duration = Duration::from_secs(5);
const CLAUDE_INTERRUPTED: &str = "[Request interrupted by user";

/// One fact read from a harness's own session record, whatever the harness.
/// `Waiting` means the harness waits for the owner, such as an approval or a question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptEvent {
    TurnStarted,
    Text { text: String },
    ToolUse { name: String },
    Waiting,
    Interrupted,
    TurnFinished { success: bool },
    TurnError { rate_limited: bool },
}

/// Follows one harness session record as the harness appends to it: the Codex rollout of a
/// session started in `cwd` after `since`, or the Claude Code transcript of a known session.
/// Each read returns the facts of the lines completed since the previous read.
pub struct TranscriptReader {
    source: Transcript,
    cwd: String,
    since: SystemTime,
    file: Option<File>,
    pending: Vec<u8>,
    in_turn: bool,
    consumed: u64,
}

impl TranscriptReader {
    /// Starts following a session record that may not exist yet.
    /// Inputs: where the harness writes it, the canonical working folder of the session and
    /// the time the harness started. Output: the reader, positioned at the record's start.
    pub fn new(source: Transcript, cwd: &Path, since: SystemTime) -> Self {
        Self {
            source,
            cwd: cwd.to_string_lossy().into_owned(),
            since,
            file: None,
            pending: Vec::new(),
            in_turn: false,
            consumed: 0,
        }
    }

    /// Counts the bytes read from the record so far, so a caller can bound a session's output.
    /// Output: the total since the reader started.
    pub fn bytes(&self) -> u64 {
        self.consumed
    }

    /// Reads what the harness appended since the last read, at most `READ_BYTES_MAX` bytes.
    /// Output: the facts in order, none while the record does not exist yet, or a plain
    /// code when a line passes `LINE_BYTES_MAX` or the record cannot be read.
    pub fn read(&mut self) -> Result<Vec<TranscriptEvent>, LaunchError> {
        if self.file.is_none() {
            self.file = match &self.source {
                Transcript::CodexRollouts { sessions } => {
                    find_rollout(sessions, &self.cwd, self.since)
                }
                Transcript::ClaudeSession { file } => open_plain(file),
                Transcript::Unavailable => None,
            };
        }
        let Some(file) = self.file.as_mut() else {
            return Ok(Vec::new());
        };
        let count = file
            .take(READ_BYTES_MAX as u64)
            .read_to_end(&mut self.pending)
            .map_err(|_| failure("launch_transcript_unavailable"))?;
        self.consumed += count as u64;
        let mut events = Vec::new();
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let Ok(value) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            let facts = match self.source {
                Transcript::CodexRollouts { .. } => codex_facts(&value),
                Transcript::ClaudeSession { .. } => claude_facts(&value),
                Transcript::Unavailable => Vec::new(),
            };
            for fact in facts {
                if let Some(fact) = self.track(fact) {
                    events.push(fact);
                }
            }
            if events.len() >= EVENTS_PER_READ_MAX {
                break;
            }
        }
        if self.pending.len() > LINE_BYTES_MAX {
            return Err(failure("launch_transcript_line_too_long"));
        }
        assert!(events.len() >= EVENTS_PER_READ_MAX || !self.pending.contains(&b'\n'));
        Ok(events)
    }

    fn track(&mut self, fact: TranscriptEvent) -> Option<TranscriptEvent> {
        match fact {
            TranscriptEvent::TurnStarted => {
                self.in_turn = true;
                Some(fact)
            }
            TranscriptEvent::TurnFinished { .. } if !self.in_turn => None,
            TranscriptEvent::TurnFinished { .. } => {
                self.in_turn = false;
                Some(fact)
            }
            other => Some(other),
        }
    }
}

/// Opens a transcript only when the path itself is a regular file, never a link's target:
/// the open takes the reparse point rather than following it. Takes the path.
/// Returns the file, or none for a link, a directory or an open failure.
#[cfg(windows)]
fn open_plain(path: &Path) -> Option<File> {
    use std::os::windows::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(crate::platform::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

#[cfg(unix)]
fn open_plain(path: &Path) -> Option<File> {
    let file = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok()
    .map(File::from)?;
    file.metadata().ok()?.is_file().then_some(file)
}

fn find_rollout(sessions: &Path, cwd: &str, since: SystemTime) -> Option<File> {
    let earliest = since.checked_sub(ROLLOUT_CLOCK_SLACK).unwrap_or(since);
    let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
    let mut scanned = 0;
    for year in newest(sessions, 2) {
        for month in newest(&year, 2) {
            for day in newest(&month, 2) {
                let Ok(entries) = std::fs::read_dir(&day) else {
                    continue;
                };
                for entry in entries.flatten() {
                    scanned += 1;
                    if scanned > ROLLOUTS_SCANNED_MAX {
                        break;
                    }
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                        continue;
                    }
                    let Ok(metadata) = entry.metadata() else {
                        continue;
                    };
                    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    if metadata.is_file() && modified >= earliest {
                        candidates.push((modified, entry.path()));
                    }
                }
            }
        }
    }
    candidates.sort_by_key(|a| std::cmp::Reverse(a.0));
    candidates
        .into_iter()
        .find_map(|(_, path)| open_plain(&path).filter(|file| started_in(file, cwd)))
        .and_then(rewound)
}

fn rewound(mut file: File) -> Option<File> {
    file.seek(SeekFrom::Start(0)).ok()?;
    Some(file)
}

fn newest(folder: &Path, keep: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .take(ROLLOUTS_SCANNED_MAX)
        .collect();
    folders.sort();
    folders.into_iter().rev().take(keep).collect()
}

fn started_in(file: &File, cwd: &str) -> bool {
    let mut head = Vec::new();
    if file
        .take(ROLLOUT_HEAD_BYTES_MAX as u64)
        .read_to_end(&mut head)
        .is_err()
    {
        return false;
    }
    let first = head.split(|byte| *byte == b'\n').next().unwrap_or_default();
    serde_json::from_slice::<Value>(first).is_ok_and(|value| {
        value["type"] == "session_meta" && value["payload"]["cwd"].as_str() == Some(cwd)
    })
}

fn line_text(text: &str) -> TranscriptEvent {
    let mut text = text.to_owned();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    TranscriptEvent::Text { text }
}

fn codex_facts(value: &Value) -> Vec<TranscriptEvent> {
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
        _ => Vec::new(),
    }
}

fn claude_facts(value: &Value) -> Vec<TranscriptEvent> {
    let message = &value["message"];
    match value["type"].as_str() {
        Some("user") if value["isMeta"] != true => {
            let texts: Vec<&str> = match &message["content"] {
                Value::String(text) => vec![text.as_str()],
                Value::Array(items) => items
                    .iter()
                    .filter(|item| item["type"] == "text")
                    .filter_map(|item| item["text"].as_str())
                    .collect(),
                _ => Vec::new(),
            };
            if texts
                .iter()
                .any(|text| text.starts_with(CLAUDE_INTERRUPTED))
            {
                vec![
                    TranscriptEvent::Interrupted,
                    TranscriptEvent::TurnFinished { success: false },
                ]
            } else if texts.is_empty() {
                Vec::new()
            } else {
                vec![TranscriptEvent::TurnStarted]
            }
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
            let mut facts = Vec::new();
            let mut said = false;
            for item in message["content"].as_array().into_iter().flatten() {
                match item["type"].as_str() {
                    Some("text") => {
                        if let Some(text) = item["text"].as_str().filter(|text| !text.is_empty()) {
                            said = true;
                            facts.push(line_text(text));
                        }
                    }
                    Some("tool_use") => facts.push(TranscriptEvent::ToolUse {
                        name: item["name"].as_str().unwrap_or("tool").to_owned(),
                    }),
                    _ => {}
                }
            }
            match message["stop_reason"].as_str() {
                Some("end_turn" | "stop_sequence") if said => {
                    facts.push(TranscriptEvent::TurnFinished { success: true });
                }
                Some("max_tokens") => facts.push(TranscriptEvent::TurnFinished { success: false }),
                _ => {}
            }
            facts
        }
        Some("system") if value["subtype"] == "turn_duration" => {
            vec![TranscriptEvent::TurnFinished { success: true }]
        }
        _ => Vec::new(),
    }
}

#[cfg(all(test, unix))]
mod tests;
