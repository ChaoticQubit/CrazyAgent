use super::{Fixed, OUTPUT_BYTES_MAX, POINTER_BYTES_MAX, Pointer};
#[cfg(windows)]
use std::io::Read;
use std::path::Path;
#[cfg(windows)]
use std::process::{Child, ExitStatus};
#[cfg(windows)]
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

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

/// Refuses to start any updater or installer from an elevated Windows token, the twin of
/// [`refused_user`]. Input: whether the token is elevated. Output: the refusal code for an
/// elevated token, None for any other.
#[cfg(windows)]
pub(super) fn refused_token(elevated: bool) -> Option<&'static str> {
    elevated.then_some(AS_ROOT)
}

/// Runs one fixed command with no shell, no input, a cleared environment and a search path of
/// only its own folder and the system folders, as one kernel install step: everything it
/// started is ended when it returns, and past its time limit after TERM and a grace.
/// Inputs: the command, the home folder and the limit. Output: the exit and the masked last
/// lines, or a refusal code.
#[cfg(not(windows))]
pub fn run(fixed: &Fixed, home: &Path, limit: Duration) -> Result<Ran, &'static str> {
    run_raw(fixed, home, limit).map(|raw| masked_ran(raw, home))
}

/// Runs one checked command the way this system runs tools: [`run_raw`] on Unix, and on
/// Windows the words the resolver gives its program, started in a kill-on-close job. Inputs:
/// the command, the resolver's places, the home folder and the limit. Output: the exit and
/// both stream tails, or a refusal code.
#[cfg(not(windows))]
pub(super) fn started(
    fixed: &Fixed,
    places: &kernel_processes::finding::Places,
    home: &Path,
    limit: Duration,
) -> Result<Raw, &'static str> {
    let _ = places;
    run_raw(fixed, home, limit)
}

/// Runs one checked command the way this system runs tools: [`run_raw`] on Unix, and on
/// Windows the words the resolver gives its program, started in a kill-on-close job. Inputs:
/// the command, the resolver's places, the home folder and the limit. Output: the exit and
/// both stream tails, or a refusal code.
#[cfg(windows)]
pub(super) fn started(
    fixed: &Fixed,
    places: &kernel_processes::finding::Places,
    home: &Path,
    limit: Duration,
) -> Result<Raw, &'static str> {
    run_raw(fixed, places, home, limit)
}

/// What the owner may see of one run: stdout then stderr, joined on a line end and masked
/// ([`masked`]). Inputs: the run's two tails and the home folder. Output: the exit and the
/// masked text.
pub(super) fn masked_ran(raw: Raw, home: &Path) -> Ran {
    let mut all = raw.stdout;
    if !all.is_empty() && !all.ends_with(b"\n") {
        all.push(b'\n');
    }
    all.extend_from_slice(&raw.stderr);
    assert!(all.len() <= OUTPUT_BYTES_MAX + 1);
    Ran {
        success: raw.success,
        output: masked(&all, home),
    }
}

/// Runs one fixed command exactly as [`run`] does and keeps each stream apart and unmasked.
/// Inputs: the command, the home folder and the limit. Output: the exit and both stream
/// tails, or a refusal code.
#[cfg(unix)]
pub(super) fn run_raw(fixed: &Fixed, home: &Path, limit: Duration) -> Result<Raw, &'static str> {
    unix::run_raw(fixed, home, limit, rustix::process::geteuid().as_raw())
}

/// Runs one fixed command exactly as [`run_raw`] does for the given effective user, so a row
/// can ask for the administrator account without being it. Inputs: the command, the home
/// folder, the limit and the effective user id. Output: the exit and both stream tails, or a
/// refusal code.
#[cfg(all(test, unix))]
pub(super) fn run_raw_as(
    fixed: &Fixed,
    home: &Path,
    limit: Duration,
    user: u32,
) -> Result<Raw, &'static str> {
    unix::run_raw(fixed, home, limit, user)
}

/// Refuses every run where the engine cannot end a tool with everything it started.
/// Inputs: unused. Output: the refusal code.
#[cfg(not(any(unix, windows)))]
pub(super) fn run_raw(_fixed: &Fixed, _home: &Path, _limit: Duration) -> Result<Raw, &'static str> {
    Err(super::UNSUPPORTED_HERE)
}

/// Runs one checked command on Windows, refused while the engine's token is elevated or
/// unreadable ([`refused_token`]): the words the resolver gives its program, each file it
/// starts judged again ([`crazyagent_win_sandbox::program_trusted`]), then the fixed arguments,
/// started suspended with no window in a kill-on-close job, with npm's prefix pinned for an
/// npm command. Inputs: the command, the places, the home and the limit. Output: the run.
#[cfg(windows)]
pub(super) fn run_raw(
    fixed: &Fixed,
    places: &kernel_processes::finding::Places,
    home: &Path,
    limit: Duration,
) -> Result<Raw, &'static str> {
    let elevated = crazyagent_win_sandbox::elevated().unwrap_or(true);
    windows::run_raw(fixed, places, home, limit, elevated)
}

/// Runs one checked command exactly as the Windows [`run_raw`] does for the given token
/// elevation, so a row can run unelevated on an elevated runner. Inputs: the command, the
/// places, the home folder, the limit and the elevation. Output: the exit and both stream
/// tails, or a refusal code.
#[cfg(all(test, windows))]
pub(super) fn run_raw_as(
    fixed: &Fixed,
    places: &kernel_processes::finding::Places,
    home: &Path,
    limit: Duration,
    elevated: bool,
) -> Result<Raw, &'static str> {
    windows::run_raw(fixed, places, home, limit, elevated)
}

/// Runs a harness's version command exactly as [`run_raw`] does. Inputs: the command, the
/// home folder and the limit. Output: the exit and both stream tails, or a refusal code.
#[cfg(unix)]
pub(super) fn version_raw(
    fixed: &Fixed,
    home: &Path,
    limit: Duration,
) -> Result<Raw, &'static str> {
    run_raw(fixed, home, limit)
}

/// Runs a harness's version words on Windows as [`run_raw`] runs a tool there: started
/// suspended with no window in a kill-on-close job of its own, so everything it starts ends
/// with the job once it exits or passes its limit, with nothing added to its environment.
/// Inputs: the absolute words, the command's search folders, the home folder and the limit.
/// Output: the exit and both stream tails, or a refusal code.
#[cfg(windows)]
pub(super) fn version_raw(
    words: &[std::ffi::OsString],
    search: &[std::path::PathBuf],
    home: &Path,
    limit: Duration,
) -> Result<Raw, &'static str> {
    windows::version_raw(words, search, home, limit)
}

#[cfg(windows)]
const POLL: Duration = Duration::from_millis(50);
#[cfg(windows)]
const DRAIN_TIME: Duration = Duration::from_secs(2);
#[cfg(windows)]
const CHUNK_BYTES: usize = 8192;
const SEARCH_MAX: usize = 4;

#[cfg(windows)]
const _: () = assert!(CHUNK_BYTES <= STREAM_BYTES_MAX);

/// The last bytes of one output pipe, read on its own thread so a chatty tool never
/// blocks on a full pipe. `done` answers when the pipe reached its end.
#[cfg(windows)]
struct Tail {
    kept: Arc<Mutex<Vec<u8>>>,
    done: mpsc::Receiver<()>,
}

#[cfg(windows)]
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
#[cfg(windows)]
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
#[cfg(windows)]
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

#[cfg(unix)]
mod unix {
    use super::{AS_ROOT, Raw, SEARCH_MAX, START_FAILED, STREAM_BYTES_MAX, TIMEOUT, refused_user};
    use crate::harnesses::update::{ARGS_MAX, Fixed, TOOL_UNSAFE};
    use kernel_processes::install::Step;
    use std::ffi::OsString;
    use std::path::Path;
    use std::time::Duration;

    /// The environment name npm reads its global prefix from, ahead of every npmrc file.
    const NPM_PREFIX: &str = "npm_config_prefix";
    const SYSTEM_PATH: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
    const LANGUAGE: &str = "C.UTF-8";

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

    /// The child's whole environment: exactly HOME, LANG, TMPDIR and PATH, then USER and
    /// LOGNAME when this user has a name. Inputs: the home folder and the search path.
    /// Output: the pairs.
    fn base(home: &Path, path: OsString) -> Vec<(&'static str, OsString)> {
        let mut base = vec![
            ("HOME", home.as_os_str().to_owned()),
            ("LANG", OsString::from(LANGUAGE)),
            ("TMPDIR", std::env::temp_dir().into_os_string()),
            ("PATH", path),
        ];
        if let Some(name) = user_name() {
            base.push(("USER", OsString::from(&name)));
            base.push(("LOGNAME", OsString::from(name)));
        }
        assert!(base.len() <= 6);
        base
    }

    /// The harness code for a kernel install run's code: a run past its time, a program that
    /// could not start, and any other refusal as unsafe. Input: the kernel code. Output: the
    /// harness code.
    fn harness_code(code: &'static str) -> &'static str {
        match code {
            "setup_timeout" => TIMEOUT,
            "setup_install_failed" => START_FAILED,
            _ => TOOL_UNSAFE,
        }
    }

    /// The last [`STREAM_BYTES_MAX`] bytes of a stream tail. Input: the tail. Output: its end.
    fn last(mut bytes: Vec<u8>) -> Vec<u8> {
        if bytes.len() > STREAM_BYTES_MAX {
            bytes.drain(..bytes.len() - STREAM_BYTES_MAX);
        }
        assert!(bytes.len() <= STREAM_BYTES_MAX);
        bytes
    }

    /// Refuses the administrator account, then runs the command as one kernel install step
    /// ([`kernel_processes::install::run`]): no shell, the fixed arguments only, the
    /// [`base`] environment plus npm's global prefix for an npm command, the home folder as
    /// its working folder, no input and its own process group, which gets TERM, a grace and
    /// KILL past the limit and KILL on every return. Inputs: the command, the home folder,
    /// the limit and the effective user id. Output: the exit and both stream tails, or a
    /// refusal code.
    pub(super) fn run_raw(
        fixed: &Fixed,
        home: &Path,
        limit: Duration,
        user: u32,
    ) -> Result<Raw, &'static str> {
        if let Some(code) = refused_user(user) {
            assert!(code == AS_ROOT);
            return Err(code);
        }
        assert!(fixed.program.is_absolute());
        assert!(fixed.args.len() <= ARGS_MAX);
        assert!(fixed.search.len() <= SEARCH_MAX);
        assert!(fixed.search.iter().all(|folder| folder.is_absolute()));
        assert!(fixed.prefix.as_deref().is_none_or(Path::is_absolute));
        if !home.is_absolute() {
            return Err(TOOL_UNSAFE);
        }
        let path = search_path(fixed).ok_or(TOOL_UNSAFE)?;
        let step = Step {
            program: fixed.program.clone(),
            args: fixed.args.iter().map(OsString::from).collect(),
            env: fixed
                .prefix
                .iter()
                .map(|prefix| (NPM_PREFIX, prefix.as_os_str().to_owned()))
                .collect(),
        };
        let ran = kernel_processes::install::run(&step, &base(home, path), home, limit)
            .map_err(harness_code)?;
        Ok(Raw {
            success: ran.code == 0,
            stdout: last(ran.output),
            stderr: last(ran.stderr),
        })
    }
}

#[cfg(windows)]
mod windows {
    use super::{
        AS_ROOT, Raw, SEARCH_MAX, START_FAILED, STREAM_BYTES_MAX, TIMEOUT, Tail, refused_token,
        waited,
    };
    use crate::harnesses::update::{ARGS_MAX, Fixed, TOOL_UNSAFE};
    use kernel_processes::finding::INTERPRETER_DEPTH_MAX;
    use std::ffi::OsString;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    const NPM_PREFIX: &str = "npm_config_prefix";
    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SYSTEM_ROOT_DEFAULT: &str = r"C:\Windows";

    /// The most words a version run starts: an interpreter chain at its depth bound, each
    /// with its first line's arguments, the script, then the version arguments.
    const WORDS_MAX: usize = (INTERPRETER_DEPTH_MAX + 1) * (ARGS_MAX + 1) + ARGS_MAX;

    /// The child's whole search path: the command's own folders, then the Windows system
    /// folders. Input: the folders. Output: the value, None when a folder cannot be on it.
    fn search_path(search: &[PathBuf]) -> Option<OsString> {
        let root = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(SYSTEM_ROOT_DEFAULT), PathBuf::from);
        let system = [root.join("System32"), root];
        std::env::join_paths(search.iter().cloned().chain(system)).ok()
    }

    /// Builds the child: no shell, the fixed arguments only, the environment a plugin
    /// server check keeps ([`crate::plugins::check::ENV_KEPT`]) with the home as
    /// USERPROFILE, the search path as PATH and the extra pairs, no input, no window, started
    /// suspended. Inputs: the words, the home, the search path and the extra pairs. Output:
    /// the command.
    fn command(
        words: &[OsString],
        home: &Path,
        path: OsString,
        extra: &[(&str, OsString)],
    ) -> Command {
        let mut command = Command::new(&words[0]);
        command.args(&words[1..]).env_clear();
        for name in crate::plugins::check::ENV_KEPT {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .env("USERPROFILE", home)
            .env("PATH", path)
            .envs(extra.iter().map(|(name, value)| (name, value)))
            .current_dir(home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
        command
    }

    /// Runs one checked command unless the token is elevated: the words the resolver gives its
    /// program, each file it starts judged again ([`crazyagent_win_sandbox::program_trusted`]),
    /// then the fixed arguments, with npm's prefix pinned for an npm command ([`ran`]).
    /// Inputs: the command, the places, the home, the limit and the elevation. Output: the
    /// exit and both stream tails, or a refusal code.
    pub(super) fn run_raw(
        fixed: &Fixed,
        places: &kernel_processes::finding::Places,
        home: &Path,
        limit: Duration,
        elevated: bool,
    ) -> Result<Raw, &'static str> {
        if let Some(code) = refused_token(elevated) {
            assert!(code == AS_ROOT);
            return Err(code);
        }
        assert!(fixed.program.is_absolute());
        assert!(fixed.args.len() <= ARGS_MAX);
        assert!(fixed.search.len() <= SEARCH_MAX);
        assert!(fixed.search.iter().all(|folder| folder.is_absolute()));
        assert!(fixed.prefix.as_deref().is_none_or(Path::is_absolute));
        if !home.is_absolute() {
            return Err(TOOL_UNSAFE);
        }
        let tool = kernel_processes::finding::resolve_entry(&fixed.program, places)
            .map_err(|_| TOOL_UNSAFE)?;
        for file in tool.grants() {
            crazyagent_win_sandbox::program_trusted(&file).map_err(|_| TOOL_UNSAFE)?;
        }
        let mut words = tool.argv();
        words.extend(fixed.args.iter().map(OsString::from));
        if words.len() > WORDS_MAX {
            return Err(TOOL_UNSAFE);
        }
        let extra: Vec<(&str, OsString)> = fixed
            .prefix
            .iter()
            .map(|prefix| (NPM_PREFIX, prefix.as_os_str().to_owned()))
            .collect();
        ran(&words, &fixed.search, home, limit, &extra)
    }

    /// Runs a harness's version words with nothing added to the environment ([`ran`]).
    /// Inputs: the absolute words, the command's search folders, the home and the limit.
    /// Output: the exit and both stream tails, or a refusal code.
    pub(super) fn version_raw(
        words: &[OsString],
        search: &[PathBuf],
        home: &Path,
        limit: Duration,
    ) -> Result<Raw, &'static str> {
        ran(words, search, home, limit, &[])
    }

    /// Starts the words suspended with no window in a kill-on-close job of their own, so
    /// everything they start ends with the job once they exit or pass the limit. Inputs: the
    /// absolute words, the search folders, the home, the limit and the extra environment
    /// pairs. Output: the exit and both stream tails, or a refusal code.
    fn ran(
        words: &[OsString],
        search: &[PathBuf],
        home: &Path,
        limit: Duration,
        extra: &[(&str, OsString)],
    ) -> Result<Raw, &'static str> {
        assert!((1..=WORDS_MAX).contains(&words.len()));
        assert!(Path::new(&words[0]).is_absolute());
        assert!(search.len() <= SEARCH_MAX);
        assert!(search.iter().all(|folder| folder.is_absolute()));
        assert!(extra.len() <= 1);
        let path = search_path(search).ok_or(TOOL_UNSAFE)?;
        let mut child = kernel_processes::spawn_gate::spawn(&mut command(words, home, path, extra))
            .map_err(|_| START_FAILED)?;
        let tree = match crazyagent_win_sandbox::ProcessTree::hold(&child) {
            Ok(tree) => tree,
            Err(error) => {
                eprintln!("harness_version_tree_refused cause={error}");
                let _ = child.kill();
                let _ = child.wait();
                return Err(START_FAILED);
            }
        };
        let stdout = Tail::start(child.stdout.take());
        let stderr = Tail::start(child.stderr.take());
        let status = waited(&mut child, limit);
        drop(tree);
        let _ = child.kill();
        let _ = child.wait();
        let (stdout, stderr) = (stdout.taken(), stderr.taken());
        assert!(stdout.len() <= STREAM_BYTES_MAX && stderr.len() <= STREAM_BYTES_MAX);
        let success = status.ok_or(TIMEOUT)?.success();
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

/// Reads one vendor release pointer of a bundled harness manifest over HTTPS, with no
/// credentials, inside the pointer's size bound and `POINTER_TIME`. Input: the pointer.
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
    kernel_processes::update::download(url, &mut body, bytes_max, limit)
        .map_err(|_| LOOKUP_FAILED)?;
    assert!(body.len() as u64 <= bytes_max);
    Ok(body)
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    /// Whether a process id still runs, as `tasklist` lists it. Input: the id. Output: the
    /// answer.
    fn running(pid: u32) -> bool {
        let mut tasklist = std::process::Command::new("tasklist");
        tasklist.args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]);
        let listed = kernel_processes::spawn_gate::collected(&mut tasklist).unwrap();
        String::from_utf8_lossy(&listed.stdout).contains(&format!("\"{pid}\""))
    }

    /// Whether a process id stops running within ten seconds, asked every tenth of a second,
    /// as a job's processes end a moment after the job closes. Input: the id. Output: true
    /// once it no longer runs.
    fn ended(pid: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while running(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        !running(pid)
    }

    /// V243-W1-VERSION-READ: a version run that passes its limit ends as timed out, and the
    /// process it started ends with its job, so no child of a harness outlives a version read.
    /// The version command is the test binary as a grandchild holder
    /// ([`crate::plugins::check::windows_tests::grandchild_holder`]), asked through a request
    /// file in the run's home, its working folder, since the run keeps no other environment.
    #[test]
    fn a_version_run_past_its_limit_ends_with_every_process_it_started() {
        use crate::plugins::check::windows_tests::{HOLDER_REQUEST, grandchild_holder};
        let folder = tempfile::tempdir().unwrap();
        let home = kernel_sandbox::paths::canonical(folder.path()).unwrap();
        std::fs::write(home.join(HOLDER_REQUEST), "").unwrap();
        let (holder, args) = grandchild_holder();
        let search = [holder.parent().unwrap().to_path_buf()];
        let words: Vec<OsString> = std::iter::once(holder.into_os_string())
            .chain(args.map(OsString::from))
            .collect();
        let ran = version_raw(&words, &search, &home, Duration::from_secs(20));
        assert_eq!(ran.err(), Some(TIMEOUT));
        let pid: u32 = std::fs::read_to_string(home.join("grandchild.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(pid > 4);
        assert!(ended(pid), "the version run's child {pid} outlived it");
    }

    /// A copy of the grandchild holder in a fresh folder under the profile, which only this
    /// user, SYSTEM and the administrators may change, so the program trust check passes it
    /// whatever the build folder allows. Output: the folder guard, the copy and its arguments.
    fn own_holder() -> (tempfile::TempDir, PathBuf, [&'static str; 5]) {
        use crate::plugins::check::windows_tests::grandchild_holder;
        let (holder, args) = grandchild_holder();
        let folder = tempfile::tempdir_in(std::env::var_os("USERPROFILE").unwrap()).unwrap();
        let own = kernel_sandbox::paths::canonical(folder.path()).unwrap();
        let copy = own.join(holder.file_name().unwrap());
        std::fs::copy(&holder, &copy).unwrap();
        (folder, copy, args)
    }

    /// An update run that passes its limit ends as timed out, and the process it started ends
    /// with its job, so no child of an installer outlives its run. The command is the
    /// grandchild holder, asked through a request file in the run's home.
    #[test]
    fn an_update_run_past_its_limit_ends_with_every_process_it_started() {
        use crate::plugins::check::windows_tests::HOLDER_REQUEST;
        let (_own, holder, args) = own_holder();
        let folder = tempfile::tempdir().unwrap();
        let home = kernel_sandbox::paths::canonical(folder.path()).unwrap();
        std::fs::write(home.join(HOLDER_REQUEST), "").unwrap();
        let search = holder.parent().unwrap().to_path_buf();
        let fixed = Fixed {
            program: holder,
            args: args.to_vec(),
            search: vec![search.clone()],
            prefix: None,
        };
        let places = crate::tools::tools_new(vec![search], None);
        let ran = run_raw_as(&fixed, &places, &home, Duration::from_secs(20), false);
        assert_eq!(ran.err(), Some(TIMEOUT));
        let pid: u32 = std::fs::read_to_string(home.join("grandchild.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(pid > 4);
        assert!(ended(pid), "the update run's child {pid} outlived it");
    }

    /// An npm command's run gets npm's global prefix pinned in its environment, here read
    /// back by the command shell Windows ships; the same command from an elevated token is
    /// refused before it starts, and the real token decides the plain run.
    #[test]
    fn an_npm_run_gets_its_prefix_and_an_elevated_engine_starts_nothing() {
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        let folder = tempfile::tempdir().unwrap();
        let home = kernel_sandbox::paths::canonical(folder.path()).unwrap();
        let prefix = home.join("prefix");
        let fixed = Fixed {
            program: system.join("cmd.exe"),
            args: vec!["/d", "/c", "set", "npm_config_prefix"],
            search: vec![system.clone()],
            prefix: Some(prefix.clone()),
        };
        let places = crate::tools::tools_new(vec![system], None);
        let raw = run_raw_as(&fixed, &places, &home, CHECK_TIME, false).unwrap();
        assert!(raw.success);
        let printed = String::from_utf8_lossy(&raw.stdout).into_owned();
        let pinned = format!("npm_config_prefix={}", prefix.display());
        assert!(printed.lines().any(|line| line == pinned), "{printed}");
        assert_eq!(
            run_raw_as(&fixed, &places, &home, CHECK_TIME, true).err(),
            Some(AS_ROOT)
        );
        if crazyagent_win_sandbox::elevated().unwrap() {
            assert_eq!(
                run_raw(&fixed, &places, &home, CHECK_TIME).err(),
                Some(AS_ROOT)
            );
        } else {
            assert!(run_raw(&fixed, &places, &home, CHECK_TIME).unwrap().success);
        }
    }
}
