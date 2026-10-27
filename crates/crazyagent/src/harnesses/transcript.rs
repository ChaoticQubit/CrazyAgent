use super::interactive::Transcript;
use super::launch::{LaunchError, failure, rate_limited};
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const READ_BYTES_MAX: usize = 1024 * 1024;
pub const LINE_BYTES_MAX: usize = 16 * 1024 * 1024;
pub const EVENTS_PER_READ_MAX: usize = 4096;
const ROLLOUTS_SCANNED_MAX: usize = 512;
const ROLLOUT_HEAD_BYTES_MAX: usize = 256 * 1024;
const ROLLOUT_CLOCK_SLACK: Duration = Duration::from_secs(5);
const CLAUDE_INTERRUPTED: &str = "[Request interrupted by user";
const CONVERSATION_ID_BYTES_MAX: usize = 128;
const EXCERPT_TAIL_BYTES_MAX: u64 = 4 * 1024 * 1024;
const EXCERPT_MESSAGES_MAX: usize = 256;

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
    earlier: Vec<FileKey>,
    file: Option<File>,
    found: Option<(String, PathBuf)>,
    start: u64,
    pending: Vec<u8>,
    in_turn: bool,
    consumed: u64,
}

impl TranscriptReader {
    /// Starts following the session record of a harness about to start, which does not exist
    /// yet. Every Codex rollout already present is remembered and never taken as this
    /// session's record, whatever its time or working folder, since another writer of a shared
    /// home may have left it. Inputs: where the harness writes it, the canonical working folder
    /// of the session and the time the harness starts. Output: the reader at the record's start.
    pub fn new(source: Transcript, cwd: &Path, since: SystemTime) -> Self {
        let earlier = match &source {
            Transcript::CodexRollouts { sessions } => {
                let mut keys: Vec<FileKey> = rollouts(sessions)
                    .into_iter()
                    .map(|(_, _, key)| key)
                    .collect();
                keys.sort();
                keys
            }
            _ => Vec::new(),
        };
        Self::with_earlier(source, cwd, since, earlier)
    }

    /// Follows the session record of a harness that is already running, such as a pane the
    /// engine adopts after a restart, whose record may already exist. Inputs: as for `new`.
    /// Output: the reader at the record's start.
    pub fn adopted(source: Transcript, cwd: &Path, since: SystemTime) -> Self {
        Self::with_earlier(source, cwd, since, Vec::new())
    }

    fn with_earlier(
        source: Transcript,
        cwd: &Path,
        since: SystemTime,
        earlier: Vec<FileKey>,
    ) -> Self {
        Self {
            source,
            cwd: cwd.to_string_lossy().into_owned(),
            since,
            earlier,
            file: None,
            found: None,
            start: 0,
            pending: Vec::new(),
            in_turn: false,
            consumed: 0,
        }
    }

    /// Follows the record of a conversation the harness resumes and appends to: a Codex
    /// rollout under the source's sessions folder that started in `cwd`, or the Claude Code
    /// transcript the source names. Reading starts at `from`, or at the record's current end
    /// when none, so the earlier turns are not reported again. Inputs: the source, the
    /// canonical working folder, the record and the offset. Output: the reader, or
    /// `launch_resume_unavailable` for a record that is missing, a link, elsewhere, of
    /// another folder or shorter than `from`.
    pub fn resumed(
        source: Transcript,
        cwd: &Path,
        record: &Path,
        from: Option<u64>,
    ) -> Result<Self, LaunchError> {
        let unavailable = || failure("launch_resume_unavailable");
        let mut reader = Self::with_earlier(source, cwd, SystemTime::now(), Vec::new());
        let (mut file, id) = match &reader.source {
            Transcript::CodexRollouts { sessions } if plain_below(sessions, record) => {
                let file = open_plain(record).ok_or_else(unavailable)?;
                let id = rollout_id(&file, &reader.cwd).ok_or_else(unavailable)?;
                (file, id)
            }
            Transcript::ClaudeSession { file } if file == record => {
                let id = claude_id(record).ok_or_else(unavailable)?;
                (open_plain(record).ok_or_else(unavailable)?, id)
            }
            _ => return Err(unavailable()),
        };
        let end = file.seek(SeekFrom::End(0)).map_err(|_| unavailable())?;
        let start = from.unwrap_or(end);
        if start > end {
            return Err(unavailable());
        }
        reader.start = file
            .seek(SeekFrom::Start(start))
            .map_err(|_| unavailable())?;
        assert_eq!(reader.start, start);
        reader.file = Some(file);
        reader.found = Some((id, record.to_path_buf()));
        Ok(reader)
    }

    /// Names the conversation this reader follows once its record is open: the id the
    /// harness resumes it by and the record's path. Output: none before the record exists.
    pub fn conversation(&self) -> Option<(String, PathBuf)> {
        self.found.clone()
    }

    /// Where in its record this reader started, the end of the earlier turns for a resumed
    /// conversation. Output: the byte offset, 0 for a record read from its start.
    pub fn start(&self) -> u64 {
        self.start
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
            let opened = match &self.source {
                Transcript::CodexRollouts { sessions } => {
                    find_rollout(sessions, &self.cwd, self.since, &self.earlier)
                }
                Transcript::ClaudeSession { file } => {
                    open_plain(file).map(|opened| (opened, claude_id(file), file.clone()))
                }
                Transcript::Unavailable => None,
            };
            if let Some((file, id, path)) = opened {
                self.found = id.map(|id| (id, path));
                self.file = Some(file);
            }
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

#[cfg(unix)]
type FileKey = (u64, u64);
#[cfg(not(unix))]
type FileKey = PathBuf;

#[cfg(unix)]
fn file_key(metadata: &std::fs::Metadata, _path: &Path) -> FileKey {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn file_key(_metadata: &std::fs::Metadata, path: &Path) -> FileKey {
    path.to_path_buf()
}

fn rollouts(sessions: &Path) -> Vec<(SystemTime, PathBuf, FileKey)> {
    let mut found = Vec::new();
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
                    if metadata.is_file() {
                        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                        let path = entry.path();
                        let key = file_key(&metadata, &path);
                        found.push((modified, path, key));
                    }
                }
            }
        }
    }
    found
}

fn find_rollout(
    sessions: &Path,
    cwd: &str,
    since: SystemTime,
    earlier: &[FileKey],
) -> Option<(File, Option<String>, PathBuf)> {
    let earliest = since.checked_sub(ROLLOUT_CLOCK_SLACK).unwrap_or(since);
    let mut candidates: Vec<(SystemTime, PathBuf)> = rollouts(sessions)
        .into_iter()
        .filter(|(modified, _, key)| *modified >= earliest && earlier.binary_search(key).is_err())
        .map(|(modified, path, _)| (modified, path))
        .collect();
    candidates.sort_by_key(|a| std::cmp::Reverse(a.0));
    candidates.into_iter().find_map(|(_, path)| {
        let file = open_plain(&path).filter(|file| {
            file.metadata()
                .is_ok_and(|metadata| earlier.binary_search(&file_key(&metadata, &path)).is_err())
        })?;
        let meta = session_meta(&file).filter(|meta| meta["cwd"].as_str() == Some(cwd))?;
        Some((rewound(file)?, conversation_id(&meta["id"]), path))
    })
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

/// Tells whether a record lies inside a folder through plain folders only: no `..`, no
/// link between the folder and the record. Takes the folder and the record. Returns true
/// when every step is a plain name and every folder on the way is a real folder.
fn plain_below(folder: &Path, record: &Path) -> bool {
    let Ok(rest) = record.strip_prefix(folder) else {
        return false;
    };
    let mut path = folder.to_path_buf();
    let mut parts = rest.components().peekable();
    while let Some(part) = parts.next() {
        let Component::Normal(name) = part else {
            return false;
        };
        path.push(name);
        if parts.peek().is_some()
            && !std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir())
        {
            return false;
        }
    }
    path != folder
}

/// Reads the payload of a rollout's first line when that line is its `session_meta`.
/// Takes the rollout opened at its start. Returns the payload, or none.
fn session_meta(file: &File) -> Option<Value> {
    let mut head = Vec::new();
    file.take(ROLLOUT_HEAD_BYTES_MAX as u64)
        .read_to_end(&mut head)
        .ok()?;
    let first = head.split(|byte| *byte == b'\n').next().unwrap_or_default();
    let mut value = serde_json::from_slice::<Value>(first).ok()?;
    (value["type"] == "session_meta").then(|| value["payload"].take())
}

/// The id a resumed Codex rollout is known by, when the rollout started in `cwd`.
/// Takes the rollout opened at its start and the folder. Returns the id, or none.
fn rollout_id(file: &File, cwd: &str) -> Option<String> {
    session_meta(file)
        .filter(|meta| meta["cwd"].as_str() == Some(cwd))
        .and_then(|meta| conversation_id(&meta["id"]))
}

/// The id of a Claude Code transcript, its file name without `.jsonl`.
/// Takes the transcript path. Returns the id, or none for a name that cannot be one.
fn claude_id(file: &Path) -> Option<String> {
    conversation_id(&Value::from(file.file_stem()?.to_str()?))
}

/// Keeps a conversation id a harness wrote only when it is short plain text.
/// Takes the JSON value. Returns the id, or none.
fn conversation_id(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|id| {
            !id.is_empty()
                && id.len() <= CONVERSATION_ID_BYTES_MAX
                && !id.chars().any(char::is_control)
        })
        .map(str::to_owned)
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

/// Who said one message of a harness conversation: what was typed into the app, by the
/// owner or the engine, or the agent's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speaker {
    Input,
    Agent,
}

/// Reads the newest typed messages and replies of a harness conversation from its record,
/// for a fresh conversation that carries it on: Codex rollout `message` items and Claude
/// Code `user` and `assistant` text, without tool output, interrupts or the harness's own
/// context blocks. Reads at most the record's last `EXCERPT_TAIL_BYTES_MAX` bytes and
/// never follows a link. Takes the harness and the record. Returns the messages oldest
/// first, at most `EXCERPT_MESSAGES_MAX`, none when the record cannot be read.
pub fn conversation_messages(harness: &str, record: &Path) -> Vec<(Speaker, String)> {
    let Some(mut file) = open_plain(record) else {
        return Vec::new();
    };
    let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
        return Vec::new();
    };
    let skip = length.saturating_sub(EXCERPT_TAIL_BYTES_MAX);
    let mut tail = Vec::new();
    if file.seek(SeekFrom::Start(skip)).is_err()
        || file
            .take(EXCERPT_TAIL_BYTES_MAX)
            .read_to_end(&mut tail)
            .is_err()
    {
        return Vec::new();
    }
    let mut lines = tail.split(|byte| *byte == b'\n');
    if skip > 0 {
        lines.next();
    }
    let mut messages = std::collections::VecDeque::new();
    for line in lines {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let said = match harness {
            "codex" => codex_message(&value),
            "claude-code" => claude_message(&value),
            _ => None,
        };
        if let Some(said) = said {
            if messages.len() == EXCERPT_MESSAGES_MAX {
                messages.pop_front();
            }
            messages.push_back(said);
        }
    }
    assert!(messages.len() <= EXCERPT_MESSAGES_MAX);
    messages.into()
}

fn codex_message(value: &Value) -> Option<(Speaker, String)> {
    let payload = &value["payload"];
    if value["type"] != "response_item" || payload["type"] != "message" {
        return None;
    }
    let speaker = match payload["role"].as_str()? {
        "user" => Speaker::Input,
        "assistant" => Speaker::Agent,
        _ => return None,
    };
    let text: String = payload["content"]
        .as_array()?
        .iter()
        .filter(|part| part["type"] == "input_text" || part["type"] == "output_text")
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    said(speaker, text)
}

fn claude_message(value: &Value) -> Option<(Speaker, String)> {
    let speaker = match value["type"].as_str()? {
        "user" if value["isMeta"] != true => Speaker::Input,
        "assistant" if value["isApiErrorMessage"] != true => Speaker::Agent,
        _ => return None,
    };
    let text = match &value["message"]["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part["type"] == "text")
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    said(speaker, text)
}

fn said(speaker: Speaker, text: String) -> Option<(Speaker, String)> {
    let start = text.trim_start();
    (!start.is_empty() && !start.starts_with('<') && !start.starts_with(CLAUDE_INTERRUPTED))
        .then_some((speaker, text))
}

#[cfg(all(test, unix))]
mod tests;
