use crate::engine::owner::{OwnerClient, OwnerClientKind};
use crate::paths::{Layout, RootEnvironment};
use kernel_common::Platform;
#[cfg(unix)]
use kernel_processes::service::UNLOAD_TIMEOUT;
use kernel_processes::service::{
    Hooks, POLL, Repair, START_TIMEOUT, Service, ServiceError, failure, persistable,
    retire_old_copies,
};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const LIVENESS_EVERY: Duration = Duration::from_secs(1);
const LOG_SCAN_BYTES: u64 = 4 << 20;

/// The engine's checks the kernel service steps call ([`Hooks`]): the version a running
/// engine reported ([`crate::engine::bootstrap::reported_engine_version`]), the data root
/// made ready ([`Layout::prepare`]) and the private folder rules of [`crate::paths`].
pub static HOOKS: Hooks = Hooks {
    reported_version: crate::engine::bootstrap::reported_engine_version,
    prepare_root,
    private_folder: |path| Ok(crate::paths::private_folder(path)?),
    is_private_folder: crate::paths::is_private_folder,
};

impl From<crate::paths::PathsError> for ServiceError {
    fn from(error: crate::paths::PathsError) -> Self {
        Self(error.0)
    }
}

/// Makes the data root ready as the engine does ([`Layout::prepare`]). Input: the data root.
/// Output: Ok, or the refusal in the engine's own text.
fn prepare_root(root: &Path) -> Result<(), ServiceError> {
    Ok(Layout::new(root.to_path_buf())?.prepare()?)
}

/// The engine service for the current user on one platform, named for the layout's data
/// root next to the platform's default data root ([`Service::current_user`]). Inputs: the
/// platform, the absolute executable the service runs and the engine's layout. Output: the
/// service with its name and definition path.
pub fn current_user(
    platform: Platform,
    executable: PathBuf,
    layout: &Layout,
) -> Result<Service, ServiceError> {
    let default_root = crate::paths::data_root(
        platform,
        &RootEnvironment {
            home_override: None,
            ..RootEnvironment::current()
        },
    )
    .ok();
    Service::current_user(platform, executable, layout.root(), default_root.as_deref())
}

/// Says whether an engine listens on a socket now. Input: the ordinary socket path.
/// Output: true when a connection is accepted.
#[cfg(unix)]
pub fn answering(socket: &Path) -> bool {
    kernel_processes::spawn_gate::connect_unix_std(socket).is_ok()
}

/// Says whether a checked engine serves the ordinary endpoint now on Windows: its pipe
/// answers and its server is the start the pipes file pins, with no byte written. Input:
/// the ordinary endpoint path. Output: the answer.
#[cfg(windows)]
pub fn answering(socket: &Path) -> bool {
    crate::ipc::owner_answering(socket)
}

/// What a client learns when it checks the engine before opening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCheck {
    Ready { version: String },
    Mismatch { version: String, message: String },
    Unavailable { reason: String },
}

/// Authenticates to the engine at a data root and compares its version with this app's.
/// Inputs: the root, the client kind and a bounded timeout. Output: ready, a version
/// mismatch with its plain message (also when a newer engine's handshake cannot be read),
/// or unavailable with the engine's reason.
pub async fn check_engine(root: &Path, kind: OwnerClientKind, timeout: Duration) -> EngineCheck {
    let reported = match OwnerClient::discover(root, kind, timeout).await {
        Ok((_, identity)) => identity.engine_version().to_owned(),
        Err(error) => match crate::engine::bootstrap::reported_engine_version(root) {
            Some(version) if version != kernel_processes::version::VERSION => version,
            _ => return EngineCheck::Unavailable { reason: error.0 },
        },
    };
    match kernel_processes::version::mismatch(&reported) {
        None => EngineCheck::Ready { version: reported },
        Some(message) => EngineCheck::Mismatch {
            version: reported,
            message,
        },
    }
}

/// How `ensure_running` found or started the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    AlreadyRunning,
    Service,
    Detached { pid: u32 },
}

/// Starts the engine for a client when none answers: through the installed service, or
/// else as a detached `<executable> engine --home <root>` with output in the engine log.
/// Inputs: the layout, the service, the executable, the PATH a detached engine gets and
/// the wait bound, which a service start outlasts while it runs ([`wait_started`]).
/// Output: how the engine runs once it answers, or an error naming the log; while a restore
/// still copies in the service's process, `engine_restoring`, with nothing stopped (L5).
pub async fn ensure_running(
    layout: &Layout,
    service: &Service,
    executable: &Path,
    path: Option<&OsStr>,
    bound: Duration,
) -> Result<Started, ServiceError> {
    if answering(&layout.socket()) {
        return Ok(Started::AlreadyRunning);
    }
    crate::restore::refuse_restart(layout.root(), service).map_err(failure)?;
    layout.prepare()?;
    let since = log_length(layout);
    if service.status()?.installed {
        let appdir = std::env::var_os("APPDIR").map(PathBuf::from);
        let kept = Service {
            executable: persistable(
                &service.executable,
                layout.root(),
                appdir.as_deref(),
                &HOOKS,
            )?,
            ..service.clone()
        };
        match kept.repair(executable, true, false, &HOOKS) {
            Repair::Start | Repair::Keep => service.start()?,
            Repair::Install => {
                kept.install(&HOOKS)?;
                retire_old_copies(layout.root(), &kept.executable, &HOOKS);
            }
            Repair::Refuse => return Err(service.refuse()),
        }
        wait_started(layout, service, bound, since).await?;
        return Ok(Started::Service);
    }
    let mut child = spawn_detached(layout, executable, path)?;
    let pid = child.id();
    wait_ready(layout, bound, Some(&mut child), since).await?;
    Ok(Started::Detached { pid })
}

fn spawn_detached(
    layout: &Layout,
    executable: &Path,
    path: Option<&OsStr>,
) -> Result<std::process::Child, ServiceError> {
    let log = layout.engine_log();
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let output = options
        .open(&log)
        .map_err(|error| failure(format!("{}: {error}", log.display())))?;
    let errors = output
        .try_clone()
        .map_err(|error| failure(format!("{}: {error}", log.display())))?;
    let mut command = std::process::Command::new(executable);
    command
        .arg("engine")
        .arg("--home")
        .arg(layout.root())
        .stdin(std::process::Stdio::null())
        .stdout(output)
        .stderr(errors);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    kernel_processes::spawn_gate::spawn(&mut command)
        .map_err(|error| failure(format!("{}: {error}", executable.display())))
}

/// Stops an engine that runs without the service, such as one a client started detached:
/// it sends SIGTERM to the process that serves the engine socket, then waits until the
/// socket is silent and the engine has let go of its database, so a new engine can start on
/// the same data folder. Input: the layout. Output: the stopped engine's pid, none when no
/// engine answers, or an error naming the pid and the engine log after the bound.
#[cfg(unix)]
pub fn stop_detached(layout: &Layout) -> Result<Option<u32>, ServiceError> {
    let Ok(stream) = kernel_processes::spawn_gate::connect_unix_std(layout.socket()) else {
        return Ok(None);
    };
    let pid = serving_pid(&stream)?;
    drop(stream);
    let target = rustix::process::Pid::from_raw(pid)
        .ok_or_else(|| failure("the engine socket names no process"))?;
    rustix::process::kill_process(target, rustix::process::Signal::TERM)
        .map_err(|error| failure(format!("the engine (pid {pid}) can't be stopped: {error}")))?;
    let deadline = Instant::now() + UNLOAD_TIMEOUT;
    while answering(&layout.socket()) || !crate::storage::engine_lock_released(layout.root()) {
        if Instant::now() >= deadline {
            return Err(failure(format!(
                "the engine (pid {pid}) still ran {} s after it was asked to stop; see {}",
                UNLOAD_TIMEOUT.as_secs(),
                layout.engine_log().display()
            )));
        }
        std::thread::sleep(POLL);
    }
    Ok(Some(pid.unsigned_abs()))
}

/// Finds no engine to stop where the Unix socket transport does not exist. Input unused.
/// Output: none.
#[cfg(not(unix))]
pub fn stop_detached(_layout: &Layout) -> Result<Option<u32>, ServiceError> {
    Ok(None)
}

/// The process that serves a connected engine socket, which must belong to this user and
/// not be this process. Input: the connected stream. Output: its pid.
#[cfg(unix)]
fn serving_pid(stream: &std::os::unix::net::UnixStream) -> Result<i32, ServiceError> {
    let unknown =
        |error: nix::Error| failure(format!("the engine socket's process is unknown: {error}"));
    #[cfg(target_os = "macos")]
    let (uid, pid) = (
        nix::unistd::getpeereid(stream).map_err(unknown)?.0.as_raw(),
        nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerPid)
            .map_err(unknown)?,
    );
    #[cfg(not(target_os = "macos"))]
    let (uid, pid) = {
        let peer = nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
            .map_err(unknown)?;
        (peer.uid(), peer.pid())
    };
    if uid != rustix::process::getuid().as_raw()
        || pid <= 0
        || pid.unsigned_abs() == std::process::id()
    {
        return Err(failure(
            "the engine socket is served by another user's process",
        ));
    }
    Ok(pid)
}

/// The engine log's length now, taken before a start so `wait_ready` reads only the
/// lines that start wrote. Input: the layout. Output: bytes, zero for no log yet.
pub fn log_length(layout: &Layout) -> u64 {
    std::fs::metadata(layout.engine_log()).map_or(0, |metadata| metadata.len())
}

fn logged_ready(layout: &Layout, since: u64) -> bool {
    use std::io::{Read, Seek};
    let Ok(mut log) = std::fs::File::open(layout.engine_log()) else {
        return false;
    };
    let length = log.metadata().map_or(0, |metadata| metadata.len());
    let start = if length < since { 0 } else { since };
    let mut text = String::new();
    log.seek(std::io::SeekFrom::Start(start)).is_ok()
        && log.take(LOG_SCAN_BYTES).read_to_string(&mut text).is_ok()
        && text.lines().any(|line| line == "ready")
}

/// Whether the last upgrade backup of the engine at a layout failed, as the record the
/// engine leaves beside its database says (F36), read without an engine. Input: the layout.
/// Output: the backup code, or none.
pub fn backup_failure(layout: &Layout) -> Option<&'static str> {
    kernel_storage::migrations::backup_failure(layout.root())
}

/// The code the TUI and the desktop show for an engine start that did not answer
/// ([`failure_code`]), read without an engine. Input: the layout. Output: the code.
pub fn start_failure(layout: &Layout) -> &'static str {
    failure_code(
        crate::research::runner::git_safe_refusal(),
        backup_failure(layout).is_some(),
    )
}

/// The cause named first: `git_safe_unavailable` when this computer's gits can't be kept
/// safe (#598 sec N1), then `engine_backup_failed` when the upgrade backup failed (F36),
/// else `engine_start_failed`. Inputs: the git refusal and whether the backup failed.
/// Output: the code.
fn failure_code(refusal: Option<&'static str>, backup_failed: bool) -> &'static str {
    match (refusal, backup_failed) {
        (Some(code), _) => code,
        (None, true) => "engine_backup_failed",
        (None, false) => "engine_start_failed",
    }
}

/// Why an engine that did not start may have stopped: its upgrade backup failed. Input: the
/// layout. Output: the words to add to the start failure, empty when no backup failed.
fn backup_reason(layout: &Layout) -> String {
    backup_failure(layout).map_or_else(String::new, |code| {
        format!(": it could not back up its data before an upgrade, so it changed nothing ({code})")
    })
}

/// Waits until the engine at a layout has written `ready` to its log after a start and
/// answers an authenticated read. Inputs: the layout, the bound, the detached engine to
/// watch, if any, and the log length before the start. Output: Ok once both hold, or an
/// error naming the engine log when the bound passes or the engine stops first.
pub async fn wait_ready(
    layout: &Layout,
    bound: Duration,
    mut child: Option<&mut std::process::Child>,
    since: u64,
) -> Result<(), ServiceError> {
    let deadline = Instant::now() + bound;
    loop {
        if answered(layout, since, PROBE_TIMEOUT.min(bound.max(POLL))).await {
            return Ok(());
        }
        if let Some(child) = child.as_deref_mut()
            && let Ok(Some(status)) = child.try_wait()
        {
            return Err(failure(format!(
                "the engine stopped while starting ({status}){}; see {}",
                backup_reason(layout),
                layout.engine_log().display()
            )));
        }
        if Instant::now() >= deadline {
            return Err(unanswered(layout, bound));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// How a start wait that did not fail ended: the engine answered, or the wait reached
/// [`START_TIMEOUT`] while the service manager still ran the process it was first seen
/// running, so the engine is still starting rather than stopped or relaunched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartWait {
    Answered,
    StillStarting,
}

/// Waits like [`wait_ready`] for an engine a service manager starts. Past `bound` it keeps
/// waiting, up to [`START_TIMEOUT`], while the manager still runs the process it was first
/// seen running before `bound`, and it fails at once when that process stops or another one takes its
/// place. Inputs: the layout, the service, the bound and the log length before the start.
/// Output: Ok once the engine answers, or an error naming the engine log.
pub async fn wait_started(
    layout: &Layout,
    service: &Service,
    bound: Duration,
    since: u64,
) -> Result<(), ServiceError> {
    match wait_start(layout, service, bound, since).await? {
        StartWait::Answered => Ok(()),
        StartWait::StillStarting => Err(unanswered(layout, START_TIMEOUT)),
    }
}

/// Waits like [`wait_started`], but a wait that reaches [`START_TIMEOUT`] looks at the
/// service manager once more and, when it still runs the process first seen running, ends
/// as [`StartWait::StillStarting`] instead of failing. Inputs: the layout, the service, the
/// bound and the log length before the start. Output: how the wait ended, or an error
/// naming the engine log when the engine stopped, was relaunched or never ran.
pub async fn wait_start(
    layout: &Layout,
    service: &Service,
    bound: Duration,
    since: u64,
) -> Result<StartWait, ServiceError> {
    assert!(bound <= START_TIMEOUT);
    let started = Instant::now();
    let limit = |seen: Option<u32>| if seen.is_some() { START_TIMEOUT } else { bound };
    let mut seen: Option<u32> = None;
    let mut looked: Option<Instant> = None;
    loop {
        if answered(layout, since, PROBE_TIMEOUT.min(bound.max(POLL))).await {
            return Ok(StartWait::Answered);
        }
        let due = started.elapsed() >= limit(seen);
        if due || looked.is_none_or(|at| at.elapsed() >= LIVENESS_EVERY) {
            looked = Some(Instant::now());
            let status = service.status()?;
            let running = status.pid.filter(|_| status.running);
            match (seen, running) {
                (None, pid) => seen = pid.filter(|_| started.elapsed() < bound),
                (Some(first), Some(pid)) if pid == first => {}
                (Some(_), _) => {
                    return Err(failure(format!(
                        "the engine stopped while starting{}; see {}",
                        backup_reason(layout),
                        layout.engine_log().display()
                    )));
                }
            }
        }
        if started.elapsed() >= limit(seen) {
            return match seen {
                Some(_) => Ok(StartWait::StillStarting),
                None => Err(unanswered(layout, bound)),
            };
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Whether the engine at a layout has written `ready` to its log after a start and answers
/// an authenticated read. Inputs: the layout, the log length before the start and the read's
/// bound. Output: the answer.
async fn answered(layout: &Layout, since: u64, probe: Duration) -> bool {
    logged_ready(layout, since)
        && answering(&layout.socket())
        && !matches!(
            check_engine(layout.root(), OwnerClientKind::Tui, probe).await,
            EngineCheck::Unavailable { .. }
        )
}

/// The error of an engine that did not answer within a bound. Inputs: the layout and the
/// bound. Output: the error naming the bound, a failed upgrade backup and the engine log.
fn unanswered(layout: &Layout, bound: Duration) -> ServiceError {
    failure(format!(
        "the engine did not answer within {} s{}; see {}",
        bound.as_secs(),
        backup_reason(layout),
        layout.engine_log().display()
    ))
}

#[cfg(test)]
mod tests;
