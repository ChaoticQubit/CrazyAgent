use super::VERSION_BYTES_MAX;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long one harness has to print its version.
pub const LIMIT: Duration = Duration::from_secs(5);

/// The most executable contents whose version the engine keeps.
pub const KNOWN_MAX: usize = 16;

const OUTPUT_BYTES_MAX: u64 = 4096;
const POLL: Duration = Duration::from_millis(20);

static KNOWN: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// Asks an executable whose content the engine verified for its version once per content,
/// as `version_of` does, since a master starts a harness every turn. A failed ask is not
/// kept, so the next start asks again. Inputs: the executable, the SHA-256 of its content
/// and the limit. Output: the version, or none.
pub fn version_of_digest(executable: &Path, digest: &str, limit: Duration) -> Option<String> {
    let known = |digest: &str| {
        KNOWN
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(digest)
            .cloned()
    };
    if let Some(version) = known(digest) {
        return Some(version);
    }
    let version = version_of(executable, limit)?;
    let mut kept = KNOWN.lock().unwrap_or_else(PoisonError::into_inner);
    if kept.len() >= KNOWN_MAX {
        kept.clear();
    }
    kept.insert(digest.to_owned(), version.clone());
    assert!(kept.len() <= KNOWN_MAX);
    Some(version)
}

/// Asks one engine-verified harness executable for its version: `--version` with no
/// input, a cleared environment holding only a system PATH, and a time limit. Its output
/// is read without waiting once it exits, so a child it left behind cannot hold the
/// reader. Inputs: the executable and the limit. Output: the version, or none when it
/// does not start, fails, runs past the limit or prints no version.
pub fn version_of(executable: &Path, limit: Duration) -> Option<String> {
    let mut command = Command::new(executable);
    command
        .arg("--version")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    let mut child = crate::spawn_gate::spawn(&mut command).ok()?;
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
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
