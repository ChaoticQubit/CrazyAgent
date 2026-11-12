#![cfg_attr(not(unix), allow(dead_code))]

use std::collections::BTreeMap;
use std::ffi::OsStr;
#[cfg(unix)]
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

#[cfg(all(test, unix))]
mod tests;

/// How long the login shell may take to print its PATH.
pub(crate) const SHELL_LIMIT: Duration = Duration::from_secs(10);
/// How long npm may take to print its global prefix.
pub(crate) const NPM_LIMIT: Duration = Duration::from_secs(10);
/// How long a program may take to print its version.
pub(crate) const VERSION_LIMIT: Duration = Duration::from_secs(5);
/// The most folders the probes keep.
pub(crate) const FOLDERS_MAX: usize = 64;
/// The most bytes of one kept version.
pub(crate) const VERSION_BYTES_MAX: usize = 64;
/// The line the login shell prints before its PATH.
pub(crate) const BEGIN: &str = "CRAZYAGENT-PATH-BEGIN";
/// The line the login shell prints after its PATH.
pub(crate) const END: &str = "CRAZYAGENT-PATH-END";
/// The most bytes of output a probed program may give.
const OUTPUT_BYTES_MAX: usize = 64 * 1024;
/// The longest folder or program path the probes accept.
const PATH_BYTES_MAX: usize = 1024;
/// The most programs whose versions are cached; others stay unknown.
const VERSIONS_MAX: usize = 64;
/// How often a timed run checks whether its program exited.
const POLL: Duration = Duration::from_millis(10);
/// How long a timed run waits for the rest of the output once its program exited, before
/// it closes the read end that a process outside the group may still hold.
const OUTPUT_GRACE: Duration = Duration::from_millis(500);
/// The only search path a probed program inherits.
const SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
/// The first wait before a folder run that failed, or a resolve that missed, runs the
/// probes again; a version probe that gave none waits this long too.
pub(crate) const RETRY_AFTER: Duration = Duration::from_secs(30);
/// The longest a folder run waits after runs that failed or found nothing new.
pub(crate) const BACKOFF_MAX: Duration = Duration::from_secs(600);
/// How often the folder wait doubles before [`BACKOFF_MAX`] caps it.
const DOUBLINGS_MAX: u32 = 5;
/// The perl program that starts the login shell in a new session with no terminal, so an
/// interactive shell can never take over the terminal the engine was started from.
const NEW_SESSION: &str = "POSIX::setsid() > 0 or exit 1; exec {$ARGV[0]} @ARGV or exit 1";

/// The folder probes' cache: what the runs found, whether one is going and the last run.
static FOLDERS: Mutex<Probes> = Mutex::new(Probes::new());
/// Each probed program's version, with the file identity it was read from.
static VERSIONS: Mutex<BTreeMap<PathBuf, Known>> = Mutex::new(BTreeMap::new());

/// The folders the background probes found: npm's global prefix `bin` of the npm the
/// resolver finds and the login shell's PATH folders. Answers at once from what the probes
/// cached, empty until the first run finishes, and starts a background run when one is due
/// ([`Probes::start`]). Output: the absolute folders.
pub fn folders() -> Vec<PathBuf> {
    refreshed(false)
}

/// A program's version as it reports it, read in the background with a time limit and
/// cached for its file's identity, so an update in place is read again, as is a probe that
/// gave none once [`RETRY_AFTER`] passed. Input: the program's real file. Output: the
/// version, or none while unknown, which never counts as missing.
pub fn version(program: &Path) -> Option<String> {
    if !program.is_absolute() || program.as_os_str().len() > PATH_BYTES_MAX {
        return None;
    }
    let identity = identity(program)?;
    let now = Instant::now();
    let mut versions = lock(&VERSIONS);
    let known = versions.get(program);
    if !version_due(known, identity, now) {
        return known.and_then(|known| known.version.clone());
    }
    let room = known.is_some() || versions.len() < VERSIONS_MAX;
    if room && start_version_probe(program, identity) {
        let probing = Known {
            identity,
            version: None,
            finished: None,
        };
        versions.insert(program.to_path_buf(), probing);
    }
    assert!(versions.len() <= VERSIONS_MAX);
    None
}

/// Tells the probes a resolve found nothing, so the folders are probed again in the
/// background once the last run waited out its backoff ([`Probes::start`]); a run already
/// going is enough. Never blocks.
pub fn missed() {
    refreshed(true);
}

/// Starts a background folder run when one is due, recording a run that could not start as
/// failed so it is retried. Input: whether a resolve missed. Output: the cached folders.
fn refreshed(missed: bool) -> Vec<PathBuf> {
    let now = Instant::now();
    let mut probes = lock(&FOLDERS);
    if probes.start(now, missed) && !start_folder_probe() {
        probes.finish(Vec::new(), false, now);
    }
    probes.found.clone()
}

/// One finished folder probe run: when it ended and whether every probe in it answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) finished: Instant,
    pub(crate) complete: bool,
}

/// The folder probes' cache: the folders found, whether a run is going, the last run, and
/// how many runs in a row failed or found nothing new, which sets the backoff.
#[derive(Debug)]
pub(crate) struct Probes {
    pub(crate) found: Vec<PathBuf>,
    pub(crate) running: bool,
    pub(crate) last: Option<Run>,
    pub(crate) streak: u32,
}

impl Probes {
    /// An empty cache with no run yet. Output: the cache.
    pub(crate) const fn new() -> Self {
        Self {
            found: Vec::new(),
            running: false,
            last: None,
            streak: 0,
        }
    }

    /// Marks a run as going when one is due: at first use, or with no run going, when the
    /// last run failed or a resolve missed and the backoff passed: 30 s, then doubling for
    /// each run in a row that failed or found nothing new, at most [`BACKOFF_MAX`]. A
    /// complete run is never run again on its own. Inputs: now and whether a resolve
    /// missed. Output: whether the caller starts the run.
    pub(crate) fn start(&mut self, now: Instant, missed: bool) -> bool {
        let wait = backoff(self.streak);
        let due = !self.running
            && self.last.is_none_or(|last| {
                (missed || !last.complete) && now.saturating_duration_since(last.finished) >= wait
            });
        if due {
            self.running = true;
        }
        due
    }

    /// Records a finished run: a complete one replaces the folders, a failed one adds its
    /// folders before the ones found earlier, at most [`FOLDERS_MAX`]. The backoff starts
    /// over after a complete run that is the first or found something new, and grows after
    /// any other. Inputs: the folders found, whether every probe answered and now.
    pub(crate) fn finish(&mut self, found: Vec<PathBuf>, complete: bool, now: Instant) {
        assert!(self.running, "a run finishes only after it started");
        assert!(found.len() <= FOLDERS_MAX);
        let earlier = std::mem::take(&mut self.found);
        let progressed = complete && (self.last.is_none() || found != earlier);
        let mut folders = found;
        if !complete {
            for folder in earlier {
                if folders.len() == FOLDERS_MAX {
                    break;
                }
                if !folders.contains(&folder) {
                    folders.push(folder);
                }
            }
        }
        assert!(folders.len() <= FOLDERS_MAX);
        self.found = folders;
        self.running = false;
        self.last = Some(Run {
            finished: now,
            complete,
        });
        self.streak = if progressed {
            0
        } else {
            self.streak.saturating_add(1)
        };
    }
}

/// The wait before a run after a failure or a miss: [`RETRY_AFTER`] for a streak of none
/// or one, doubling with each further run in the streak, at most [`BACKOFF_MAX`].
/// Input: the streak. Output: the wait.
fn backoff(streak: u32) -> Duration {
    let doublings = streak.saturating_sub(1).min(DOUBLINGS_MAX);
    let wait = RETRY_AFTER.saturating_mul(1 << doublings).min(BACKOFF_MAX);
    assert!((RETRY_AFTER..=BACKOFF_MAX).contains(&wait));
    wait
}

/// A folder run going on its thread: dropped without [`Running::finish`], such as when the
/// run panics, it records the run as failed, so the next one can start after its backoff.
pub(crate) struct Running<'cache> {
    probes: &'cache Mutex<Probes>,
    finished: bool,
}

impl<'cache> Running<'cache> {
    /// Takes charge of the run [`Probes::start`] marked as going. Input: the cache.
    /// Output: the guard.
    pub(crate) fn new(probes: &'cache Mutex<Probes>) -> Self {
        assert!(
            lock(probes).running,
            "a run is marked as going before its thread takes it"
        );
        Self {
            probes,
            finished: false,
        }
    }

    /// Records the run's folders and whether every probe in it answered.
    /// Inputs: the folders and the answer.
    pub(crate) fn finish(mut self, found: Vec<PathBuf>, complete: bool) {
        lock(self.probes).finish(found, complete, Instant::now());
        self.finished = true;
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut probes = lock(self.probes);
        if probes.running {
            probes.finish(Vec::new(), false, Instant::now());
        }
    }
}

/// What tells one program file from its update in place: its device, inode, size and
/// modification time in seconds and nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) size: u64,
    pub(crate) modified: (i64, i64),
}

/// One program's cached version: the file it was read from, the version, and when its
/// probe ended, none while it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Known {
    pub(crate) identity: Identity,
    pub(crate) version: Option<String>,
    pub(crate) finished: Option<Instant>,
}

/// Whether a program's version is probed now: never probed, its file changed, or with no
/// probe going its last one gave none and is [`RETRY_AFTER`] old. Inputs: the cached
/// entry, the file's identity now and now. Output: the answer.
pub(crate) fn version_due(known: Option<&Known>, identity: Identity, now: Instant) -> bool {
    let Some(known) = known else {
        return true;
    };
    if known.identity != identity {
        return true;
    }
    match (&known.version, known.finished) {
        (None, Some(finished)) => now.saturating_duration_since(finished) >= RETRY_AFTER,
        _ => false,
    }
}

/// The identity of a program file, its links followed. Input: the file. Output: the
/// identity, or none for anything but a readable plain file.
#[cfg(unix)]
fn identity(file: &Path) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(file).ok()?;
    metadata.is_file().then(|| Identity {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.size(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
    })
}

/// No identity where versions are not probed. Input unused. Output: none.
#[cfg(not(unix))]
fn identity(_file: &Path) -> Option<Identity> {
    None
}

/// Locks a probe cache, keeping what it holds when a probe thread panicked while holding it.
/// Input: the cache. Output: its guard.
fn lock<T>(cache: &Mutex<T>) -> MutexGuard<'_, T> {
    cache.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts one folder run on a background thread that records what it found. Unit tests
/// never start one, so a developer's own shell files are never run, and where Unix shells
/// do not exist nothing is probed. Output: whether the run started.
fn start_folder_probe() -> bool {
    #[cfg(all(unix, not(test)))]
    {
        std::thread::Builder::new()
            .name("tools-probe".to_owned())
            .spawn(|| {
                let running = Running::new(&FOLDERS);
                let (found, complete) = probe_folders();
                running.finish(found, complete);
            })
            .is_ok()
    }
    #[cfg(not(all(unix, not(test))))]
    {
        false
    }
}

/// Starts one program's version probe on a background thread that fills its cache entry
/// while the entry still names the same file identity. Inputs: the program and its
/// identity. Output: whether the probe started.
fn start_version_probe(program: &Path, identity: Identity) -> bool {
    #[cfg(unix)]
    {
        let program = program.to_path_buf();
        std::thread::Builder::new()
            .name("tools-version".to_owned())
            .spawn(move || {
                let found = probe_version(&program, VERSION_LIMIT);
                let mut versions = lock(&VERSIONS);
                let entry = versions
                    .get_mut(&program)
                    .filter(|known| known.identity == identity);
                if let Some(known) = entry {
                    known.version = found;
                    known.finished = Some(Instant::now());
                }
            })
            .is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = (program, identity);
        false
    }
}

/// Runs both folder probes for this account: the login shell its account names, with this
/// process's HOME, then the npm the engine's places find. Output: the probed folders, and
/// whether both answered (false when the account or HOME is unknown).
#[cfg(unix)]
#[cfg_attr(test, allow(dead_code))]
fn probe_folders() -> (Vec<PathBuf>, bool) {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute());
    let account = nix::unistd::User::from_uid(nix::unistd::getuid())
        .ok()
        .flatten();
    let (Some(home), Some(account)) = (home, account) else {
        return (Vec::new(), false);
    };
    let login = Some(account.shell.as_path())
        .filter(|shell| shell.is_absolute())
        .and_then(|shell| login_path(shell, &home, &account.name, SHELL_LIMIT));
    let login_folders = login
        .as_deref()
        .map(|path| kept(path, Some(&home)))
        .unwrap_or_default();
    let (found, npm_answered) = probed(
        &login_folders,
        super::Places::from_environment(None),
        &home,
        &account.name,
    );
    (found, login.is_some() && npm_answered)
}

/// Runs a command with no input, standard error dropped, its own process group and a time
/// limit; at the limit, or once it exits, its whole group is killed, so nothing it started
/// can hold the output open. Inputs: the command and the limit. Output: at most 64 KiB of
/// standard output when it exits with success in time, else none.
#[cfg(unix)]
pub(crate) fn run_timed(command: &mut Command, limit: Duration) -> Option<Vec<u8>> {
    use std::os::unix::process::CommandExt;
    run_leading(command.process_group(0), limit)
}

/// Runs a command that leads its own process group, made so by the caller or by starting a
/// new session itself, as [`run_timed`] runs it. The output is read without a thread from a
/// non-blocking read end, closed when the run ends: at most [`OUTPUT_GRACE`] after the
/// program exits, even when a process outside the group still holds the write end.
/// Inputs: the command and the limit. Output: the standard output, or none.
#[cfg(unix)]
fn run_leading(command: &mut Command, limit: Duration) -> Option<Vec<u8>> {
    use std::process::Stdio;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let deadline = Instant::now() + limit;
    let mut child = crate::spawn_gate::spawn(command).ok()?;
    let mut stdout = child.stdout.take().expect("standard output is piped");
    let nonblocking = rustix::fs::fcntl_getfl(&stdout)
        .and_then(|flags| rustix::fs::fcntl_setfl(&stdout, flags | rustix::fs::OFlags::NONBLOCK))
        .is_ok();
    let mut output = Vec::new();
    let status = loop {
        if !nonblocking {
            break None;
        }
        drained(&mut stdout, &mut output);
        match child.try_wait() {
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) | Err(_) => break None,
            Ok(Some(status)) => break Some(status),
        }
    };
    kill_group(child.id());
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    if !status.success() {
        return None;
    }
    let grace = Instant::now() + OUTPUT_GRACE;
    while drained(&mut stdout, &mut output) == Pipe::Open && Instant::now() < grace {
        std::thread::sleep(POLL);
    }
    drop(stdout);
    assert!(output.len() <= OUTPUT_BYTES_MAX);
    Some(output)
}

/// Whether a run's output may still give bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pipe {
    Open,
    Ended,
}

/// Reads what a run's non-blocking output holds now, at most [`OUTPUT_BYTES_MAX`] in all.
/// Inputs: the read end and the bytes read so far. Output: whether more may come; a full
/// buffer, the end or a read error ends it.
#[cfg(unix)]
fn drained(stdout: &mut std::process::ChildStdout, output: &mut Vec<u8>) -> Pipe {
    use std::io::{ErrorKind, Read};
    let mut chunk = [0_u8; 4096];
    loop {
        let room = OUTPUT_BYTES_MAX
            .saturating_sub(output.len())
            .min(chunk.len());
        if room == 0 {
            return Pipe::Ended;
        }
        match stdout.read(&mut chunk[..room]) {
            Ok(0) => return Pipe::Ended,
            Ok(read) => output.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => return Pipe::Open,
            Err(_) => return Pipe::Ended,
        }
    }
}

/// Kills every process left in a timed run's group; a group already gone is fine.
/// Input: the run's process id, which is its group's id.
#[cfg(unix)]
fn kill_group(id: u32) {
    let pid = i32::try_from(id)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .expect("a child's process id is a positive i32");
    assert!(pid.as_raw_nonzero().get() > 1);
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
}

/// The PATH a login shell prints between [`BEGIN`] and [`END`], ignoring whatever its
/// startup files print around it. Input: the shell's output. Output: the PATH, or none.
#[cfg(unix)]
pub(crate) fn between_markers(output: &[u8]) -> Option<OsString> {
    use std::os::unix::ffi::OsStringExt;
    let mut lines = output.split(|byte| *byte == b'\n');
    lines.by_ref().find(|line| *line == BEGIN.as_bytes())?;
    let path = lines.next()?;
    (lines.next()? == END.as_bytes()).then(|| OsString::from_vec(path.to_vec()))
}

/// Runs the owner's login shell from HOME in a new session with no terminal, interactive so
/// the files that set up version managers are read, with the environment cleared to HOME,
/// USER, LOGNAME, SHELL, TERM and a system PATH, and reads the PATH it ends with.
/// Inputs: the shell, HOME, the user name and the limit. Output: the PATH, or none.
#[cfg(unix)]
pub(crate) fn login_path(
    shell: &Path,
    home: &Path,
    user: &str,
    limit: Duration,
) -> Option<OsString> {
    let script = format!("printf '\\n%s\\n' {BEGIN}; /usr/bin/printenv PATH; printf '%s\\n' {END}");
    let mut command = Command::new("/usr/bin/perl");
    command
        .args(["-mPOSIX", "-e", NEW_SESSION, "--"])
        .arg(shell)
        .args(["-i", "-l", "-c"])
        .arg(script)
        .current_dir(home)
        .env_clear()
        .env("HOME", home)
        .env("USER", user)
        .env("LOGNAME", user)
        .env("SHELL", shell)
        .env("TERM", "dumb")
        .env("PATH", SYSTEM_PATH);
    between_markers(&run_leading(&mut command, limit)?)
}

/// The folders of a PATH value a launch may search: absolute plain folders of bounded
/// length that exist, in no `.crazyagent` folder and in no project ([`super::in_project`],
/// the rule [`super::Places`] applies too), that only this user and the system can
/// change, each once, at most [`FOLDERS_MAX`]. Inputs: the PATH value and HOME. Output: the
/// folders in order.
pub(crate) fn kept(path: &OsStr, home: Option<&Path>) -> Vec<PathBuf> {
    kept_folders(std::env::split_paths(path), home)
}

/// Keeps the folders [`kept`] keeps from folders already split. Inputs: the folders and
/// HOME. Output: the kept folders in order.
fn kept_folders(folders: impl Iterator<Item = PathBuf>, home: Option<&Path>) -> Vec<PathBuf> {
    let real_home = home.and_then(|home| std::fs::canonicalize(home).ok());
    let mut kept: Vec<PathBuf> = Vec::new();
    for folder in folders {
        if kept.len() == FOLDERS_MAX {
            break;
        }
        if !kept.contains(&folder) && searchable(&folder, real_home.as_deref()) {
            kept.push(folder);
        }
    }
    assert!(kept.len() <= FOLDERS_MAX);
    kept
}

/// Whether one folder may be searched for programs ([`kept`]). Inputs: the folder and HOME
/// with its links resolved. Output: the answer.
fn searchable(folder: &Path, real_home: Option<&Path>) -> bool {
    let plain = folder.as_os_str().len() <= PATH_BYTES_MAX
        && folder.is_absolute()
        && folder
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)));
    if !plain || crate::harnesses::in_crazyagent_folder(folder) {
        return false;
    }
    let Ok(real) = std::fs::canonicalize(folder) else {
        return false;
    };
    real.is_dir()
        && !crate::harnesses::in_crazyagent_folder(&real)
        && !super::in_project(real_home, &real)
        && crate::service::changeable_by_others(folder).is_none()
}

/// The `bin` folder of the npm global prefix npm prints. Input: npm's output. Output: the
/// folder, or none for anything but one absolute path.
#[cfg(unix)]
pub(crate) fn prefix_bin(output: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let line = output.strip_suffix(b"\n").unwrap_or(output);
    if line.is_empty() || line.len() > PATH_BYTES_MAX || line.contains(&b'\n') {
        return None;
    }
    let prefix = Path::new(OsStr::from_bytes(line));
    prefix.is_absolute().then(|| prefix.join("bin"))
}

/// Asks a resolved npm for its global prefix with the environment cleared to HOME, USER,
/// LOGNAME and a system PATH, its interpreter named by path. Inputs: npm, HOME, the user
/// name and the limit. Output: the prefix's `bin` folder, or none.
#[cfg(unix)]
pub(crate) fn npm_bin(
    npm: &super::Resolved,
    home: &Path,
    user: &str,
    limit: Duration,
) -> Option<PathBuf> {
    let words = npm.argv();
    let (program, arguments) = words.split_first()?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .args(["prefix", "-g"])
        .current_dir(home)
        .env_clear()
        .env("HOME", home)
        .env("USER", user)
        .env("LOGNAME", user)
        .env("PATH", SYSTEM_PATH);
    prefix_bin(&run_timed(&mut command, limit)?)
}

/// The folders the probes add, in order: the `bin` folder of the global prefix of the npm
/// the places find once the login folders are added, then the login folders, each kept as
/// [`kept`] keeps them. Inputs: the kept login folders, the places, HOME and the user name.
/// Output: the folders, and false when a found npm failed to answer.
#[cfg(unix)]
pub(crate) fn probed(
    login: &[PathBuf],
    places: super::Places,
    home: &Path,
    user: &str,
) -> (Vec<PathBuf>, bool) {
    let npm = super::resolve("npm", &places.with_probed(login.to_vec())).ok();
    let bin = npm
        .as_ref()
        .and_then(|npm| npm_bin(npm, home, user, NPM_LIMIT));
    let answered = npm.is_none() || bin.is_some();
    let found = kept_folders(bin.into_iter().chain(login.iter().cloned()), Some(home));
    (found, answered)
}

/// A version from a version report such as `tmux 3.4` or `codex-cli v0.164.0`: the first
/// word holding a digit, without a leading `v`, made of digits, letters, `.`, `-` and `+`,
/// at most [`VERSION_BYTES_MAX`] bytes. Input: the report. Output: the version, or none.
pub(crate) fn reported(output: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(output);
    let word = text
        .split_whitespace()
        .find(|word| word.bytes().any(|byte| byte.is_ascii_digit()))?;
    let version = word.strip_prefix('v').unwrap_or(word);
    let plain = version.len() <= VERSION_BYTES_MAX
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'));
    plain.then(|| version.to_owned())
}

/// Runs a program with the flag it answers its version to (`-V` for tmux, `--version` for
/// the rest), through its interpreter when it is a script, from HOME with the environment
/// cleared to HOME and a system PATH. Inputs: the program's file and the limit.
/// Output: the version, or none.
#[cfg(unix)]
pub(crate) fn probe_version(program: &Path, limit: Duration) -> Option<String> {
    let places = super::Places::from_environment(None);
    let resolved = super::resolve_entry(program, &places).ok()?;
    let flag = if resolved.program.file_name() == Some(OsStr::new("tmux")) {
        "-V"
    } else {
        "--version"
    };
    let words = resolved.argv();
    let (first, arguments) = words.split_first()?;
    let mut command = Command::new(first);
    command
        .args(arguments)
        .arg(flag)
        .env_clear()
        .env("PATH", SYSTEM_PATH);
    if let Some(home) = places.home() {
        command.current_dir(home).env("HOME", home);
    }
    reported(&run_timed(&mut command, limit)?)
}
