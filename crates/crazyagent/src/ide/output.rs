use std::fs;
use std::io::{self, BufRead, BufReader, PipeWriter, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// The most bytes one launch's log keeps; later output is still read and dropped, so the
/// server never blocks on a full pipe.
pub const LOG_BYTES_MAX: u64 = 4 * 1024 * 1024;

/// What a token in a logged line is replaced with.
pub const REDACTED: &str = "<redacted>";

/// The line logged in place of a line longer than [`LINE_BYTES_MAX`].
pub const CUT: &str = "<line cut>\n";

/// The longest output line that is logged; a longer one is dropped whole, so no part of a
/// token is ever logged from a line cut in two.
pub const LINE_BYTES_MAX: usize = 16 * 1024;

const QUERY: &str = "tkn=";

/// Redacts one output line before it is logged: every copy of the launch token, and the
/// value of every `tkn=` parameter or `vscode-tkn=` cookie, become [`REDACTED`]. The server
/// prints its full workbench URL with the token. Inputs: the line and the launch token.
/// Output: the line to log.
pub fn redact(line: &str, token: &str) -> String {
    assert!(!token.is_empty());
    let line = line.replace(token, REDACTED);
    let mut redacted = String::with_capacity(line.len());
    let mut rest = line.as_str();
    while let Some(at) = rest.find(QUERY) {
        let (before, after) = rest.split_at(at + QUERY.len());
        redacted.push_str(before);
        let value = after
            .find(|character: char| {
                !(character.is_ascii_alphanumeric() || matches!(character, '%' | '-' | '_'))
            })
            .unwrap_or(after.len());
        if value > 0 {
            redacted.push_str(REDACTED);
        }
        rest = &after[value..];
    }
    redacted.push_str(rest);
    assert!(!redacted.contains(token));
    redacted
}

/// Copies a server's output into its log line by line, each line through [`redact`], until
/// every writer has closed. At most [`LOG_BYTES_MAX`] bytes are kept and a line longer
/// than [`LINE_BYTES_MAX`] is logged as [`CUT`]; output past the cap, or after the log fails,
/// is read and dropped. Inputs: the output, the log and the launch token. Output: the
/// bytes logged.
pub fn copy(output: impl Read, mut log: impl Write, token: &str) -> u64 {
    let mut reader = BufReader::new(output);
    let mut line = Vec::with_capacity(1024);
    let mut kept: u64 = 0;
    let mut writable = true;
    loop {
        line.clear();
        let Ok(read) = (&mut reader)
            .take(LINE_BYTES_MAX as u64 + 1)
            .read_until(b'\n', &mut line)
        else {
            break;
        };
        if read == 0 {
            break;
        }
        let text = if line.len() > LINE_BYTES_MAX && line.last() != Some(&b'\n') {
            if skip_line(&mut reader).is_err() {
                break;
            }
            CUT.to_owned()
        } else {
            redact(&String::from_utf8_lossy(&line), token)
        };
        let size = text.len() as u64;
        if writable && kept + size <= LOG_BYTES_MAX {
            writable = log.write_all(text.as_bytes()).is_ok();
            kept += if writable { size } else { 0 };
        }
    }
    let _ = log.flush();
    assert!(kept <= LOG_BYTES_MAX);
    kept
}

/// Opens a launch's log emptied, mode 0600, never through a link, and starts the thread that
/// fills it through [`copy`] from a new pipe. The pipe is made while no child starts, so
/// only the server inherits its write end. Inputs: the log path and the launch token.
/// Output: the write end for the server's stdout and stderr.
pub fn start(path: &Path, token: &str) -> io::Result<PipeWriter> {
    let log = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    log.set_permissions(fs::Permissions::from_mode(0o600))?;
    let (reader, writer) = crate::spawn_gate::guarded(io::pipe)?;
    let token = token.to_owned();
    let _detached = std::thread::Builder::new()
        .name("ide-server-log".to_owned())
        .spawn(move || copy(reader, log, &token))?;
    Ok(writer)
}

fn skip_line(reader: &mut impl BufRead) -> io::Result<()> {
    let mut scratch = Vec::with_capacity(1024);
    loop {
        scratch.clear();
        let read = (&mut *reader)
            .take(LINE_BYTES_MAX as u64)
            .read_until(b'\n', &mut scratch)?;
        if read == 0 || scratch.last() == Some(&b'\n') {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests;
