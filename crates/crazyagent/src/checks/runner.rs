use super::snapshot::Snapshot;
use super::{AdmittedCheck, NetworkMode};
#[cfg(target_os = "macos")]
use super::{CheckRefusal, ReportFormat};
#[cfg(target_os = "macos")]
use crate::engine::owner::failure;
use crate::sandbox::SandboxRemote;
#[cfg(target_os = "macos")]
use crate::sandbox::toolchains::Toolchain;
use std::collections::BTreeMap;
#[cfg(target_os = "macos")]
use std::ffi::OsStr;
#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::io::{self, Read};
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// The most bytes of each output stream a run keeps: its tail.
pub const OUTPUT_CAP: usize = 4 * 1024 * 1024;
/// The most bytes of a run's report, all its files together.
pub const REPORT_CAP: usize = 8 * 1024 * 1024;
/// The most files of one Allure results folder a run reads.
pub const REPORT_FILES_MAX: usize = 20_000;
/// The most bytes of the toolchain version a run records.
pub const VERSION_BYTES_MAX: usize = 200;
/// The resource limits the macOS sandbox does not enforce on a check, stated in every run.
pub const GAPS: [&str; 3] = [
    "cpu_not_capped",
    "memory_not_capped",
    "process_count_not_capped",
];

#[cfg(not(target_os = "macos"))]
const OFF_MACOS: &str = "checks_unavailable_off_macos";
#[cfg(target_os = "macos")]
const UNAVAILABLE: &str = "checks_unavailable";
#[cfg(target_os = "macos")]
const SANDBOX_UNAVAILABLE: &str = "checks_sandbox_unavailable";
#[cfg(target_os = "macos")]
const VERSION_UNAVAILABLE: &str = "unavailable";
#[cfg(target_os = "macos")]
const VERSION_TIME: Duration = Duration::from_secs(30);
#[cfg(target_os = "macos")]
const VERSION_READ_MAX: usize = 1024;
#[cfg(target_os = "macos")]
const POLL: Duration = Duration::from_millis(20);
#[cfg(target_os = "macos")]
const GROUP_EXIT_WAIT: Duration = Duration::from_secs(10);
#[cfg(target_os = "macos")]
const DRAIN_WAIT: Duration = Duration::from_secs(5);
#[cfg(target_os = "macos")]
const CHUNK_BYTES: usize = 64 * 1024;

/// The bounds of one check run: its wall time (the shorter of this and the check's own
/// limit), the tail kept of each output stream, the report bytes read, the widest network
/// the run may get (the queue's ceiling, `remotes` by default and `off` for an untrusted
/// project) and the remotes `remotes` reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLimits {
    pub timeout: Duration,
    pub output_cap: usize,
    pub report_cap: usize,
    pub network_ceiling: NetworkMode,
    pub remotes: Vec<SandboxRemote>,
}

/// One report file a run left in its snapshot: its relative name and bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// How one check run went. `exit` is the exit code of a run that ended by itself;
/// `timed_out` and `cancelled` say which stop ended it otherwise; `unavailable` names why
/// it never started. The tails keep the last bytes of each stream with the count dropped
/// before them. `network` is the mode the run got, `toolchain_version` what the toolchain
/// printed inside the sandbox (`none` for no toolchain), the digests cover the installed
/// sandbox profile and every environment name and value, `cleanup_uncertain` says the
/// sandbox could not be confirmed empty afterwards, and `gaps` names the limits not enforced.
/// `holdout` marks a holdout check's run: its repository code saw the holdout files and
/// could print them, so its tails and report are owner-only wherever they are stored, and
/// an agent-facing view of it shows only the verdict and counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckExecution {
    pub holdout: bool,
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub unavailable: Option<String>,
    pub duration_ms: u64,
    pub stdout_tail: Vec<u8>,
    pub stdout_elided: u64,
    pub stderr_tail: Vec<u8>,
    pub stderr_elided: u64,
    pub report: Vec<ReportFile>,
    pub report_problem: Option<&'static str>,
    pub network: NetworkMode,
    pub toolchain_version: String,
    pub sandbox_digest: String,
    pub environment_digest: String,
    pub cleanup_uncertain: bool,
    pub gaps: &'static [&'static str],
}

impl CheckExecution {
    fn unstarted(network: NetworkMode, holdout: bool) -> Self {
        Self {
            holdout,
            exit: None,
            timed_out: false,
            cancelled: false,
            unavailable: None,
            duration_ms: 0,
            stdout_tail: Vec::new(),
            stdout_elided: 0,
            stderr_tail: Vec::new(),
            stderr_elided: 0,
            report: Vec::new(),
            report_problem: None,
            network,
            toolchain_version: "none".into(),
            sandbox_digest: String::new(),
            environment_digest: String::new(),
            cleanup_uncertain: false,
            gaps: &GAPS,
        }
    }
}

/// How one supervised process ended: its status when it ended by itself, which stop ended
/// it otherwise, its wall time, each stream's tail and dropped count, and whether its
/// sandbox could not be confirmed empty.
#[cfg(target_os = "macos")]
struct Ended {
    status: Option<ExitStatus>,
    timed_out: bool,
    cancelled: bool,
    duration: Duration,
    stdout: (Vec<u8>, u64),
    stderr: (Vec<u8>, u64),
    cleanup_uncertain: bool,
}

/// Runs one admitted check inside its snapshot, never as the owner: under the project
/// Seatbelt boundary confined to the snapshot clone, with a fresh home and temp folder in the
/// run folder, a short scratch folder for build output, the check's toolchain read-only and
/// the narrower of the check's network and the ceiling; no credentials, owner home, engine
/// runtime, mailbox or other worktree is granted. Holdout files are copied in for the run
/// only; a holdout check runs with network off and spends its snapshot, so every later run
/// on it is refused with `checks_snapshot_spent` and the queue runs holdout checks last,
/// each in its own snapshot. The toolchain version is read first in the same boundary. A
/// timeout or cancel kills the process group, and the run's warden then stops every process
/// left in its sandbox. Inputs: the snapshot, the check, its holdout bytes by path (empty
/// for an ordinary check), the limits and the cancel flag. Output: the execution;
/// `unavailable` names why it did not start, such as `checks_holdout_missing`.
pub fn run(
    snapshot: &Snapshot,
    check: &AdmittedCheck,
    holdout: &BTreeMap<String, Vec<u8>>,
    limits: &RunLimits,
    cancel: &AtomicBool,
) -> CheckExecution {
    let network = if check.holdout.is_some() {
        NetworkMode::Off
    } else {
        check.spec.network.min(limits.network_ceiling)
    };
    let mut execution = CheckExecution::unstarted(network, check.holdout.is_some());
    #[cfg(target_os = "macos")]
    if let Err(refusal) = confined(snapshot, check, holdout, limits, cancel, &mut execution) {
        execution.unavailable = Some(refusal.0);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (snapshot, holdout, cancel);
        execution.unavailable = Some(OFF_MACOS.into());
    }
    assert!(execution.unavailable.is_none() || execution.exit.is_none());
    assert!(!(execution.timed_out && execution.cancelled));
    assert!(execution.stdout_tail.len() <= limits.output_cap);
    assert!(execution.stderr_tail.len() <= limits.output_cap);
    assert!(execution.network <= limits.network_ceiling && execution.network <= check.spec.network);
    assert!(check.holdout.is_none() || execution.network == NetworkMode::Off);
    assert_eq!(execution.holdout, check.holdout.is_some());
    assert!(execution.report.is_empty() || !execution.cleanup_uncertain);
    execution
}

/// Prepares the boundary and runs the check; every refusal happens before the check
/// starts, and once it started the execution records how it went. Inputs: as [`run`],
/// with the execution to fill. Output: Ok, or the refusal that kept the check from starting.
#[cfg(target_os = "macos")]
fn confined(
    snapshot: &Snapshot,
    check: &AdmittedCheck,
    holdout: &BTreeMap<String, Vec<u8>>,
    limits: &RunLimits,
    cancel: &AtomicBool,
    execution: &mut CheckExecution,
) -> Result<(), CheckRefusal> {
    use crate::sandbox::toolchains::{self, ScopedTmp, ToolHomes, Toolchains};
    use crate::sandbox::{ProjectSandbox, SandboxNetwork};
    use sha2::{Digest, Sha256};
    if snapshot.spent() {
        return Err(failure(super::snapshot::SPENT));
    }
    verify_holdout(check, holdout)?;
    let folder = snapshot.folder();
    let home = fresh_folder(&folder.join("home"))?;
    let tmp = fresh_folder(&folder.join("tmp"))?;
    let homes = ToolHomes::owner().map_err(|_| failure(SANDBOX_UNAVAILABLE))?;
    let family: Vec<Toolchain> = check.spec.toolchain.into_iter().collect();
    let tools = Toolchains::grant(&homes, &family);
    if family.contains(&Toolchain::Rust) {
        toolchains::prepare_cargo_home(&home, &homes).map_err(|_| failure(UNAVAILABLE))?;
    }
    let scratch = ScopedTmp::make(&home).map_err(|_| failure(UNAVAILABLE))?;
    let (target, npm) = (scratch.path().join("target"), scratch.path().join("npm"));
    for made in [&target, &npm] {
        fresh_folder(made)?;
    }
    let remotes = match execution.network {
        NetworkMode::Remotes => limits.remotes.clone(),
        NetworkMode::Off | NetworkMode::Localhost => Vec::new(),
    };
    let sandbox = ProjectSandbox::new(folder, &home, &tmp, SandboxNetwork::Only(remotes))
        .and_then(|sandbox| sandbox.confine_to_clone(&snapshot.root))
        .and_then(|sandbox| sandbox.allow_tools(&tools))
        .and_then(|sandbox| sandbox.allow_scratch(scratch.path()))
        .map_err(|_| failure(SANDBOX_UNAVAILABLE))?;
    assert!(!sandbox.writes(folder) && sandbox.writes(&snapshot.root));
    let profile = folder.join("check.sb");
    sandbox
        .install(&profile)
        .map_err(|_| failure(SANDBOX_UNAVAILABLE))?;
    if execution.network == NetworkMode::Localhost {
        allow_loopback(&profile)?;
    }
    let installed = fs::read(&profile).map_err(|_| failure(SANDBOX_UNAVAILABLE))?;
    execution.sandbox_digest = format!("{:x}", Sha256::digest(&installed));
    let cwd = working_folder(&snapshot.root, &check.spec.cwd)?;
    let _placed = snapshot.place(holdout)?;
    let probe = sandbox.command(&profile, Path::new("/usr/bin/true"));
    let path = probe
        .get_envs()
        .find(|(name, _)| *name == "PATH")
        .and_then(|(_, value)| value)
        .map(OsStr::to_os_string)
        .unwrap_or_default();
    if let Some(toolchain) = check.spec.toolchain {
        execution.toolchain_version = version(&sandbox, &profile, &path, &cwd, toolchain);
    }
    let program = check
        .spec
        .argv
        .first()
        .and_then(|name| program(name, &path, &cwd))
        .ok_or_else(|| failure("checks_command_missing"))?;
    let mut command = sandbox.command(&profile, &program);
    command
        .args(&check.spec.argv[1..])
        .current_dir(&cwd)
        .env("CARGO_TARGET_DIR", &target)
        .env("npm_config_cache", &npm)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    execution.environment_digest = environment_digest(&command);
    let timeout = limits
        .timeout
        .min(Duration::from_secs(u64::from(check.spec.timeout_s)));
    let ended = supervise(&mut command, timeout, limits.output_cap, cancel)
        .map_err(|_| failure(SANDBOX_UNAVAILABLE))?;
    execution.exit = if ended.timed_out || ended.cancelled {
        None
    } else {
        ended.status.and_then(|status| status.code())
    };
    execution.timed_out = ended.timed_out;
    execution.cancelled = ended.cancelled;
    execution.duration_ms = u64::try_from(ended.duration.as_millis()).unwrap_or(u64::MAX);
    (execution.stdout_tail, execution.stdout_elided) = ended.stdout;
    (execution.stderr_tail, execution.stderr_elided) = ended.stderr;
    execution.cleanup_uncertain = ended.cleanup_uncertain;
    (execution.report, execution.report_problem) = read_report(
        &snapshot.root,
        check.spec.format,
        check.spec.report.as_deref(),
        limits.report_cap,
        execution.cleanup_uncertain,
    );
    Ok(())
}

/// Checks that the holdout bytes are exactly the files the admitted check names, each with
/// the SHA-256 the owner stored. Inputs: the check and the bytes by path. Output: Ok, or
/// `checks_holdout_missing` for a missing, extra or changed file.
#[cfg(target_os = "macos")]
fn verify_holdout(
    check: &AdmittedCheck,
    holdout: &BTreeMap<String, Vec<u8>>,
) -> Result<(), CheckRefusal> {
    use sha2::{Digest, Sha256};
    let none = BTreeMap::new();
    let named = check.holdout.as_ref().unwrap_or(&none);
    let same = named.len() == holdout.len()
        && named.iter().all(|(path, digest)| {
            holdout
                .get(path)
                .is_some_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == *digest)
        });
    if !same {
        return Err(failure("checks_holdout_missing"));
    }
    Ok(())
}

/// Makes one empty private folder of the run folder, removing what an earlier run on the
/// same snapshot left there, links included and never followed. Input: the path. Output:
/// the folder, or `checks_unavailable`.
#[cfg(target_os = "macos")]
fn fresh_folder(path: &Path) -> Result<PathBuf, CheckRefusal> {
    use std::os::unix::fs::DirBuilderExt;
    let cleared = match fs::symlink_metadata(path) {
        Ok(found) if found.is_dir() && !found.file_type().is_symlink() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    };
    cleared
        .and_then(|()| fs::DirBuilder::new().mode(0o700).create(path))
        .map_err(|_| failure(UNAVAILABLE))?;
    Ok(path.to_path_buf())
}

/// The folder a check starts in: the snapshot root for `.`, else a relative path whose
/// every component is a real folder of the snapshot. Inputs: the root and the check's
/// `cwd`. Output: the folder, or `checks_cwd_missing`.
#[cfg(target_os = "macos")]
fn working_folder(root: &Path, cwd: &str) -> Result<PathBuf, CheckRefusal> {
    if cwd == "." {
        return Ok(root.to_path_buf());
    }
    if !super::snapshot::safe_path(cwd) {
        return Err(failure("checks_cwd_missing"));
    }
    let mut at = root.to_path_buf();
    for part in cwd.split('/') {
        at.push(part);
        if !fs::symlink_metadata(&at)
            .is_ok_and(|found| found.is_dir() && !found.file_type().is_symlink())
        {
            return Err(failure("checks_cwd_missing"));
        }
    }
    assert!(at.starts_with(root));
    Ok(at)
}

/// Appends the loopback rules of network `localhost` to an installed profile: outbound to
/// any loopback port, bind and inbound on local ports, the owner's fixed ports kept denied
/// both ways so a check reaches no owner model endpoint.
/// Seatbelt matches a local address by port only, so a listener on another local address
/// is not refused; the run states that gap. Input: the profile. Output: Ok, or
/// `checks_sandbox_unavailable`.
#[cfg(target_os = "macos")]
fn allow_loopback(profile: &Path) -> Result<(), CheckRefusal> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let owner: String = crate::sandbox::OWNER_PORTS
        .iter()
        .map(|port| format!(" (local ip \"*:{port}\")"))
        .collect();
    let owner_remote: String = crate::sandbox::OWNER_PORTS
        .iter()
        .map(|port| format!(" (remote ip \"localhost:{port}\")"))
        .collect();
    let rules = format!(
        "(allow network-outbound (remote ip \"localhost:*\"))\n(allow network-bind (local ip \"localhost:*\"))\n(allow network-inbound (local ip \"localhost:*\"))\n(deny network-outbound{owner_remote})\n(deny network-bind{owner})\n(deny network-inbound{owner})\n"
    );
    assert!(!rules.contains("signal"));
    fs::OpenOptions::new()
        .append(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(profile)
        .and_then(|mut file| {
            file.write_all(rules.as_bytes())
                .and_then(|()| file.sync_all())
        })
        .map_err(|_| failure(SANDBOX_UNAVAILABLE))
}

/// The program a check's first item names: a path with a `/`, absolute or from the start
/// folder, or the first executable file of that name on the sandbox `PATH`, links kept so
/// a rustup proxy still sees its own name. Inputs: the name, the `PATH` and the start
/// folder. Output: the program, or none.
#[cfg(target_os = "macos")]
fn program(name: &str, path: &OsStr, cwd: &Path) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    if name.contains('/') {
        let program = if name.starts_with('/') {
            PathBuf::from(name)
        } else {
            cwd.join(name)
        };
        return program.is_file().then_some(program);
    }
    on_path(name, path)
}

#[cfg(target_os = "macos")]
fn on_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path)
        .filter(|folder| folder.is_absolute())
        .map(|folder| folder.join(name))
        .find(|candidate| {
            fs::metadata(candidate)
                .is_ok_and(|found| found.is_file() && found.permissions().mode() & 0o111 != 0)
        })
}

/// Reads the toolchain's version inside the check's boundary from its start folder, so a
/// toolchain file in the snapshot picks it: `rustc`, `node` or `python3` with `--version`.
/// Inputs: the sandbox, its profile, the sandbox `PATH`, the start folder and the family.
/// Output: the first printed line, at most [`VERSION_BYTES_MAX`] bytes, or `unavailable`.
#[cfg(target_os = "macos")]
fn version(
    sandbox: &crate::sandbox::ProjectSandbox,
    profile: &Path,
    path: &OsStr,
    cwd: &Path,
    toolchain: Toolchain,
) -> String {
    let tool = match toolchain {
        Toolchain::Rust => "rustc",
        Toolchain::Node => "node",
        Toolchain::Python => "python3",
    };
    let Some(program) = on_path(tool, path) else {
        return VERSION_UNAVAILABLE.into();
    };
    let mut command = sandbox.command(profile, &program);
    command
        .arg("--version")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let never = AtomicBool::new(false);
    let Ok(ended) = supervise(&mut command, VERSION_TIME, VERSION_READ_MAX, &never) else {
        return VERSION_UNAVAILABLE.into();
    };
    if !ended.status.is_some_and(|status| status.success()) || ended.stdout.1 > 0 {
        return VERSION_UNAVAILABLE.into();
    }
    let text = String::from_utf8_lossy(&ended.stdout.0);
    let line: String = text
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .scan(0, |used, character| {
            *used += character.len_utf8();
            (*used <= VERSION_BYTES_MAX).then_some(character)
        })
        .collect();
    if line.is_empty() {
        return VERSION_UNAVAILABLE.into();
    }
    assert!(line.len() <= VERSION_BYTES_MAX);
    line
}

/// The SHA-256 of every variable the command sets, names and values, sorted, each name and
/// value ended by a zero byte; no value is kept. Input: the command. Output: the hex digest.
#[cfg(target_os = "macos")]
fn environment_digest(command: &Command) -> String {
    use sha2::{Digest, Sha256};
    let mut pairs: Vec<(&OsStr, &OsStr)> = command
        .get_envs()
        .filter_map(|(name, value)| value.map(|value| (name, value)))
        .collect();
    pairs.sort();
    let mut hasher = Sha256::new();
    for (name, value) in pairs {
        hasher.update(name.as_encoded_bytes());
        hasher.update([0]);
        hasher.update(value.as_encoded_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

/// Starts one sandboxed command with its warden and supervises it until it ends, runs past
/// `timeout` or `cancel` is set. A process that did not end by itself has its group killed
/// while its leader is still unreaped, then the warden stops every process left in the
/// sandbox, and the group is awaited empty. Inputs: the command, the timeout, the tail kept
/// of each stream and the cancel flag. Output: how it ended, or the error that kept it from
/// starting.
#[cfg(target_os = "macos")]
fn supervise(
    command: &mut Command,
    timeout: Duration,
    cap: usize,
    cancel: &AtomicBool,
) -> io::Result<Ended> {
    use std::sync::atomic::Ordering;
    use std::time::Instant;
    let started = Instant::now();
    let (mut child, warden) = crate::sandbox::warden::spawn(command)?;
    let group = child.id();
    let stdout = child.stdout.take().map(|stream| drain(stream, cap));
    let stderr = child.stderr.take().map(|stream| drain(stream, cap));
    let (status, timed_out, cancelled) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (Some(status), false, false),
            Ok(None) => {}
            Err(_) => break (None, false, false),
        }
        if started.elapsed() >= timeout {
            break (None, true, false);
        }
        if cancel.load(Ordering::SeqCst) {
            break (None, false, true);
        }
        std::thread::sleep(POLL);
    };
    let duration = started.elapsed();
    if status.is_none() {
        stop_group(group);
    }
    let unconfirmed = crate::sandbox::warden::fire_reported(warden, "check").is_err();
    let status = status.or_else(|| child.wait().ok());
    await_group_exit(group);
    let (stdout, late_out) = collect(stdout);
    let (stderr, late_err) = collect(stderr);
    assert!(stdout.0.len() <= cap && stderr.0.len() <= cap);
    Ok(Ended {
        status,
        timed_out,
        cancelled,
        duration,
        stdout,
        stderr,
        cleanup_uncertain: unconfirmed || late_out || late_err,
    })
}

#[cfg(target_os = "macos")]
fn drain(
    stream: impl Read + Send + 'static,
    cap: usize,
) -> std::sync::mpsc::Receiver<(Vec<u8>, u64)> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(tail(stream, cap));
    });
    receiver
}

/// Waits up to [`DRAIN_WAIT`] for one stream's tail. Input: the stream's receiver, none
/// when the stream was not piped. Output: the tail and dropped count, and whether the
/// stream was still held open past the wait.
#[cfg(target_os = "macos")]
fn collect(receiver: Option<std::sync::mpsc::Receiver<(Vec<u8>, u64)>>) -> ((Vec<u8>, u64), bool) {
    let Some(receiver) = receiver else {
        return ((Vec::new(), 0), false);
    };
    match receiver.recv_timeout(DRAIN_WAIT) {
        Ok(kept) => (kept, false),
        Err(_) => ((Vec::new(), 0), true),
    }
}

/// Reads a stream to its end keeping only its last `cap` bytes. Inputs: the stream and the
/// cap. Output: the tail and how many bytes were dropped before it.
#[cfg(target_os = "macos")]
fn tail(mut stream: impl Read, cap: usize) -> (Vec<u8>, u64) {
    let mut kept: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
    let mut elided = 0u64;
    let mut chunk = vec![0u8; CHUNK_BYTES];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                kept.extend(&chunk[..read]);
                if kept.len() > cap {
                    let excess = kept.len() - cap;
                    kept.drain(..excess);
                    elided += excess as u64;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    assert!(kept.len() <= cap);
    (kept.into_iter().collect(), elided)
}

#[cfg(target_os = "macos")]
fn stop_group(group: u32) {
    let Some(pid) = i32::try_from(group)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return;
    };
    assert!(pid.as_raw_nonzero().get() > 1);
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
}

/// Waits until no process of a check's group is left, killing again any member still
/// there, for at most [`GROUP_EXIT_WAIT`]. Input: the group id.
#[cfg(target_os = "macos")]
fn await_group_exit(group: u32) {
    let Some(pid) = i32::try_from(group)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return;
    };
    let started = std::time::Instant::now();
    while rustix::process::test_kill_process_group(pid).is_ok()
        && started.elapsed() < GROUP_EXIT_WAIT
    {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Reads the report a check left in its snapshot: nothing for libtest and exit-code
/// checks, the JUnit file, or every `*-result.json` of the Allure folder, by name, never
/// through a link and at most `cap` bytes in all. Nothing is read when the run's cleanup is
/// uncertain, since a process it left could swap a folder on the path for a link between
/// the check and the open. Inputs: the snapshot root, the format, the report path, the cap
/// and whether cleanup is uncertain. Output: the files, and `report_missing`,
/// `report_link_refused`, `report_unreadable` or `report_too_large` when it cannot be read.
#[cfg(target_os = "macos")]
fn read_report(
    root: &Path,
    format: ReportFormat,
    report: Option<&str>,
    cap: usize,
    uncertain: bool,
) -> (Vec<ReportFile>, Option<&'static str>) {
    let read = match (format, report) {
        (ReportFormat::Libtest | ReportFormat::ExitCode, _) => return (Vec::new(), None),
        _ if uncertain => Err("report_unreadable"),
        (_, None) => Err("report_missing"),
        (ReportFormat::Junit, Some(name)) => inside(root, name).and_then(|path| {
            read_file(&path, cap).map(|bytes| {
                vec![ReportFile {
                    name: name.to_owned(),
                    bytes,
                }]
            })
        }),
        (ReportFormat::Allure, Some(name)) => {
            inside(root, name).and_then(|folder| allure(&folder, name, cap))
        }
    };
    match read {
        Ok(files) => {
            assert!(files.iter().map(|file| file.bytes.len()).sum::<usize>() <= cap);
            (files, None)
        }
        Err(problem) => (Vec::new(), Some(problem)),
    }
}

/// Resolves a report path inside the snapshot with no link on the way. Inputs: the root
/// and the relative path. Output: the path, or the report problem.
#[cfg(target_os = "macos")]
fn inside(root: &Path, name: &str) -> Result<PathBuf, &'static str> {
    if !super::snapshot::safe_path(name) {
        return Err("report_missing");
    }
    let parts: Vec<&str> = name.split('/').collect();
    let mut at = root.to_path_buf();
    for (index, part) in parts.iter().enumerate() {
        at.push(part);
        let found = fs::symlink_metadata(&at).map_err(|_| "report_missing")?;
        if found.file_type().is_symlink() {
            return Err("report_link_refused");
        }
        if index + 1 < parts.len() && !found.is_dir() {
            return Err("report_missing");
        }
    }
    assert!(at.starts_with(root));
    Ok(at)
}

/// Reads one report file without following a link or waiting on a pipe, at most `cap`
/// bytes. Inputs: the path and the cap. Output: the bytes, or the report problem.
#[cfg(target_os = "macos")]
fn read_file(path: &Path, cap: usize) -> Result<Vec<u8>, &'static str> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
        .map_err(|_| "report_unreadable")?;
    if !file.metadata().is_ok_and(|found| found.is_file()) {
        return Err("report_unreadable");
    }
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "report_unreadable")?;
    if bytes.len() > cap {
        return Err("report_too_large");
    }
    Ok(bytes)
}

/// Reads every `*-result.json` regular file of an Allure results folder, by name, within
/// [`REPORT_FILES_MAX`] files and `cap` bytes in all. Inputs: the folder, its relative name
/// and the cap. Output: the files, or the report problem.
#[cfg(target_os = "macos")]
fn allure(folder: &Path, name: &str, cap: usize) -> Result<Vec<ReportFile>, &'static str> {
    if !fs::symlink_metadata(folder).is_ok_and(|found| found.is_dir()) {
        return Err("report_missing");
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(folder).map_err(|_| "report_unreadable")? {
        let entry = entry.map_err(|_| "report_unreadable")?;
        let Some(file) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if file.ends_with("-result.json")
            && fs::symlink_metadata(entry.path()).is_ok_and(|found| found.is_file())
        {
            names.push(file);
        }
        if names.len() > REPORT_FILES_MAX {
            return Err("report_too_large");
        }
    }
    names.sort();
    let mut left = cap;
    let mut files = Vec::with_capacity(names.len());
    for file in names {
        let bytes = read_file(&folder.join(&file), left)?;
        left -= bytes.len();
        files.push(ReportFile {
            name: format!("{name}/{file}"),
            bytes,
        });
    }
    assert!(files.len() <= REPORT_FILES_MAX);
    Ok(files)
}
