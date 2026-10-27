use super::failure;
use crate::engine::EngineError;
use std::io::Write;
use std::path::Path;

/// How many of the last raw stream bytes a master host keeps to name its last frames for the
/// engine log; the bytes themselves are never written there.
pub(in crate::engine) const TAIL_BYTES: usize = 8192;
/// How many lines after a turn's result a host names by kind; later ones are only counted.
#[cfg(unix)]
const LATE_KINDS_MAX: usize = 16;
/// The longest stream type, subtype or method name a host copies into the engine log.
const KIND_BYTES_MAX: usize = 64;
/// The engine log of Claude Code master stream diagnostics, in the engine logs folder.
pub(in crate::engine) const LOG_FILE: &str = "master-stream.log";
/// The size the log never passes: a log that a new entry would take past it is first moved
/// to `master-stream.log.1`, replacing the older copy.
pub(in crate::engine) const ROTATE_BYTES: u64 = 256 * 1024;

/// What a master host kept about its stream for the engine log: the fault that retired it,
/// the line or frame that fault was found on, the kinds and count of the lines that came
/// after a Claude Code turn's result, and the last raw stream bytes, which only name the
/// last frames by kind and size.
#[derive(Clone, Debug, Default)]
pub(in crate::engine) struct StreamDiagnostic {
    pub(in crate::engine) fault: Option<String>,
    pub(in crate::engine) message: Option<String>,
    pub(in crate::engine) late: Vec<String>,
    pub(in crate::engine) late_lines: usize,
    tail: Vec<u8>,
    cut: bool,
}

impl StreamDiagnostic {
    /// Keeps the last `TAIL_BYTES` of the raw stream and notes when older bytes were
    /// dropped. Input: the bytes as read from the harness. Output: none.
    pub(in crate::engine) fn keep(&mut self, bytes: &[u8]) {
        self.tail.extend_from_slice(bytes);
        if self.tail.len() > TAIL_BYTES {
            let excess = self.tail.len() - TAIL_BYTES;
            self.tail.drain(..excess);
            self.cut = true;
        }
        assert!(self.tail.len() <= TAIL_BYTES);
    }

    /// Counts one line that came after the turn's result and names its kind while fewer than
    /// `LATE_KINDS_MAX` are named. Input: the line. Output: none.
    #[cfg(unix)]
    pub(in crate::engine) fn late(&mut self, line: &[u8]) {
        self.late_lines = self.late_lines.saturating_add(1);
        if self.late.len() < LATE_KINDS_MAX {
            self.late.push(kind(line));
        }
        assert!(self.late.len() <= LATE_KINDS_MAX);
    }

    /// Names the kind of the first line the protocol refused. Input: the line. Output: none.
    #[cfg(unix)]
    pub(in crate::engine) fn failed(&mut self, line: &[u8]) {
        if self.message.is_none() {
            self.message = Some(kind(line));
        }
    }

    /// Names each line of the kept stream bytes by its kind and size, never by its content,
    /// so tool output and credentials in the stream stay out of the engine log: `cut` for a
    /// first line whose start was dropped, `unfinished` for a last line with no newline yet.
    /// Input: none. Output: one `kind bytes` entry per line, oldest first.
    pub(in crate::engine) fn frames(&self) -> Vec<String> {
        let mut frames = Vec::new();
        let mut lines = self.tail.split(|byte| *byte == b'\n').peekable();
        while let Some(line) = lines.next() {
            let last = lines.peek().is_none();
            if last && line.is_empty() {
                break;
            }
            let name = if frames.is_empty() && self.cut {
                "cut".to_owned()
            } else if last {
                "unfinished".to_owned()
            } else {
                kind(line)
            };
            frames.push(format!("{name} {}", line.len()));
        }
        assert!(frames.len() <= self.tail.len() + 1);
        frames
    }
}

/// Names a stream line by its protocol kind: a Claude Code line by its `type` and, when it
/// has one, its `subtype`, such as `system/task_notification`; a Codex JSON-RPC frame by its
/// `method`, or `response` or `error`. Input: the line. Output: the name, with `unreadable`
/// for a line or field that is not a short JSON name of letters, `_` and, in a method, `/`.
fn kind(line: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
        return "unreadable".to_owned();
    };
    let name = |field: &str, method: bool| {
        value.get(field).map(|value| {
            value
                .as_str()
                .filter(|name| {
                    (1..=KIND_BYTES_MAX).contains(&name.len())
                        && name.bytes().all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte == b'_'
                                || (method && (byte.is_ascii_uppercase() || byte == b'/'))
                        })
                })
                .unwrap_or("unreadable")
        })
    };
    match (name("type", false), name("subtype", false)) {
        (Some(kind), Some(subtype)) => return format!("{kind}/{subtype}"),
        (Some(kind), None) => return kind.to_owned(),
        (None, _) => {}
    }
    if let Some(method) = name("method", true) {
        return method.to_owned();
    }
    match (value.get("id"), value.get("error")) {
        (Some(_), Some(_)) => "error".to_owned(),
        (Some(_), None) => "response".to_owned(),
        (None, _) => "unreadable".to_owned(),
    }
}

/// Appends one entry as a JSON line to the master stream log, creating the logs folder
/// private (0700) and the log 0600 when missing and rotating a full log first; a log or
/// folder that is a link, shared or not this user's is refused. Input: the engine logs
/// folder and the entry. Output: an error code for an unsafe path or a failed write.
pub(in crate::engine) fn append(logs: &Path, entry: &serde_json::Value) -> Result<(), EngineError> {
    let mut line = serde_json::to_vec(entry).map_err(|_| failure("master_stream_log_entry"))?;
    line.push(b'\n');
    let length = u64::try_from(line.len()).map_err(|_| failure("master_stream_log_entry"))?;
    if length > ROTATE_BYTES {
        return Err(failure("master_stream_log_entry"));
    }
    private_folder(logs)?;
    let log = logs.join(LOG_FILE);
    if let Ok(metadata) = std::fs::symlink_metadata(&log)
        && metadata.is_file()
        && metadata.len().saturating_add(length) > ROTATE_BYTES
    {
        std::fs::rename(&log, logs.join(format!("{LOG_FILE}.1")))
            .map_err(|_| failure("master_stream_log_unavailable"))?;
    }
    let mut file = open(&log)?;
    file.write_all(&line)
        .map_err(|_| failure("master_stream_log_unavailable"))
}

#[cfg(unix)]
fn private_folder(logs: &Path) -> Result<(), EngineError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::DirBuilder::new().mode(0o700).create(logs) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(failure("master_stream_log_unavailable")),
    }
    let metadata =
        std::fs::symlink_metadata(logs).map_err(|_| failure("master_stream_log_unavailable"))?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(failure("master_stream_log_unsafe"));
    }
    Ok(())
}

#[cfg(unix)]
fn open(log: &Path) -> Result<std::fs::File, EngineError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(log)
        .map_err(|error| {
            if error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) {
                failure("master_stream_log_unsafe")
            } else {
                failure("master_stream_log_unavailable")
            }
        })?;
    let metadata = file
        .metadata()
        .map_err(|_| failure("master_stream_log_unavailable"))?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(failure("master_stream_log_unsafe"));
    }
    Ok(file)
}

/// Refuses the logs folder where its Unix owner and mode cannot be checked. Takes the
/// folder, unused. Returns `master_stream_log_unavailable`.
#[cfg(not(unix))]
fn private_folder(_logs: &Path) -> Result<(), EngineError> {
    Err(failure("master_stream_log_unavailable"))
}

/// Refuses the log where its Unix owner and mode cannot be checked. Takes the log path,
/// unused. Returns `master_stream_log_unavailable`.
#[cfg(not(unix))]
fn open(_log: &Path) -> Result<std::fs::File, EngineError> {
    Err(failure("master_stream_log_unavailable"))
}
