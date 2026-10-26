use super::failure;
use crate::engine::EngineError;
use std::io::Write;
use std::path::Path;

/// How many of the last raw stream bytes a Claude Code master host keeps for the engine log.
pub(in crate::engine) const TAIL_BYTES: usize = 8192;
/// How many lines after a turn's result a host names by kind; later ones are only counted.
const LATE_KINDS_MAX: usize = 16;
/// The longest stream type or subtype name a host copies into the engine log.
const KIND_BYTES_MAX: usize = 64;
/// The engine log of Claude Code master stream diagnostics, in the engine logs folder.
pub(in crate::engine) const LOG_FILE: &str = "master-stream.log";
/// The size the log never passes: a log that a new entry would take past it is first moved
/// to `master-stream.log.1`, replacing the older copy.
pub(in crate::engine) const ROTATE_BYTES: u64 = 256 * 1024;
const CREDENTIAL_PREFIX: &[u8] = b"sk-ant-";
const REDACTED: &[u8] = b"[redacted]";

/// What a Claude Code master host kept about its stream for the engine log: the fault that
/// retired it, the type and subtype of the line that fault was found on, the kinds and count
/// of the lines that came after its turn's result, and the last raw stream bytes.
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
    pub(in crate::engine) fn late(&mut self, line: &[u8]) {
        self.late_lines = self.late_lines.saturating_add(1);
        if self.late.len() < LATE_KINDS_MAX {
            self.late.push(kind(line));
        }
        assert!(self.late.len() <= LATE_KINDS_MAX);
    }

    /// Names the kind of the first line the protocol refused. Input: the line. Output: none.
    pub(in crate::engine) fn failed(&mut self, line: &[u8]) {
        if self.message.is_none() {
            self.message = Some(kind(line));
        }
    }

    /// The kept stream bytes as text with every Claude credential redacted. When older bytes
    /// were dropped, the leading run of credential characters goes with them, so a credential
    /// cut before its prefix never shows. Input: none. Output: the text.
    pub(in crate::engine) fn tail(&self) -> String {
        let mut start = 0;
        if self.cut {
            while start < self.tail.len() && credential_byte(self.tail[start]) {
                start += 1;
            }
        }
        let bytes = &self.tail[start..];
        let mut text = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index..].starts_with(CREDENTIAL_PREFIX) {
                text.extend_from_slice(CREDENTIAL_PREFIX);
                text.extend_from_slice(REDACTED);
                index += CREDENTIAL_PREFIX.len();
                while index < bytes.len() && credential_byte(bytes[index]) {
                    index += 1;
                }
            } else {
                text.push(bytes[index]);
                index += 1;
            }
        }
        assert!(index == bytes.len());
        String::from_utf8_lossy(&text).into_owned()
    }
}

fn credential_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

/// Names a stream line by its `type` and, when it has one, its `subtype`, such as
/// `system/task_notification`. Input: the line. Output: the name, with `unreadable` for a
/// line or field that is not a short lowercase JSON name.
fn kind(line: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
        return "unreadable".to_owned();
    };
    let name = |field: &str| {
        value.get(field).map(|value| {
            value
                .as_str()
                .filter(|name| {
                    (1..=KIND_BYTES_MAX).contains(&name.len())
                        && name
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
                })
                .unwrap_or("unreadable")
        })
    };
    match (name("type"), name("subtype")) {
        (Some(kind), Some(subtype)) => format!("{kind}/{subtype}"),
        (Some(kind), None) => kind.to_owned(),
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
