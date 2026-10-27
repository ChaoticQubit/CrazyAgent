use super::ports::{self, Ports};
use super::{env, sandbox, settings, token, webview};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rustix::process::{Pid, Signal};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

/// How long a server may go without a [`Servers::touch`] before it stops.
pub const IDLE_LIMIT: Duration = Duration::from_secs(15 * 60);

/// How long a launch may take to listen before it is stopped. A fresh build's first run
/// can wait in the macOS first-exec check.
pub const START_LIMIT: Duration = Duration::from_secs(90);

/// How long a stopping server's process group has to exit after SIGTERM before SIGKILL.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// The most IDE servers one engine runs at once.
pub const SERVERS_MAX: usize = 8;

/// The file in a project's IDE folder whose path the server's extension host receives in
/// [`env::CAPABILITY_VARIABLE`]; the engine bridge writes the capability there.
pub const CAPABILITY: &str = "capability";

/// The file in a project's IDE folder that holds the running server's output, new each launch.
pub const LOG: &str = "server.log";

/// The workspace file the engine writes in a project's IDE folder for the server to open.
pub const WORKSPACE: &str = "crazyagent.code-workspace";

const GROUP: &str = "server.pid";
const SERVER_DATA: &str = "server";
const EXTENSIONS: &str = "extensions";
const SERVER_MAIN: &str = "out/server-main.js";
const PROJECTS_MAX: usize = 256;
const GROUP_BYTES_MAX: u64 = 256;
const READY_POLL: Duration = Duration::from_millis(100);
const CONNECT_TIME: Duration = Duration::from_millis(500);
const WATCH_POLL: Duration = Duration::from_secs(1);
const STOP_POLL: Duration = Duration::from_millis(20);
const KILL_WAIT: Duration = Duration::from_secs(2);
#[cfg(target_os = "macos")]
const LISTENERS_BYTES_MAX: usize = 64 * 1024;

/// Where one project's running IDE server is reached: the workbench URL on 127.0.0.1, its
/// port, the webview host's port, the per-launch token file the desktop sets the session
/// cookie from, and the capability file the server's extension host reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub url: String,
    pub port: u16,
    pub webview_port: u16,
    pub token_path: PathBuf,
    pub capability_path: PathBuf,
}

/// The engine's IDE servers: at most one per project, each in its own process group on
/// 127.0.0.1 with its own token, stopped when idle, on [`Servers::stop_all`] and on drop.
pub struct Servers {
    folder: PathBuf,
    idle: Duration,
    slots: Mutex<HashMap<ProjectId, Arc<Slot>>>,
    launches: AtomicU64,
    closed: AtomicBool,
}

#[derive(Default)]
struct Slot {
    start: tokio::sync::Mutex<()>,
    state: Mutex<Option<Running>>,
}

struct Running {
    launch: u64,
    child: Child,
    group: Pid,
    info: ServerInfo,
    folder: PathBuf,
    touched: Instant,
    webview: tokio::task::JoinHandle<()>,
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Servers {
    /// The IDE servers of one engine, none running yet. Inputs: the engine's data folder,
    /// whose [`super::FOLDER`] folder holds their state, and how long a server may go
    /// untouched, [`IDLE_LIMIT`] outside tests. Output: the register.
    pub fn new(data: &Path, idle: Duration) -> Self {
        assert!(!idle.is_zero());
        Self {
            folder: data.join(super::FOLDER),
            idle,
            slots: Mutex::default(),
            launches: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        }
    }

    /// Ends every server a previous engine recorded and left running, across the project
    /// folders in `<data>/ide` (at most [`PROJECTS_MAX`]), so a crashed engine leaves no
    /// server behind for a project that is never opened again. The owner server runs it
    /// once at start, before any open. A folder others could write is reported and left
    /// alone. Input: the register. Output: none.
    pub async fn sweep_left(&self) {
        let user = rustix::process::getuid().as_raw();
        let private = |path: &Path| {
            fs::symlink_metadata(path).is_ok_and(|found| {
                found.file_type().is_dir() && found.uid() == user && found.mode() & 0o022 == 0
            })
        };
        if !self.folder.exists() {
            return;
        }
        if !private(&self.folder) {
            eprintln!("ide_folder_unsafe");
            return;
        }
        let Ok(entries) = fs::read_dir(&self.folder) else {
            return;
        };
        let mut swept = 0;
        for entry in entries.flatten().take(PROJECTS_MAX) {
            let folder = entry.path();
            if private(&folder) {
                sweep(&folder).await;
                swept += 1;
            }
        }
        assert!(swept <= PROJECTS_MAX);
    }

    /// The folder that holds one project's IDE state, `<data>/ide/<project>`. Input: the
    /// project. Output: the folder, or `ide_project_invalid` for the id `.`.
    pub fn project_folder(&self, project: &ProjectId) -> Result<PathBuf, EngineError> {
        if project.as_str() == "." {
            return Err(failure("ide_project_invalid"));
        }
        let folder = self.folder.join(project.as_str());
        assert_eq!(folder.parent(), Some(self.folder.as_path()));
        Ok(folder)
    }

    /// Opens a project's IDE server, starting it from `build` unless it already runs: the
    /// recorded ports while free, a new token, the allowlisted environment, its own process
    /// group, and the webview host. Returns once the server listens on 127.0.0.1 and no
    /// other process holds its port. The server runs under its Seatbelt profile from
    /// [`sandbox::profile`]; where no sandbox is qualified it never starts. Inputs: the
    /// project, its canonical folder and the canonical root of a verified build. Output:
    /// where the server is reached, or a plain refusal.
    pub async fn open(
        &self,
        project: &ProjectId,
        root: &Path,
        build: &Path,
    ) -> Result<ServerInfo, EngineError> {
        if !sandbox::qualified() {
            return Err(failure("ide_platform_unqualified"));
        }
        let slot = self.slot(project)?;
        let _start = slot.start.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(failure("ide_servers_closed"));
        }
        if let Some(info) = reuse(&slot).await {
            return Ok(info);
        }
        if self.running() >= SERVERS_MAX {
            return Err(failure("ide_servers_full"));
        }
        let build = checked_build(build)?;
        let folder = self.prepare(project)?;
        sweep(&folder).await;
        let recorded = ports::recorded(&folder);
        let (listener, webview_port) = webview::bind(recorded.map(|known| known.webview))
            .await
            .map_err(|_| failure("ide_port_unavailable"))?;
        let port = match recorded {
            Some(known) if known.server != webview_port && ports::free(known.server) => {
                known.server
            }
            _ => ports::pick(webview_port).map_err(|_| failure("ide_port_unavailable"))?,
        };
        let chosen = Ports {
            server: port,
            webview: webview_port,
        };
        if recorded != Some(chosen) {
            ports::record(&folder, chosen).map_err(|_| failure("ide_state_unavailable"))?;
        }
        let token_path = token::issue(&folder).map_err(|_| failure("ide_token_unavailable"))?;
        let info = ServerInfo {
            url: format!("http://127.0.0.1:{port}/"),
            port,
            webview_port,
            token_path,
            capability_path: folder.join(CAPABILITY),
        };
        let child = command(&build, &folder, root, &info)
            .and_then(|mut command| {
                crate::spawn_gate::spawn(&mut command)
                    .map_err(|_| failure("ide_server_start_failed"))
            })
            .inspect_err(|_| {
                let _ = token::remove(&folder);
            })?;
        let group = Pid::from_child(&child);
        assert!(group.as_raw_nonzero().get() > 1);
        record_group(&folder, group);
        let launch = self.launches.fetch_add(1, Ordering::SeqCst);
        let webview = tokio::spawn(webview::serve(
            listener,
            build.join(webview::PRE),
            webview_port,
        ));
        *locked(&slot.state) = Some(Running {
            launch,
            child,
            group,
            info: info.clone(),
            folder,
            touched: Instant::now(),
            webview,
        });
        if let Err(error) = ready(&slot, launch, port, group).await {
            stop_launch(&slot, launch).await;
            return Err(error);
        }
        if let Some(running) = locked(&slot.state).as_mut() {
            running.touched = Instant::now();
        }
        tokio::spawn(watch(Arc::downgrade(&slot), launch, self.idle));
        Ok(info)
    }

    /// Records that the owner still uses a project's IDE, the desktop's heartbeat, so its
    /// server does not stop for idleness. Input: the project. Output: whether one runs.
    pub fn touch(&self, project: &ProjectId) -> bool {
        let Some(slot) = locked(&self.slots).get(project).cloned() else {
            return false;
        };
        let mut state = locked(&slot.state);
        match state.as_mut() {
            Some(running) => {
                running.touched = Instant::now();
                true
            }
            None => false,
        }
    }

    /// Stops a project's server: SIGTERM to its process group, SIGKILL after
    /// [`STOP_GRACE`], then its token is removed. Input: the project. Output: whether one
    /// was running.
    pub async fn stop(&self, project: &ProjectId) -> bool {
        let Some(slot) = locked(&self.slots).get(project).cloned() else {
            return false;
        };
        let _start = slot.start.lock().await;
        let running = locked(&slot.state).take();
        let stopped = running.is_some();
        if let Some(running) = running {
            finish(running).await;
        }
        stopped
    }

    /// Stops every server, as the engine stops, and refuses later opens. Every group gets
    /// SIGTERM at once, so a launch still starting fails at once instead of holding the
    /// stop back. Output: none; each server is gone or its lingering group was logged.
    pub async fn stop_all(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let slots: Vec<Arc<Slot>> = locked(&self.slots).values().cloned().collect();
        for slot in &slots {
            if let Some(running) = locked(&slot.state).as_ref()
                && group_present(running.group)
            {
                let _ = rustix::process::kill_process_group(running.group, Signal::TERM);
            }
        }
        for slot in slots {
            let _start = slot.start.lock().await;
            let running = locked(&slot.state).take();
            if let Some(running) = running {
                finish(running).await;
            }
        }
    }

    /// The process group of a project's running server, for tests that watch it end.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn group(&self, project: &ProjectId) -> Option<i32> {
        let slot = locked(&self.slots).get(project).cloned()?;
        let state = locked(&slot.state);
        state
            .as_ref()
            .map(|running| running.group.as_raw_nonzero().get())
    }

    fn slot(&self, project: &ProjectId) -> Result<Arc<Slot>, EngineError> {
        let mut slots = locked(&self.slots);
        if let Some(slot) = slots.get(project) {
            return Ok(slot.clone());
        }
        if slots.len() >= PROJECTS_MAX {
            slots.retain(|_, slot| Arc::strong_count(slot) > 1 || locked(&slot.state).is_some());
            if slots.len() >= PROJECTS_MAX {
                return Err(failure("ide_servers_full"));
            }
        }
        let slot = slots.entry(project.clone()).or_default().clone();
        assert!(slots.len() <= PROJECTS_MAX);
        Ok(slot)
    }

    fn running(&self) -> usize {
        locked(&self.slots)
            .values()
            .filter(|slot| locked(&slot.state).is_some())
            .count()
    }

    fn prepare(&self, project: &ProjectId) -> Result<PathBuf, EngineError> {
        let folder = self.project_folder(project)?;
        for path in [
            self.folder.clone(),
            folder.clone(),
            folder.join(SERVER_DATA),
            folder.join(EXTENSIONS),
        ] {
            token::private_folder(&path).map_err(|_| failure("ide_folder_unsafe"))?;
        }
        Ok(folder)
    }
}

impl Drop for Servers {
    /// Kills every server still running when the register goes away, so no IDE server
    /// outlives the engine that started it.
    fn drop(&mut self) {
        let slots = std::mem::take(&mut *locked(&self.slots));
        for slot in slots.into_values() {
            let Some(mut running) = locked(&slot.state).take() else {
                continue;
            };
            running.webview.abort();
            if matches!(running.child.try_wait(), Ok(None)) || group_present(running.group) {
                let _ = rustix::process::kill_process_group(running.group, Signal::KILL);
            }
            let _ = running.child.kill();
            let _ = running.child.wait();
            let _ = token::remove(&running.folder);
            let _ = fs::remove_file(running.folder.join(GROUP));
        }
    }
}

/// Hands back the running server of a slot and marks it used; a server that exited is
/// cleaned up instead. Input: the slot, its start lock held. Output: the server, if alive.
async fn reuse(slot: &Slot) -> Option<ServerInfo> {
    let dead = {
        let mut state = locked(&slot.state);
        let running = state.as_mut()?;
        if matches!(running.child.try_wait(), Ok(None)) {
            running.touched = Instant::now();
            return Some(running.info.clone());
        }
        state.take()
    };
    if let Some(dead) = dead {
        finish(dead).await;
    }
    None
}

/// Waits until a launch listens on its port, the launch ends or [`START_LIMIT`] passes.
async fn ready(slot: &Slot, launch: u64, port: u16, group: Pid) -> Result<(), EngineError> {
    let deadline = Instant::now() + START_LIMIT;
    loop {
        {
            let mut state = locked(&slot.state);
            let Some(running) = state.as_mut().filter(|running| running.launch == launch) else {
                return Err(failure("ide_server_stopped"));
            };
            if !matches!(running.child.try_wait(), Ok(None)) {
                return Err(failure("ide_server_exited"));
            }
        }
        let connected = tokio::time::timeout(
            CONNECT_TIME,
            crate::spawn_gate::connect_tcp(("127.0.0.1", port)),
        )
        .await;
        if matches!(connected, Ok(Ok(_))) {
            return owned_listener(port, group).await;
        }
        if Instant::now() >= deadline {
            return Err(failure("ide_server_start_timeout"));
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

/// Stops a launch once it exited or went untouched for `idle`, checking again under the
/// start lock so an open that just reused it keeps it. Ends with the launch.
async fn watch(weak: Weak<Slot>, launch: u64, idle: Duration) {
    loop {
        tokio::time::sleep(WATCH_POLL.min(idle)).await;
        let Some(slot) = weak.upgrade() else {
            return;
        };
        match due(&slot, launch, idle) {
            None => return,
            Some(false) => continue,
            Some(true) => {}
        }
        let _start = slot.start.lock().await;
        match due(&slot, launch, idle) {
            None => return,
            Some(false) => continue,
            Some(true) => {
                stop_launch(&slot, launch).await;
                return;
            }
        }
    }
}

/// Whether a launch should stop: none when the slot holds another launch or none, else
/// whether it exited or went untouched for `idle`.
fn due(slot: &Slot, launch: u64, idle: Duration) -> Option<bool> {
    let mut state = locked(&slot.state);
    let running = state.as_mut().filter(|running| running.launch == launch)?;
    Some(!matches!(running.child.try_wait(), Ok(None)) || running.touched.elapsed() >= idle)
}

async fn stop_launch(slot: &Slot, launch: u64) {
    let running = locked(&slot.state).take_if(|running| running.launch == launch);
    if let Some(running) = running {
        finish(running).await;
    }
}

/// Ends a launch: the webview host, the process group, its token and group record.
async fn finish(mut running: Running) {
    running.webview.abort();
    let _ = (&mut running.webview).await;
    end_group(&mut running.child, running.group).await;
    let _ = token::remove(&running.folder);
    let _ = fs::remove_file(running.folder.join(GROUP));
}

/// Ends a server's process group: SIGTERM, then SIGKILL after [`STOP_GRACE`], and reaps
/// its leader. A signal is sent only while the group still exists, so a reused process id
/// is never hit. Inputs: the leader and its group.
async fn end_group(child: &mut Child, group: Pid) {
    if group_present(group) {
        let _ = rustix::process::kill_process_group(group, Signal::TERM);
    }
    let grace = Instant::now() + STOP_GRACE;
    while !ended(child, group) && Instant::now() < grace {
        tokio::time::sleep(STOP_POLL).await;
    }
    let wait = Instant::now() + KILL_WAIT;
    while !ended(child, group) && Instant::now() < wait {
        let _ = rustix::process::kill_process_group(group, Signal::KILL);
        tokio::time::sleep(STOP_POLL).await;
    }
    if !ended(child, group) {
        eprintln!(
            "ide_server_group_lingers group={}",
            group.as_raw_nonzero().get()
        );
    }
}

fn ended(child: &mut Child, group: Pid) -> bool {
    !matches!(child.try_wait(), Ok(None)) && !group_present(group)
}

fn group_present(group: Pid) -> bool {
    !matches!(
        rustix::process::test_kill_process_group(group),
        Err(rustix::io::Errno::SRCH)
    )
}

fn checked_build(build: &Path) -> Result<PathBuf, EngineError> {
    let real = fs::canonicalize(build).map_err(|_| failure("ide_build_invalid"))?;
    let kind = |path: PathBuf| fs::symlink_metadata(path).map(|metadata| metadata.file_type());
    if real != build
        || !kind(real.join("node")).is_ok_and(|found| found.is_file())
        || !kind(real.join(SERVER_MAIN)).is_ok_and(|found| found.is_file())
        || !kind(real.join(webview::PRE)).is_ok_and(|found| found.is_dir())
    {
        return Err(failure("ide_build_invalid"));
    }
    Ok(real)
}

/// The server's command line under the Seatbelt profile installed for this launch, after
/// the AI lock is written ([`settings::write`]); a lock that cannot be written stops the
/// start. Loopback host and port, the token file (never the token), its own data folder
/// with the Remote settings folder locked, the locked-keys file readable, its extensions
/// folder read only, telemetry off, a cleared environment holding only
/// [`env::server_env`] with TMPDIR at its short temp folder, output to the launch's log, and
/// its own process group.
fn command(
    build: &Path,
    folder: &Path,
    root: &Path,
    info: &ServerInfo,
) -> Result<Command, EngineError> {
    let log = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(folder.join(LOG))
        .map_err(|_| failure("ide_state_unavailable"))?;
    let errors = log
        .try_clone()
        .map_err(|_| failure("ide_state_unavailable"))?;
    let data = folder
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| failure("ide_state_unavailable"))?;
    let home = folder.join(SERVER_DATA);
    let written = settings::write(folder, &home)?;
    let tmp = crate::sandbox::toolchains::make_tmp(&home)
        .map_err(|_| failure("ide_state_unavailable"))?;
    let (extensions, workspace) = (folder.join(EXTENSIONS), folder.join(WORKSPACE));
    let machine = settings::machine_folder(&home);
    let grants = sandbox::Grants {
        data,
        root,
        build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[extensions.as_path()],
        readable: &[
            info.token_path.as_path(),
            info.capability_path.as_path(),
            workspace.as_path(),
            written.locked.as_path(),
        ],
        locked: &[machine.as_path()],
    };
    let profile = sandbox::install(folder, &grants, info.port)?;
    let mut command = sandbox::command(&profile, &build.join("node"));
    command
        .arg(build.join(SERVER_MAIN))
        .args(["--host", "127.0.0.1", "--port"])
        .arg(info.port.to_string())
        .arg("--connection-token-file")
        .arg(&info.token_path)
        .arg("--accept-server-license-terms")
        .arg("--server-data-dir")
        .arg(&home)
        .arg("--extensions-dir")
        .arg(folder.join(EXTENSIONS))
        .args(["--telemetry-level", "off"])
        .current_dir(folder)
        .env_clear()
        .envs(env::server_env(
            std::env::vars_os(),
            &info.capability_path,
            &written.locked,
        ))
        .env("TMPDIR", &tmp)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(errors)
        .process_group(0);
    assert!(
        command
            .get_args()
            .all(|arg| arg != "--connection-token" && arg != "--without-connection-token")
    );
    assert!(command.get_envs().count() <= env::ENV_MAX);
    assert_eq!(command.get_program(), crate::sandbox::SANDBOX_EXEC);
    Ok(command)
}

/// Records a launch's process group and its leader's start time, so the next engine can
/// end a server that outlived a crashed one without hitting a reused process id.
fn record_group(folder: &Path, group: Pid) {
    let pid = group.as_raw_nonzero().get();
    let started = crate::storage::harness_groups::start_identity(pid)
        .ok()
        .flatten()
        .unwrap_or_default();
    let written = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(folder.join(GROUP))
        .and_then(|mut file| file.write_all(format!("{pid} {started}\n").as_bytes()));
    if written.is_err() {
        eprintln!("ide_server_group_unrecorded");
    }
}

/// Ends the server a previous engine recorded for this project and left running: its group
/// is killed when the leader is gone or still has the recorded start time.
async fn sweep(folder: &Path) {
    let path = folder.join(GROUP);
    if let Some((group, started)) = recorded_group(&path) {
        let ours = group_present(group)
            && match crate::storage::harness_groups::start_identity(group.as_raw_nonzero().get()) {
                Ok(None) => true,
                Ok(Some(now)) => started.as_deref() == Some(now.as_str()),
                Err(_) => false,
            };
        let wait = Instant::now() + KILL_WAIT;
        while ours && group_present(group) && Instant::now() < wait {
            let _ = rustix::process::kill_process_group(group, Signal::KILL);
            tokio::time::sleep(STOP_POLL).await;
        }
    }
    let _ = fs::remove_file(&path);
}

fn recorded_group(path: &Path) -> Option<(Pid, Option<String>)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > GROUP_BYTES_MAX {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let (pid, started) = text.strip_suffix('\n')?.split_once(' ')?;
    let group = pid
        .parse::<i32>()
        .ok()
        .filter(|pid| *pid > 1)
        .and_then(Pid::from_raw)?;
    Some((group, (!started.is_empty()).then(|| started.to_owned())))
}

/// Checks that every listener on the server's port belongs to its process group and binds
/// 127.0.0.1 only, through `lsof`. Inputs: the port and the group. Output: done, or
/// `ide_server_port_taken` or `ide_server_not_loopback`.
#[cfg(target_os = "macos")]
async fn owned_listener(port: u16, group: Pid) -> Result<(), EngineError> {
    let listeners = tokio::task::spawn_blocking(move || listeners(port))
        .await
        .map_err(|_| failure("ide_server_check_failed"))??;
    if listeners.is_empty()
        || listeners.iter().any(|(pid, _)| {
            Pid::from_raw(*pid).is_none_or(|pid| rustix::process::getpgid(Some(pid)) != Ok(group))
        })
    {
        return Err(failure("ide_server_port_taken"));
    }
    let loopback = format!("127.0.0.1:{port}");
    if listeners.iter().any(|(_, address)| *address != loopback) {
        return Err(failure("ide_server_not_loopback"));
    }
    Ok(())
}

/// Refuses where no listener check is qualified.
#[cfg(not(target_os = "macos"))]
async fn owned_listener(_port: u16, _group: Pid) -> Result<(), EngineError> {
    Err(failure("ide_platform_unqualified"))
}

/// Lists the processes listening on a TCP port and the address each binds, through `lsof`.
/// Input: the port. Output: process ids with addresses, empty when none listens.
#[cfg(target_os = "macos")]
pub(crate) fn listeners(port: u16) -> Result<Vec<(i32, String)>, EngineError> {
    let output = crate::spawn_gate::output(
        Command::new("/usr/sbin/lsof")
            .env_clear()
            .env("LC_ALL", "C")
            .arg("-nP")
            .arg(format!("-iTCP:{port}"))
            .args(["-sTCP:LISTEN", "-Fpn"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .map_err(|_| failure("ide_server_check_failed"))?;
    if output.stdout.len() > LISTENERS_BYTES_MAX {
        return Err(failure("ide_server_check_failed"));
    }
    let text =
        std::str::from_utf8(&output.stdout).map_err(|_| failure("ide_server_check_failed"))?;
    if !output.status.success() && !text.is_empty() {
        return Err(failure("ide_server_check_failed"));
    }
    let mut pid = None;
    let mut found = Vec::new();
    for line in text.lines() {
        if let Some(number) = line.strip_prefix('p') {
            pid = Some(
                number
                    .parse::<i32>()
                    .map_err(|_| failure("ide_server_check_failed"))?,
            );
        } else if let Some(address) = line.strip_prefix('n') {
            let pid = pid.ok_or_else(|| failure("ide_server_check_failed"))?;
            found.push((pid, address.to_owned()));
        }
    }
    Ok(found)
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
