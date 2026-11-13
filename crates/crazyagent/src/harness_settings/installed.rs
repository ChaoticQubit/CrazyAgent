use super::VERSION_BYTES_MAX;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

/// How long one harness has to print its version.
pub const LIMIT: Duration = Duration::from_secs(5);

/// The most executable files whose version the engine keeps.
pub const KNOWN_MAX: usize = 16;

const OUTPUT_BYTES_MAX: u64 = 4096;
const POLL: Duration = Duration::from_millis(20);

static KNOWN: Mutex<BTreeMap<Identity, String>> = Mutex::new(BTreeMap::new());

/// One executable file as the engine last saw it: its path, size, modification time
/// and, on Unix, its device, inode and status change time, so a replaced or rewritten
/// file is a new identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Identity {
    path: PathBuf,
    len: u64,
    modified: SystemTime,
    file: (u64, u64, i64, i64),
}

/// Asks an executable for its version once per file identity, as `version_of` does, for
/// the engine's lifetime. A failed ask, or one during which the file changed, is not
/// kept, so the next start asks again. Inputs: the executable and the limit. Output: the
/// version, or none.
pub fn version_cached(executable: &Path, limit: Duration) -> Option<String> {
    let before = identity(executable)?;
    if let Some(version) = known().get(&before) {
        return Some(version.clone());
    }
    let version = version_of(executable, limit)?;
    if identity(executable).as_ref() != Some(&before) {
        return Some(version);
    }
    let mut kept = known();
    if kept.len() >= KNOWN_MAX {
        kept.clear();
    }
    kept.insert(before, version.clone());
    assert!(kept.len() <= KNOWN_MAX);
    Some(version)
}

/// Asks an executable for its version for a Models read. Inputs: the executable, the
/// limit, the backoff and the time now. Output: the version, or none.
pub fn version_for_models(
    executable: &Path,
    limit: Duration,
    backoff: Duration,
    now: Instant,
) -> Option<String> {
    let _ = (backoff, now);
    version_cached(executable, limit)
}

fn known() -> MutexGuard<'static, BTreeMap<Identity, String>> {
    KNOWN.lock().unwrap_or_else(PoisonError::into_inner)
}

fn identity(executable: &Path) -> Option<Identity> {
    let metadata = std::fs::metadata(executable).ok()?;
    Some(Identity {
        path: executable.to_path_buf(),
        len: metadata.len(),
        modified: metadata.modified().ok()?,
        file: file_id(&metadata),
    })
}

#[cfg(unix)]
fn file_id(metadata: &std::fs::Metadata) -> (u64, u64, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

/// Gives no file identity beyond path, size and modification time where the platform
/// has none. Input: the metadata, unused. Output: zeros.
#[cfg(not(unix))]
fn file_id(_metadata: &std::fs::Metadata) -> (u64, u64, i64, i64) {
    (0, 0, 0, 0)
}

/// Asks one engine-verified harness executable for its version: `--version` with no
/// input, a cleared environment holding only a system PATH, and a time limit. It runs in
/// its own process group, which is killed when it runs past the limit. Its output is read
/// without waiting once it exits, so a child it left behind cannot hold the reader.
/// Inputs: the executable and the limit. Output: the version, or none when it does not
/// start, fails, runs past the limit or prints no version.
pub fn version_of(executable: &Path, limit: Duration) -> Option<String> {
    let mut command = Command::new(executable);
    command
        .arg("--version")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    own_group(&mut command);
    let mut child = crate::spawn_gate::spawn(&mut command).ok()?;
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            _ => {
                stop(&mut child);
                return None;
            }
        }
    };
    let stdout = child.stdout.take()?;
    if !status.success() || !nonblocking(&stdout) {
        return None;
    }
    let mut output = Vec::new();
    let _ = stdout.take(OUTPUT_BYTES_MAX).read_to_end(&mut output);
    assert!(output.len() as u64 <= OUTPUT_BYTES_MAX);
    version_from(&output)
}

#[cfg(unix)]
fn own_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// Leaves the probe in this process's group where the platform has none of its own.
/// Input: the command, unchanged.
#[cfg(not(unix))]
fn own_group(_command: &mut Command) {}

/// Kills a probe that is not yet reaped and its whole process group, then reaps it.
/// Input: the probe, which `own_group` started as its group's leader.
#[cfg(unix)]
fn stop(child: &mut Child) {
    use rustix::process::{Pid, Signal, kill_process_group};
    if let Some(group) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) {
        assert!(group.as_raw_nonzero().get() > 1);
        let _ = kill_process_group(group, Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Kills a probe that is not yet reaped, then reaps it. Input: the probe.
#[cfg(not(unix))]
fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn nonblocking(stdout: &std::process::ChildStdout) -> bool {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
    fcntl_getfl(stdout).is_ok_and(|flags| fcntl_setfl(stdout, flags | OFlags::NONBLOCK).is_ok())
}

/// Refuses to read a version where the pipe cannot be read without waiting. Takes the
/// child's output, unused. Returns false, so the version stays unknown.
#[cfg(not(unix))]
fn nonblocking(_stdout: &std::process::ChildStdout) -> bool {
    false
}

/// Reads the version a harness printed, such as `2.0.31 (Claude Code)` or
/// `codex-cli 0.50.0`. Input: the output. Output: the first word of its first line that
/// starts with a digit, when it is a valid version.
pub fn version_from(output: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(output).ok()?;
    let word = text
        .lines()
        .next()?
        .split_whitespace()
        .find(|word| word.starts_with(|c: char| c.is_ascii_digit()))?;
    version_valid(word).then(|| word.to_owned())
}

/// Checks one version text: 1 to `VERSION_BYTES_MAX` bytes, a leading digit, then ASCII
/// letters, digits, `.`, `-` and `+`.
pub fn version_valid(text: &str) -> bool {
    text.len() <= VERSION_BYTES_MAX
        && text.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'+')
}
