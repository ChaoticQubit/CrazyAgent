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
/// How much of a record `typed_past` reads at a time.
const TYPED_CHUNK_BYTES: usize = 256 * 1024;
const CLAUDE_NOTIFICATION: &str = "<task-notification>";
const CLAUDE_NOTIFICATION_MODE: &str = "task-notification";
const NOTIFIED_TASKS_MAX: usize = 16;
const CLAUDE_ASIDES: [&str; 10] = [
    "<command-name>",
    "<command-message>",
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<local-command-caveat>",
    "<bash-input>",
    "<bash-stdout>",
    "<bash-stderr>",
    "<system-reminder>",
    "<user-prompt-submit-hook>",
];
const CLAUDE_NOT_PROMPTS: [&str; 3] = [
    "isCompactSummary",
    "isVisibleInTranscriptOnly",
    "isSidechain",
];

/// One fact read from a harness's own session record, whatever the harness.
/// `Waiting` means the harness waits for the owner, such as an approval or a question.
/// `BackgroundStarted` and `BackgroundEnded` follow a command the harness runs on after its
/// tool call returns, such as Claude Code's Bash `run_in_background`, by its task id: it
/// ends with its task notification, or with the agent's own stop of it, which brings none.
/// Claude Code records a notification that comes while the agent is idle as a user message
/// that starts a turn, and one that comes during a turn as a queued command attachment.
/// Any other Claude Code user line starts a turn only when it is a prompt the model answers
/// (see `claude_prompt`); the model's first line after one that is not starts it instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptEvent {
    TurnStarted,
    Text { text: String },
    ToolUse { name: String },
    Waiting,
    Interrupted,
    TurnFinished { success: bool },
    TurnError { rate_limited: bool },
    BackgroundStarted { id: String },
    BackgroundEnded { id: String },
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
    scanned: usize,
    in_turn: bool,
    aside: bool,
    dropped: bool,
    consumed: u64,
    codex_calls: CodexExecCalls,
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
            scanned: 0,
            in_turn: false,
            aside: false,
            dropped: false,
            consumed: 0,
            codex_calls: CodexExecCalls::default(),
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

    /// Says whether this record reports the commands its harness starts in the background
    /// and their ends: a Claude Code transcript and a Codex 0.164 rollout both do (PA-10).
    /// Output: the answer.
    pub fn reports_background(&self) -> bool {
        matches!(
            self.source,
            Transcript::ClaudeSession { .. } | Transcript::CodexRollouts { .. }
        )
    }

    /// Says whether the harness starts a turn of its own when a background command ends, as
    /// Claude Code does with its task notification; Codex only records the end, so a session
    /// holding a turn for its commands ends that turn itself. Output: the answer.
    pub fn notifies_background_end(&self) -> bool {
        matches!(self.source, Transcript::ClaudeSession { .. })
    }

    /// Counts the bytes read from the record so far, so a caller can bound a session's output.
    /// Output: the total since the reader started.
    pub fn bytes(&self) -> u64 {
        self.consumed
    }

    /// Says how long ago the harness last wrote its record, by the modification time of the
    /// file this reader holds open. Output: the time since, none before the record is open,
    /// when the time cannot be read or when it lies in the future.
    pub fn silent(&self) -> Option<Duration> {
        let written = self.file.as_ref()?.metadata().ok()?.modified().ok()?;
        written.elapsed().ok()
    }

    /// Forgets the open turn as no turn: its end, if one comes alone, is given no more. The
    /// model at work in it later, with text or a tool, starts it again, so it is followed
    /// and ends like any other. Takes the reader, which must be in a turn. Returns nothing.
    pub fn drop_turn(&mut self) {
        assert!(self.in_turn);
        self.in_turn = false;
        self.dropped = true;
    }

    /// Reads what the harness appended since the last read, at most `READ_BYTES_MAX` bytes,
    /// scanning on from where the last read stopped and draining the used lines once, so many
    /// short lines cost one pass and a poll with no new bytes scans nothing. Output: the facts
    /// in order, none while the record does not exist yet, or a plain code when a line passes
    /// `LINE_BYTES_MAX` or the record cannot be read.
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
        let mut used = 0;
        let mut scanned = self.scanned;
        while events.len() < EVENTS_PER_READ_MAX {
            let Some(length) = self.pending[scanned..]
                .iter()
                .position(|byte| *byte == b'\n')
            else {
                scanned = self.pending.len();
                break;
            };
            let line = &self.pending[used..=scanned + length];
            used = scanned + length + 1;
            scanned = used;
            let Ok(value) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            let facts = match self.source {
                Transcript::CodexRollouts { .. } => {
                    let exec = self.codex_calls.sees(&value);
                    codex_facts(&value, exec)
                }
                Transcript::ClaudeSession { .. } => self.claude_line(&value),
                Transcript::Unavailable => Vec::new(),
            };
            for fact in facts {
                let works = matches!(
                    fact,
                    TranscriptEvent::Text { .. } | TranscriptEvent::ToolUse { .. }
                );
                if works && self.dropped && !self.in_turn {
                    events.extend(self.track(TranscriptEvent::TurnStarted));
                }
                if let Some(fact) = self.track(fact) {
                    events.push(fact);
                }
            }
        }
        self.pending.drain(..used);
        self.scanned = scanned - used;
        if self.pending.len() > LINE_BYTES_MAX {
            return Err(failure("launch_transcript_line_too_long"));
        }
        assert!(self.scanned <= self.pending.len());
        assert!(events.len() >= EVENTS_PER_READ_MAX || self.scanned == self.pending.len());
        Ok(events)
    }

    /// Reads one Claude Code line with what the reader knows. The model's first line after
    /// a user line that carried text yet was no prompt, such as a typed command that runs a
    /// prompt, starts the turn that line did not, so the turn is followed and ends like any
    /// other; a sub-agent's line starts none. Takes the line. Returns its facts in order.
    fn claude_line(&mut self, value: &Value) -> Vec<TranscriptEvent> {
        let mut facts = claude_facts(value);
        let answer =
            value["type"] == "assistant" && value["isSidechain"] != true && !facts.is_empty();
        if answer && self.aside && !self.in_turn {
            facts.insert(0, TranscriptEvent::TurnStarted);
        }
        self.aside |= claude_aside(value);
        facts
    }

    fn track(&mut self, fact: TranscriptEvent) -> Option<TranscriptEvent> {
        match fact {
            TranscriptEvent::TurnStarted => {
                self.in_turn = true;
                self.aside = false;
                self.dropped = false;
                Some(fact)
            }
            TranscriptEvent::TurnFinished { .. } if !self.in_turn => {
                self.aside = false;
                None
            }
            TranscriptEvent::TurnFinished { .. } => {
                self.in_turn = false;
                self.aside = false;
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
pub(crate) fn open_plain(path: &Path) -> Option<File> {
    use std::os::windows::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(crate::platform::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

#[cfg(unix)]
pub(crate) fn open_plain(path: &Path) -> Option<File> {
    let file = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
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
pub(crate) fn plain_below(folder: &Path, record: &Path) -> bool {
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

/// Reads the task ids a Claude Code task notification says ended: each `<task-id>` of a
/// message that starts with `<task-notification>`, as Claude Code writes one, kept as
/// `conversation_id` keeps an id, at most `NOTIFIED_TASKS_MAX`. A message that only quotes
/// a notification, such as a typed coordination message, names none (DEC-119). Takes the
/// message text. Returns the ids, none for other text.
fn notified_tasks(text: &str) -> Vec<String> {
    if !text.starts_with(CLAUDE_NOTIFICATION) {
        return Vec::new();
    }
    let ids: Vec<String> = text
        .split("<task-id>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</task-id>"))
        .filter_map(|(id, _)| conversation_id(&Value::String(id.trim().to_owned())))
        .take(NOTIFIED_TASKS_MAX)
        .collect();
    assert!(ids.len() <= NOTIFIED_TASKS_MAX);
    ids
}

fn line_text(text: &str) -> TranscriptEvent {
    let mut text = text.to_owned();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    TranscriptEvent::Text { text }
}

/// The facts of one Codex rollout line. Takes the line and whether it is the output of an
/// exec tool call ([`CodexExecCalls::sees`]), the only output whose header can name a
/// command left running. Returns the facts, none for a line that says nothing the session
/// follows.
fn codex_facts(value: &Value, exec: bool) -> Vec<TranscriptEvent> {
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
const CODEX_EXEC_CALLS_MAX: usize = 64;

/// The longest Codex tool call id a reader keeps.
const CODEX_CALL_ID_BYTES_MAX: usize = 128;

/// The exec tool calls of a Codex rollout whose output is still to come, oldest first: only
/// the header Codex writes on an exec output can name a command left running, so the
/// output of any other tool, or text in an output's body, never starts one (PA9-F1).
#[derive(Debug, Default)]
struct CodexExecCalls {
    waiting: std::collections::VecDeque<String>,
}

impl CodexExecCalls {
    /// Takes one rollout line: remembers an exec_command or write_stdin call by its call
    /// id, and tells whether the line is the output of a remembered call, which it then
    /// forgets. Takes the line. Returns true only for such an output.
    fn sees(&mut self, value: &Value) -> bool {
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
fn content_texts(content: &Value) -> Vec<&str> {
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
fn claude_prompt(value: &Value, texts: &[&str]) -> bool {
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
fn claude_aside(value: &Value) -> bool {
    if value["type"] != "user" || value["isMeta"] == true {
        return false;
    }
    let texts = content_texts(&value["message"]["content"]);
    let interrupt = texts
        .iter()
        .any(|text| text.starts_with(CLAUDE_INTERRUPTED));
    !texts.is_empty() && !interrupt && !claude_prompt(value, &texts)
}

fn claude_facts(value: &Value) -> Vec<TranscriptEvent> {
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

/// Most bytes of one tool call's input or of one tool's output the session search keeps.
pub const TOOL_TEXT_BYTES_MAX: usize = 32 * 1024;
/// Most bytes of a tool value's plain lines made before the cut: the kept bytes and enough
/// past them that a cut shows, a character's bytes at most.
const FIELDS_BYTES_MAX: usize = TOOL_TEXT_BYTES_MAX + 4;

/// Reads one line of a harness conversation record for the session search: the messages
/// `conversation_messages` reads, plus each tool call with its input (Claude Code
/// `tool_use`, Codex `function_call` and `custom_tool_call`) and each tool's output
/// (`tool_result`, `function_call_output` and `custom_tool_call_output`), each cut to
/// `TOOL_TEXT_BYTES_MAX` bytes. Takes the catalog harness id and the line. Returns who said
/// it ("typed", "agent" or "tool") and the text, or none for any other line, a harness
/// without a reader or a line that is not JSON.
pub fn record_message(harness: &str, line: &[u8]) -> Option<(&'static str, String)> {
    let value = serde_json::from_slice::<Value>(line).ok()?;
    match harness {
        "codex" => codex_entry(&value),
        "claude-code" => claude_entry(&value),
        _ => None,
    }
}

fn speaker_name(speaker: Speaker) -> &'static str {
    match speaker {
        Speaker::Input => "typed",
        Speaker::Agent => "agent",
    }
}

fn codex_entry(value: &Value) -> Option<(&'static str, String)> {
    let payload = &value["payload"];
    if value["type"] != "response_item" {
        return None;
    }
    match payload["type"].as_str()? {
        "function_call" | "custom_tool_call" => {
            let input = match &payload["arguments"] {
                Value::String(text) => {
                    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone()))
                }
                Value::Null => payload["input"].clone(),
                other => other.clone(),
            };
            let name = payload["name"].as_str().unwrap_or("tool");
            Some(("agent", tool_call(name, &input)))
        }
        "function_call_output" | "custom_tool_call_output" => {
            let output = match &payload["output"] {
                Value::String(text) => match serde_json::from_str::<Value>(text) {
                    Ok(Value::Object(map)) => map
                        .get("output")
                        .and_then(Value::as_str)
                        .unwrap_or(text.as_str())
                        .to_owned(),
                    _ => text.clone(),
                },
                other => fields(other),
            };
            Some(("tool", tool_output(&output)))
        }
        _ => codex_message(value).map(|(speaker, text)| (speaker_name(speaker), text)),
    }
}

fn claude_entry(value: &Value) -> Option<(&'static str, String)> {
    let speaker = match value["type"].as_str()? {
        "user" if value["isMeta"] != true => "typed",
        "assistant" if value["isApiErrorMessage"] != true => "agent",
        _ => return None,
    };
    let mut texts: Vec<String> = claude_message(value)
        .map(|(_, text)| text)
        .into_iter()
        .collect();
    let mut output = false;
    for part in value["message"]["content"].as_array().into_iter().flatten() {
        match part["type"].as_str() {
            Some("tool_use") => {
                texts.push(tool_call(
                    part["name"].as_str().unwrap_or("tool"),
                    &part["input"],
                ));
            }
            Some("tool_result") => {
                output = true;
                let text = match &part["content"] {
                    Value::Array(items) => items
                        .iter()
                        .filter(|item| item["type"] == "text")
                        .filter_map(|item| item["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    other => fields(other),
                };
                texts.push(tool_output(&text));
            }
            _ => {}
        }
    }
    (!texts.is_empty()).then(|| (if output { "tool" } else { speaker }, texts.join("\n")))
}

fn tool_call(name: &str, input: &Value) -> String {
    format!("Tool call {name}:\n{}", capped(&fields(input)))
}

fn tool_output(text: &str) -> String {
    format!("Tool output:\n{}", capped(text))
}

/// A tool's input or output as plain lines: each field of an object as "name: value", a
/// list of strings joined by spaces, a string as it is and any other value as compact JSON,
/// so secret masking reads it line by line and word by word. Only the first
/// `FIELDS_BYTES_MAX` bytes are made, so a deep or huge value costs about its kept bytes.
/// Takes the JSON value. Returns the text, longer than `TOOL_TEXT_BYTES_MAX` only when the
/// whole text is.
fn fields(value: &Value) -> String {
    let mut text = String::new();
    push_fields(value, &mut text);
    text
}

/// Appends a tool value's plain lines to `out`, as [`fields`] makes them, stopping once
/// `out` holds `FIELDS_BYTES_MAX` bytes. Takes the value and the text to extend.
fn push_fields(value: &Value, out: &mut String) {
    match value {
        Value::Null => {}
        Value::String(text) => push_bounded(out, text),
        Value::Object(map) => {
            for (at, (key, value)) in map.iter().enumerate() {
                if out.len() >= FIELDS_BYTES_MAX {
                    return;
                }
                if at > 0 {
                    push_bounded(out, "\n");
                }
                push_bounded(out, key);
                push_bounded(out, ": ");
                push_fields(value, out);
            }
        }
        Value::Array(items) if items.iter().all(Value::is_string) => {
            for (at, item) in items.iter().filter_map(Value::as_str).enumerate() {
                if out.len() >= FIELDS_BYTES_MAX {
                    return;
                }
                if at > 0 {
                    push_bounded(out, " ");
                }
                push_bounded(out, item);
            }
        }
        other => {
            let mut json = Bounded {
                bytes: Vec::new(),
                room: FIELDS_BYTES_MAX.saturating_sub(out.len()),
            };
            let _ = serde_json::to_writer(&mut json, other);
            push_bounded(out, &String::from_utf8_lossy(&json.bytes));
        }
    }
}

/// Appends as much of `text` to `out` as fits under `FIELDS_BYTES_MAX` bytes, cut at a
/// character boundary. Takes the text to extend and the text to add.
fn push_bounded(out: &mut String, text: &str) {
    let mut end = text.len().min(FIELDS_BYTES_MAX.saturating_sub(out.len()));
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    out.push_str(&text[..end]);
}

/// Bytes a JSON value is written into, at most `room` of them; a write past that fails, so
/// serializing a huge value stops at the bound.
struct Bounded {
    bytes: Vec<u8>,
    room: usize,
}

impl std::io::Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let taken = bytes.len().min(self.room - self.bytes.len());
        if taken == 0 && !bytes.is_empty() {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        self.bytes.extend_from_slice(&bytes[..taken]);
        Ok(taken)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Cuts a tool's text to `TOOL_TEXT_BYTES_MAX` bytes at the last white space before the
/// bound, so no word and so no secret is split, and marks the cut with "...". Takes the
/// text. Returns it whole when it fits.
fn capped(text: &str) -> String {
    if text.len() <= TOOL_TEXT_BYTES_MAX {
        return text.to_owned();
    }
    let mut end = TOOL_TEXT_BYTES_MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let end = text[..end].rfind(char::is_whitespace).unwrap_or(end);
    format!("{}...", &text[..end])
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

pub mod pending;

/// Reads what was typed into a harness conversation after byte `offset` of its record, as
/// `conversation_messages` reads typed messages, from at most `EXCERPT_TAIL_BYTES_MAX`
/// bytes past the offset, never following a link. Takes the harness, the record and the
/// offset. Returns the typed texts oldest first, or none when the record cannot be read or
/// is now shorter than the offset.
pub fn typed_after(harness: &str, record: &Path, offset: u64) -> Option<Vec<String>> {
    let mut file = open_plain(record)?;
    let length = file.metadata().ok()?.len();
    if length < offset {
        return None;
    }
    let mut read = Vec::new();
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.take(EXCERPT_TAIL_BYTES_MAX)
        .read_to_end(&mut read)
        .ok()?;
    let mut typed = Vec::new();
    for line in read.split(|byte| *byte == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let said = match harness {
            "codex" => codex_message(&value),
            "claude-code" => claude_message(&value),
            _ => None,
        };
        if let Some((Speaker::Input, text)) = said {
            typed.push(text);
        }
    }
    Some(typed)
}

/// Reads what was typed into a harness conversation after byte `offset` of its record,
/// with where each typed line ends, so a caller can read on from one typed message to the
/// next. It reads on to the record's end as it was when opened, `TYPED_CHUNK_BYTES` at a
/// time and never following a link, so a turn that wrote any amount to the record hides no
/// typed line behind it. Only lines the harness finished are read. A line longer than
/// `LINE_BYTES_MAX`, which the turn reader refuses too, is passed over unread. A Claude
/// Code line is typed only when it is a prompt that starts a turn (`claude_prompt`), so a
/// compact summary, a line only the transcript shows or a sub-agent's line is never read
/// as a turn's words. Reading stops early once `EXCERPT_MESSAGES_MAX` typed texts or more
/// than `LINE_BYTES_MAX` bytes of them are held. Takes the harness, the record and the
/// offset. Returns the typed texts oldest first, each with the byte offset just past its
/// line, and the offset just past the last line read, the record's last finished line
/// when no typed text was found; none when the record cannot be read or is now shorter
/// than the offset.
pub fn typed_past(harness: &str, record: &Path, offset: u64) -> Option<(Vec<(u64, String)>, u64)> {
    let mut file = open_plain(record)?;
    let length = file.metadata().ok()?.len();
    if length < offset {
        return None;
    }
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut typed: Vec<(u64, String)> = Vec::new();
    let mut held = 0;
    let mut line: Vec<u8> = Vec::new();
    let mut long = false;
    let (mut at, mut end) = (offset, offset);
    let mut chunk = vec![0; TYPED_CHUNK_BYTES];
    'record: while at < length {
        let left = usize::try_from(length - at).unwrap_or(usize::MAX);
        let count = file.read(&mut chunk[..left.min(TYPED_CHUNK_BYTES)]).ok()?;
        if count == 0 {
            break;
        }
        for piece in chunk[..count].split_inclusive(|byte| *byte == b'\n') {
            at += u64::try_from(piece.len()).ok()?;
            if long || line.len() + piece.len() > LINE_BYTES_MAX {
                long = true;
                line.clear();
            } else {
                line.extend_from_slice(piece);
            }
            if piece.last() != Some(&b'\n') {
                continue;
            }
            end = at;
            let text = (!long).then(|| typed_line(harness, &line)).flatten();
            long = false;
            line.clear();
            if let Some(text) = text {
                held += text.len();
                typed.push((end, text));
                if typed.len() >= EXCERPT_MESSAGES_MAX || held > LINE_BYTES_MAX {
                    break 'record;
                }
            }
        }
    }
    assert!(offset <= end && end <= at && at <= length);
    assert!(typed.len() <= EXCERPT_MESSAGES_MAX);
    Some((typed, end))
}

/// Reads one finished line of a harness record as a typed message, as `typed_past` counts
/// one. A line without the word `"user"` cannot be typed and is not parsed, so the many
/// long lines a turn writes cost one scan each. Takes the harness and the line. Returns
/// the typed text, or none for any other line.
fn typed_line(harness: &str, line: &[u8]) -> Option<String> {
    const USER: &[u8] = b"\"user\"";
    if !line.windows(USER.len()).any(|part| part == USER) {
        return None;
    }
    let value = serde_json::from_slice::<Value>(line).ok()?;
    let said = match harness {
        "codex" => codex_message(&value),
        "claude-code" if claude_prompt(&value, &content_texts(&value["message"]["content"])) => {
            claude_message(&value)
        }
        _ => None,
    };
    match said {
        Some((Speaker::Input, text)) => Some(text),
        _ => None,
    }
}

#[cfg(all(test, unix))]
mod tests;
