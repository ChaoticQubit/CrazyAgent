use super::{Fixed, OUTPUT_BYTES_MAX, POINTER_BYTES_MAX, Pointer};
use std::path::Path;
use std::time::Duration;

pub const SHOWN_BYTES_MAX: usize = 4 * 1024;
pub const VERSION_TIME: Duration = Duration::from_secs(10);
pub const CHECK_TIME: Duration = Duration::from_secs(60);
pub const UPDATE_TIME: Duration = Duration::from_secs(15 * 60);
pub const POINTER_TIME: Duration = Duration::from_secs(10);

pub const LOOKUP_FAILED: &str = "harness_update_lookup_failed";
pub const TIMEOUT: &str = "harness_update_timeout";
pub const AS_ROOT: &str = "harness_update_as_root";
pub const START_FAILED: &str = "harness_update_start_failed";

pub(super) const STREAM_BYTES_MAX: usize = OUTPUT_BYTES_MAX / 2;
const HOME_MARK: &str = "~";

const _: () = assert!(SHOWN_BYTES_MAX <= OUTPUT_BYTES_MAX);
const _: () = assert!(STREAM_BYTES_MAX * 2 <= OUTPUT_BYTES_MAX);

/// How one tool run ended, as the owner may see it: whether the tool reported success and the
/// masked last lines of what it printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub success: bool,
    pub output: String,
}

/// How one tool run ended, for the engine's own parsing: the exit and the unmasked last
/// `STREAM_BYTES_MAX` bytes of each stream. Never shown and never sent to a client.
#[cfg_attr(not(unix), allow(dead_code))]
pub(super) struct Raw {
    pub(super) success: bool,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
}

/// Refuses to start any updater or installer as the administrator account.
/// Input: the effective user id. Output: the refusal code for root, None for anyone else.
pub fn refused_user(effective_user: u32) -> Option<&'static str> {
    (effective_user == 0).then_some(AS_ROOT)
}

/// Runs one fixed command with no shell, no input, a cleared environment and a search path of
/// only its own folder and the system folders, and ends it with everything it started when it
/// passes its time limit. Inputs: the command, the home folder and the limit.
/// Output: the exit and the masked last lines, or a refusal code.
pub fn run(fixed: &Fixed, home: &Path, limit: Duration) -> Result<Ran, &'static str> {
    let raw = run_raw(fixed, home, limit)?;
    let mut all = raw.stdout;
    if !all.is_empty() && !all.ends_with(b"\n") {
        all.push(b'\n');
    }
    all.extend_from_slice(&raw.stderr);
    assert!(all.len() <= OUTPUT_BYTES_MAX + 1);
    Ok(Ran {
        success: raw.success,
        output: masked(&all, home),
    })
}

/// Runs one fixed command exactly as [`run`] does and keeps each stream apart and unmasked.
/// Inputs: the command, the home folder and the limit. Output: the exit and both stream
/// tails, or a refusal code.
#[cfg(unix)]
pub(super) fn run_raw(fixed: &Fixed, home: &Path, limit: Duration) -> Result<Raw, &'static str> {
    unix::run_raw(fixed, home, limit)
}

/// Refuses every run where the engine cannot end a tool with everything it started.
/// Inputs: unused. Output: the refusal code.
#[cfg(not(unix))]
pub(super) fn run_raw(_fixed: &Fixed, _home: &Path, _limit: Duration) -> Result<Raw, &'static str> {
    Err(super::UNSUPPORTED_HERE)
}

#[cfg(unix)]
mod unix {
    use super::{AS_ROOT, Raw, START_FAILED, STREAM_BYTES_MAX, TIMEOUT, refused_user};
    use crate::harnesses::update::{ARGS_MAX, Fixed, TOOL_UNSAFE};
    use rustix::process::{Pid, Signal};
    use std::ffi::OsString;
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::sync::{Arc, Mutex, PoisonError, mpsc};
    use std::time::{Duration, Instant};

    const POLL: Duration = Duration::from_millis(50);
    const STOP_GRACE: Duration = Duration::from_secs(2);
    const DRAIN_TIME: Duration = Duration::from_secs(2);
    const CHUNK_BYTES: usize = 8192;
    const SEARCH_MAX: usize = 4;
    const SYSTEM_PATH: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
    const LANGUAGE: &str = "C.UTF-8";

    const _: () = assert!(CHUNK_BYTES <= STREAM_BYTES_MAX);

    /// The last bytes of one output pipe, read on its own thread so a chatty tool never
    /// blocks on a full pipe. `done` answers when the pipe reached its end.
    struct Tail {
        kept: Arc<Mutex<Vec<u8>>>,
        done: mpsc::Receiver<()>,
    }

    impl Tail {
        /// Starts reading a pipe to its end, keeping its last `STREAM_BYTES_MAX` bytes.
        /// Input: the pipe, present for a piped stream. Output: the handle to what is kept.
        fn start(pipe: Option<impl Read + Send + 'static>) -> Self {
            let kept = Arc::new(Mutex::new(Vec::new()));
            let (sender, done) = mpsc::channel();
            if let Some(pipe) = pipe {
                let into = kept.clone();
                let reader = std::thread::Builder::new().name("harness-update-output".into());
                let _ = reader.spawn(move || {
                    follow(pipe, &into);
                    let _ = sender.send(());
                });
            }
            Self { kept, done }
        }

        /// Waits a short time for the pipe's end, then takes what was kept so far, so a
        /// descendant that still holds the pipe cannot hold the run. Output: the last bytes.
        fn taken(self) -> Vec<u8> {
            let _ = self.done.recv_timeout(DRAIN_TIME);
            let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
            let mut bytes = std::mem::take(&mut *kept);
            if bytes.len() > STREAM_BYTES_MAX {
                bytes.drain(..bytes.len() - STREAM_BYTES_MAX);
            }
            assert!(bytes.len() <= STREAM_BYTES_MAX);
            bytes
        }
    }

    /// Reads a pipe to its end into a shared buffer that never holds more than twice the
    /// kept size. Inputs: the pipe and the buffer. Output: none.
    fn follow(mut pipe: impl Read, into: &Mutex<Vec<u8>>) {
        let mut chunk = [0_u8; CHUNK_BYTES];
        loop {
            let read = match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let mut kept = into.lock().unwrap_or_else(PoisonError::into_inner);
            kept.extend_from_slice(&chunk[..read]);
            if kept.len() > STREAM_BYTES_MAX * 2 {
                let extra = kept.len() - STREAM_BYTES_MAX;
                kept.drain(..extra);
            }
            assert!(kept.len() <= STREAM_BYTES_MAX * 2);
        }
    }

    /// Waits for a child to end, polling, for at most a time limit.
    /// Inputs: the child and the limit. Output: its exit, None when it still runs or cannot
    /// be asked.
    fn waited(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + limit;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return None;
                    }
                    std::thread::sleep(POLL.min(left));
                }
                Err(_) => return None,
            }
        }
    }

    /// Waits for the tool to its limit; past it, asks its whole process group to stop, waits
    /// a short grace, then kills the group and reaps the tool. A group that is already gone
    /// is fine. Inputs: the child, its group and the limit. Output: whether it reported
    /// success, None when it was ended for time.
    fn ended(child: &mut Child, group: Pid, limit: Duration) -> Option<bool> {
        if let Some(status) = waited(child, limit) {
            return Some(status.success());
        }
        let _ = rustix::process::kill_process_group(group, Signal::TERM);
        let stopped = waited(child, STOP_GRACE).is_some();
        let _ = rustix::process::kill_process_group(group, Signal::KILL);
        if !stopped {
            let _ = child.kill();
            let _ = child.wait();
        }
        None
    }

    /// The child's whole search path: the command's own folders, then the system folders.
    /// Input: the command. Output: the value, None when a folder cannot be on a search path.
    fn search_path(fixed: &Fixed) -> Option<OsString> {
        let own = fixed.search.iter().map(|folder| folder.as_path());
        let system = SYSTEM_PATH.iter().map(Path::new);
        std::env::join_paths(own.chain(system)).ok()
    }

    fn user_name() -> Option<String> {
        nix::unistd::User::from_uid(nix::unistd::geteuid())
            .ok()
            .flatten()
            .map(|user| user.name)
    }

    /// Builds the child: no shell, the fixed arguments only, an environment of exactly HOME,
    /// USER, LOGNAME, LANG, TMPDIR and PATH, no input, its own process group.
    /// Inputs: the command, the home folder and the search path. Output: the command to start.
    fn command(fixed: &Fixed, home: &Path, path: OsString) -> Command {
        let mut command = Command::new(&fixed.program);
        command
            .args(&fixed.args)
            .env_clear()
            .env("HOME", home)
            .env("LANG", LANGUAGE)
            .env("TMPDIR", std::env::temp_dir())
            .env("PATH", path)
            .current_dir(home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        if let Some(name) = user_name() {
            command.env("USER", &name).env("LOGNAME", &name);
        }
        command
    }

    pub(super) fn run_raw(
        fixed: &Fixed,
        home: &Path,
        limit: Duration,
    ) -> Result<Raw, &'static str> {
        if let Some(code) = refused_user(rustix::process::geteuid().as_raw()) {
            assert!(code == AS_ROOT);
            return Err(code);
        }
        assert!(fixed.program.is_absolute());
        assert!(fixed.args.len() <= ARGS_MAX);
        assert!(fixed.search.len() <= SEARCH_MAX);
        assert!(fixed.search.iter().all(|folder| folder.is_absolute()));
        let path = search_path(fixed).ok_or(TOOL_UNSAFE)?;
        let mut child =
            crate::spawn_gate::spawn(&mut command(fixed, home, path)).map_err(|_| START_FAILED)?;
        let group = Pid::from_child(&child);
        assert!(group.as_raw_nonzero().get() > 1);
        let stdout = Tail::start(child.stdout.take());
        let stderr = Tail::start(child.stderr.take());
        let success = ended(&mut child, group, limit);
        let (stdout, stderr) = (stdout.taken(), stderr.taken());
        assert!(stdout.len() <= STREAM_BYTES_MAX && stderr.len() <= STREAM_BYTES_MAX);
        let success = success.ok_or(TIMEOUT)?;
        Ok(Raw {
            success,
            stdout,
            stderr,
        })
    }
}

/// Where the reader is inside a terminal control sequence.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Escape {
    Outside,
    Started,
    Control,
    Command,
    CommandEnd,
}

/// Drops terminal control sequences and control characters from tool output. A carriage
/// return becomes a line end, a tab a space, and an unfinished sequence ends at the line end.
/// Input: the text. Output: printable text and newlines only.
fn printable(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut state = Escape::Outside;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            state = Escape::Outside;
            plain.push('\n');
            continue;
        }
        state = match (state, c) {
            (Escape::Outside, '\u{1b}') => Escape::Started,
            (Escape::Outside, '\r') => {
                if chars.peek() != Some(&'\n') {
                    plain.push('\n');
                }
                Escape::Outside
            }
            (Escape::Outside, '\t') => {
                plain.push(' ');
                Escape::Outside
            }
            (Escape::Outside, c) => {
                if !c.is_control() {
                    plain.push(c);
                }
                Escape::Outside
            }
            (Escape::Started, '[') => Escape::Control,
            (Escape::Started, ']') => Escape::Command,
            (Escape::Started, _) => Escape::Outside,
            (Escape::Control, '\u{40}'..='\u{7e}') => Escape::Outside,
            (Escape::Control, _) => Escape::Control,
            (Escape::Command, '\u{7}') => Escape::Outside,
            (Escape::Command, '\u{1b}') => Escape::CommandEnd,
            (Escape::Command, _) => Escape::Command,
            (Escape::CommandEnd, _) => Escape::Outside,
        };
    }
    assert!(plain.len() <= text.len());
    assert!(plain.chars().all(|c| c == '\n' || !c.is_control()));
    plain
}

/// Replaces the home folder's path with `~` wherever it stands as a whole path part.
/// Inputs: the text and the home folder. Output: the text without that path; unchanged for a
/// home that is not an absolute folder below the root.
fn without_home(text: &str, home: &Path) -> String {
    let home = home
        .to_str()
        .map(|home| home.trim_end_matches('/'))
        .filter(|home| home.len() > 1 && home.starts_with('/'));
    let Some(home) = home else {
        return text.to_owned();
    };
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(home) {
        let after = &rest[at + home.len()..];
        let whole = after
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '-'));
        kept.push_str(&rest[..at]);
        kept.push_str(if whole { HOME_MARK } else { home });
        rest = after;
    }
    kept.push_str(rest);
    assert!(kept.len() <= text.len());
    kept
}

/// Keeps the end of a text within a byte bound, starting at a line start when one is inside
/// the bound. Inputs: the text and the bound. Output: the kept end.
fn last_lines(text: &str, bytes_max: usize) -> &str {
    if text.len() <= bytes_max {
        return text;
    }
    let mut start = text.len() - bytes_max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let end = &text[start..];
    let kept = match end.find('\n') {
        Some(line_end) if line_end + 1 < end.len() => &end[line_end + 1..],
        _ => end,
    };
    assert!(kept.len() <= bytes_max);
    kept
}

/// Makes tool output safe to show: text only, no terminal control sequences, credential
/// shapes hidden, the home folder written as `~`, and only the last `SHOWN_BYTES_MAX` bytes.
/// Inputs: the output bytes and the home folder. Output: the text to show.
pub fn masked(output: &[u8], home: &Path) -> String {
    let start = output.len().saturating_sub(OUTPUT_BYTES_MAX);
    let plain = printable(&String::from_utf8_lossy(&output[start..]));
    let hidden = crate::project_setup::redact(&plain);
    let text = without_home(&hidden, home);
    let shown = last_lines(&text, SHOWN_BYTES_MAX);
    assert!(shown.len() <= SHOWN_BYTES_MAX);
    assert!(!shown.contains('\u{1b}'));
    shown.to_owned()
}

/// Reads one vendor release pointer from the compiled table over HTTPS, with no credentials,
/// inside the pointer's size bound and `POINTER_TIME`. Input: the pointer.
/// Output: the reply body, kept in memory only, or `LOOKUP_FAILED` for any failure.
pub fn lookup(pointer: &Pointer) -> Result<Vec<u8>, &'static str> {
    read_pointer(pointer.url, pointer.bytes_max, POINTER_TIME)
}

/// Reads one address through the engine's bounded HTTPS reader. Every failure is the same
/// code, so nothing a server sent is ever shown. Inputs: the address, the byte bound and the
/// time limit. Output: the body within the bound, or `LOOKUP_FAILED`.
pub(super) fn read_pointer(
    url: &str,
    bytes_max: u64,
    limit: Duration,
) -> Result<Vec<u8>, &'static str> {
    assert!(bytes_max <= POINTER_BYTES_MAX);
    let mut body = Vec::new();
    crate::update::download(url, &mut body, bytes_max, limit).map_err(|_| LOOKUP_FAILED)?;
    assert!(body.len() as u64 <= bytes_max);
    Ok(body)
}
