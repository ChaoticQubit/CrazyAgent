use serde::{Deserialize, Serialize};
#[cfg(unix)]
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::ffi::OsString;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const VERSION_BYTES_MAX: usize = 32;
/// The most standard output [`run_command`] keeps from one program.
#[cfg(unix)]
pub const OUTPUT_BYTES_MAX: usize = 64 * 1024;
const BOUNDED_BYTES_MAX: u64 = 4096;
const POLL: Duration = Duration::from_millis(50);
#[cfg(unix)]
const PAUSE_MIN: Duration = Duration::from_millis(1);
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A plain reason setup could not find or install something, as a stable code such as
/// `setup_checksum_mismatch`. `plain_reason` turns the code into the sentence setup shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallError(pub &'static str);

impl std::fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for InstallError {}

/// A tool version as the tool prints it, such as `3.5a` or `0.9.9`: 1 to 32 characters
/// of letters, digits, `.`, `-` and `+`, starting with a letter or digit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version(String);

impl Version {
    /// Checks one version text. Input: the text. Output: the version, or
    /// `setup_version_unreadable` when it is empty, too long or has other characters.
    pub fn parse(text: &str) -> Result<Self, InstallError> {
        let valid = !text.is_empty()
            && text.len() <= VERSION_BYTES_MAX
            && text
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric())
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'+');
        if !valid {
            return Err(InstallError("setup_version_unreadable"));
        }
        Ok(Self(text.to_owned()))
    }

    /// Returns the version text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Version {
    type Error = InstallError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}

impl From<Version> for String {
    fn from(version: Version) -> Self {
        version.0
    }
}

/// Runs one setup command with no input and a time limit. Input: the command, the limit,
/// and whether to keep its standard output, which is only for short version reports;
/// install output goes nowhere, so it can never fill a pipe. Output: up to 4 KiB of
/// standard output, `setup_timeout` after killing it at the limit or when its output is
/// still open there, or `setup_install_failed` when it cannot start or exits with a failure.
/// On Unix the command leads its own process group, and every path that stops it at the
/// limit or on a failure kills that group, so a process it left holding its output ends
/// too; a clean exit inside the limit leaves the group alone.
pub fn run_bounded(
    command: &mut Command,
    limit: Duration,
    capture: bool,
) -> Result<Vec<u8>, InstallError> {
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child =
        crate::spawn_gate::spawn(command).map_err(|_| InstallError("setup_install_failed"))?;
    #[cfg(unix)]
    let pid = child.id();
    let deadline = Instant::now() + limit;
    let report = match child.stdout.take().map(read_report).transpose() {
        Ok(report) => report,
        Err(failed) => {
            #[cfg(unix)]
            stop(pid);
            let _ = child.kill();
            let _ = child.wait();
            return Err(failed);
        }
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            outcome => {
                #[cfg(unix)]
                stop(pid);
                let _ = child.kill();
                let _ = child.wait();
                return Err(InstallError(if outcome.is_ok() {
                    "setup_timeout"
                } else {
                    "setup_install_failed"
                }));
            }
        }
    };
    if !status.success() {
        return Err(InstallError("setup_install_failed"));
    }
    let Some(report) = report else {
        return Ok(Vec::new());
    };
    let left = deadline.saturating_duration_since(Instant::now());
    let output = match report.recv_timeout(left) {
        Ok(read) => read.map_err(|_| InstallError("setup_version_unreadable"))?,
        Err(_) => {
            #[cfg(unix)]
            stop(pid);
            return Err(InstallError("setup_timeout"));
        }
    };
    assert!(output.len() as u64 <= BOUNDED_BYTES_MAX);
    Ok(output)
}

/// Reads a command's report on its own thread while the command runs: the first
/// `BOUNDED_BYTES_MAX` bytes are kept and the rest is drained, so neither a full pipe nor a
/// process that keeps the pipe open past the command's exit holds [`run_bounded`] past its
/// limit. Input: the command's standard output. Output: where the read arrives, or
/// `setup_install_failed` when the thread cannot start.
fn read_report(
    stdout: std::process::ChildStdout,
) -> Result<std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>, InstallError> {
    let (sent, report) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("crazyagent-setup-report".into())
        .spawn(move || {
            let mut kept = Vec::new();
            let mut limited = stdout.take(BOUNDED_BYTES_MAX);
            let read = limited.read_to_end(&mut kept).map(|_| kept);
            let _ = std::io::copy(&mut limited.into_inner(), &mut std::io::sink());
            let _ = sent.send(read);
        })
        .map_err(|_| InstallError("setup_install_failed"))?;
    Ok(report)
}

/// Reads a tool's version from its version report, such as `tmux 3.4` or
/// `uv 0.9.5 (0a1b2c3 2025-10-01)`: the first word holding a digit, without a leading `v`.
/// Output: the version, or `setup_version_unreadable`.
pub fn reported_version(output: &[u8]) -> Result<Version, InstallError> {
    let unreadable = InstallError("setup_version_unreadable");
    let text = std::str::from_utf8(output).map_err(|_| unreadable)?;
    let word = text
        .split_whitespace()
        .find(|word| word.bytes().any(|b| b.is_ascii_digit()))
        .ok_or(unreadable)?;
    let word = word
        .strip_prefix('v')
        .filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(word);
    Version::parse(word)
}

#[cfg(unix)]
/// Runs one program with only the given environment, no input and a time bound, then
/// stops its whole process group. Inputs: the absolute program, its arguments, the
/// environment pairs, the working folder and the bound. Output: whether it exited with
/// success and at most `OUTPUT_BYTES_MAX` bytes of its standard output, or `setup_timeout`.
pub fn run(
    program: &Path,
    args: &[OsString],
    env: &[(&str, OsString)],
    cwd: &Path,
    limit: Duration,
) -> Result<(bool, Vec<u8>), InstallError> {
    assert!(program.is_absolute());
    let mut command = Command::new(program);
    command.args(args).env_clear().current_dir(cwd);
    for (name, value) in env {
        command.env(name, value);
    }
    run_command(command, limit)
}

#[cfg(unix)]
/// Runs one prepared command, such as a sandboxed one, with no input and a time bound, then
/// stops its whole process group. It wakes when the output closes, so a quick program is
/// not held for a whole poll. Inputs: the command and the bound. Output: whether it exited
/// with success and at most `OUTPUT_BYTES_MAX` bytes of its standard output, or
/// `setup_timeout`.
pub fn run_command(mut command: Command, limit: Duration) -> Result<(bool, Vec<u8>), InstallError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child =
        crate::spawn_gate::spawn(&mut command).map_err(|_| InstallError("setup_install_failed"))?;
    let pid = child.id();
    let stdout = child.stdout.take();
    let (finished, read_to_end) = std::sync::mpsc::channel::<()>();
    let reader = std::thread::Builder::new()
        .name("crazyagent-setup-output".into())
        .spawn(move || {
            let mut kept = Vec::new();
            if let Some(stdout) = stdout {
                let mut limited = stdout.take(OUTPUT_BYTES_MAX as u64);
                let _ = limited.read_to_end(&mut kept);
                let _ = std::io::copy(&mut limited.into_inner(), &mut std::io::sink());
            }
            let _ = finished.send(());
            kept
        });
    let deadline = Instant::now() + limit;
    let mut reading = reader.is_ok();
    let mut pause = PAUSE_MIN;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break Err(InstallError("setup_timeout"));
                }
                if reading {
                    reading = matches!(
                        read_to_end.recv_timeout(POLL.min(left)),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    );
                } else {
                    std::thread::sleep(pause.min(left));
                    pause = (pause * 2).min(POLL);
                }
            }
            Err(_) => break Err(InstallError("setup_install_failed")),
        }
    };
    stop(pid);
    let _ = child.kill();
    let _ = child.wait();
    let output = match reader {
        Ok(reader) => reader.join().unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    assert!(output.len() <= OUTPUT_BYTES_MAX);
    Ok((status?.success(), output))
}

#[cfg(unix)]
fn stop(pid: u32) {
    if let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        assert!(pid.as_raw_nonzero().get() > 1);
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
}

#[cfg(unix)]
/// Reads the version a program printed for `--version` or `-V`, such as `uv 0.9.5 (...)`.
/// Input: the program's output. Output: the second word of its first line as a version.
pub fn version_from(output: &[u8]) -> Result<Version, InstallError> {
    let text = std::str::from_utf8(output).map_err(|_| InstallError("setup_version_unreadable"))?;
    let line = text.lines().next().unwrap_or_default();
    let word = line
        .split_whitespace()
        .nth(1)
        .ok_or(InstallError("setup_version_unreadable"))?;
    Version::parse(word)
}

#[cfg(unix)]
/// Returns the lowercase hex SHA-256 of one file, read as a stream.
/// Input: the file path. Output: 64 hex characters, or `setup_store_unavailable`.
pub fn file_sha256(path: &Path) -> Result<String, InstallError> {
    let mut file =
        std::fs::File::open(path).map_err(|_| InstallError("setup_store_unavailable"))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|_| InstallError("setup_store_unavailable"))?;
    let text: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(text.len(), 64);
    Ok(text)
}

#[cfg(unix)]
/// Tells whether a text is exactly 64 lowercase hex characters, the shape of a SHA-256.
pub fn is_sha256(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(unix)]
/// Creates one folder owned by this user with mode 0700, or checks an existing one.
/// Input: the folder path. Output: `setup_store_unavailable` when it can't be created or
/// is a link, another user's folder or open to others.
pub fn private_folder(path: &Path) -> Result<(), InstallError> {
    let unavailable = InstallError("setup_store_unavailable");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|_| unavailable)?;
    let metadata = std::fs::symlink_metadata(path).map_err(|_| unavailable)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(unavailable);
    }
    Ok(())
}

#[cfg(unix)]
/// Makes a fresh private staging folder inside a parent, named from the caller's random id.
/// Inputs: the parent folder, a name prefix and an ASCII id of at least 24 characters, of
/// which the first 24 name the folder. Output: the new folder path, or
/// `setup_store_unavailable` when the parent is not private or the folder already exists.
pub fn staging_folder(parent: &Path, prefix: &str, id: &str) -> Result<PathBuf, InstallError> {
    assert!(id.len() >= 24 && id.is_ascii());
    private_folder(parent)?;
    let path = parent.join(format!(".{prefix}-{}", &id[..24]));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .map_err(|_| InstallError("setup_store_unavailable"))?;
    Ok(path)
}

#[cfg(test)]
mod tests;
