#![cfg_attr(not(unix), allow(dead_code))]

use std::collections::BTreeMap;
use std::ffi::OsStr;
#[cfg(unix)]
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

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
const OUTPUT_BYTES_MAX: u64 = 64 * 1024;
/// The longest folder or program path the probes accept.
const PATH_BYTES_MAX: usize = 1024;
/// The most programs whose versions are cached; others stay unknown.
const VERSIONS_MAX: usize = 64;
/// How often a timed run checks whether its program exited.
const POLL: Duration = Duration::from_millis(10);
/// How long a timed run waits for the output once its program exited.
const OUTPUT_GRACE: Duration = Duration::from_millis(500);
/// The only search path a probed program inherits.
const SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
/// The perl program that starts the login shell in a new session with no terminal, so an
/// interactive shell can never take over the terminal the engine was started from.
const NEW_SESSION: &str = "POSIX::setsid() > 0 or exit 1; exec {$ARGV[0]} @ARGV or exit 1";

/// The folders the background probes found: none before the first [`folders`] call starts
/// them, empty while they run, then what they found.
static FOLDERS: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);
/// Each probed program's version, none while its probe runs or when it gave none.
static VERSIONS: Mutex<BTreeMap<PathBuf, Option<String>>> = Mutex::new(BTreeMap::new());

/// The folders the background probes found: npm's global prefix `bin` of the npm the
/// resolver finds and the login shell's PATH folders. Answers at once from what the probes
/// cached, starting them in the background, and is empty until they finish.
/// Output: the absolute folders.
pub fn folders() -> Vec<PathBuf> {
    let mut cached = lock(&FOLDERS);
    if cached.is_none() {
        *cached = Some(Vec::new());
        start_folder_probe();
    }
    cached.clone().unwrap_or_default()
}

/// A program's version as it reports it, read once in the background with a time limit and
/// cached. Input: the program's real file. Output: the version, or none while unknown,
/// which never counts as missing.
pub fn version(program: &Path) -> Option<String> {
    if !program.is_absolute() || program.as_os_str().len() > PATH_BYTES_MAX {
        return None;
    }
    let mut versions = lock(&VERSIONS);
    if let Some(known) = versions.get(program) {
        return known.clone();
    }
    if versions.len() < VERSIONS_MAX && start_version_probe(program) {
        versions.insert(program.to_path_buf(), None);
    }
    assert!(versions.len() <= VERSIONS_MAX);
    None
}

/// Locks a probe cache, keeping what it holds when a probe thread panicked while holding it.
/// Input: the cache. Output: its guard.
fn lock<T>(cache: &Mutex<T>) -> MutexGuard<'_, T> {
    cache.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts the folder probes on a background thread that caches what they find. Unit tests
/// never start them, so a developer's own shell files are never run, and where Unix shells
/// do not exist nothing is probed.
fn start_folder_probe() {
    #[cfg(all(unix, not(test)))]
    let _ = std::thread::Builder::new()
        .name("tools-probe".to_owned())
        .spawn(|| {
            let found = probe_folders();
            *lock(&FOLDERS) = Some(found);
        });
}

/// Starts one program's version probe on a background thread that fills its cache entry.
/// Input: the program. Output: whether the probe started.
fn start_version_probe(program: &Path) -> bool {
    #[cfg(unix)]
    {
        let program = program.to_path_buf();
        std::thread::Builder::new()
            .name("tools-version".to_owned())
            .spawn(move || {
                let found = probe_version(&program, VERSION_LIMIT);
                if let Some(entry) = lock(&VERSIONS).get_mut(&program) {
                    *entry = found;
                }
            })
            .is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = program;
        false
    }
}

/// Runs both folder probes for this account: the login shell its account names, with this
/// process's HOME, then the npm the engine's places find. Output: the probed folders, or
/// none when the account or HOME is unknown.
#[cfg(unix)]
#[cfg_attr(test, allow(dead_code))]
fn probe_folders() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute());
    let account = nix::unistd::User::from_uid(nix::unistd::getuid())
        .ok()
        .flatten();
    let (Some(home), Some(account)) = (home, account) else {
        return Vec::new();
    };
    let login = Some(account.shell.as_path())
        .filter(|shell| shell.is_absolute())
        .and_then(|shell| login_path(shell, &home, &account.name, SHELL_LIMIT))
        .map(|path| kept(&path, Some(&home)))
        .unwrap_or_default();
    probed(
        &login,
        super::Places::from_environment(None),
        &home,
        &account.name,
    )
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
/// new session itself, as [`run_timed`] runs it. Inputs: the command and the limit.
/// Output: the standard output, or none.
#[cfg(unix)]
fn run_leading(command: &mut Command, limit: Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let deadline = Instant::now() + limit;
    let mut child = crate::spawn_gate::spawn(command).ok()?;
    let stdout = child.stdout.take().expect("standard output is piped");
    let (sender, receiver) = std::sync::mpsc::channel();
    let reading = std::thread::Builder::new()
        .name("tools-probe-output".to_owned())
        .spawn(move || {
            let mut output = Vec::new();
            let read = stdout.take(OUTPUT_BYTES_MAX).read_to_end(&mut output);
            let _ = sender.send(read.map(|_| output));
        });
    let status = loop {
        match child.try_wait() {
            Ok(None) if reading.is_ok() && Instant::now() < deadline => std::thread::sleep(POLL),
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
    let grace = deadline
        .saturating_duration_since(Instant::now())
        .max(OUTPUT_GRACE);
    let output = receiver.recv_timeout(grace).ok()?.ok()?;
    assert!(u64::try_from(output.len()).is_ok_and(|length| length <= OUTPUT_BYTES_MAX));
    Some(output)
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
/// length that exist, in no `.crazyagent` folder and in no project (a folder at or below one
/// holding `.crazyagent`, HOME and `/` not counted), that only this user and the system can
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
        && !in_project(&real, real_home)
        && crate::service::changeable_by_others(folder).is_none()
}

/// Whether a real folder is a project or inside one: it or a folder above it, other than
/// HOME and `/`, holds a `.crazyagent` folder. Inputs: the real folder and the real HOME.
/// Output: the answer.
fn in_project(real: &Path, real_home: Option<&Path>) -> bool {
    real.ancestors()
        .filter(|above| Some(*above) != real_home && above.parent().is_some())
        .any(|above| {
            above
                .join(crate::projects_home::CRAZYAGENT_DIRECTORY)
                .is_dir()
        })
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
/// Output: the folders.
#[cfg(unix)]
pub(crate) fn probed(
    login: &[PathBuf],
    places: super::Places,
    home: &Path,
    user: &str,
) -> Vec<PathBuf> {
    let npm = super::resolve("npm", &places.with_probed(login.to_vec()))
        .ok()
        .and_then(|npm| npm_bin(&npm, home, user, NPM_LIMIT));
    kept_folders(npm.into_iter().chain(login.iter().cloned()), Some(home))
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
