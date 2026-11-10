use super::ports::{self, Ports};
use super::workspace::IdePolicy;
use super::{bridge, env, extensions, output, proxy, sandbox, settings, token};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::sandbox::policy::AgentAccess;
use rustix::process::{Pid, Signal};
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fs;
use std::io::{PipeWriter, Write};
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

/// The file in a project's IDE folder that holds the running server's output, new each
/// launch, every line through [`output::redact`].
pub const LOG: &str = "server.log";

/// How long the server keeps a closed workbench's extension host for a reconnect; its own
/// default is three hours.
pub const RECONNECTION_GRACE: Duration = Duration::from_secs(30);

/// The workspace file the engine writes in a project's IDE folder for the server to open,
/// the only one the proxy lets the workbench open.
pub const WORKSPACE: &str = super::workspace::FILE;

const GROUP: &str = "server.pid";
const SERVER_DATA: &str = "server-data";
const EXTENSIONS: &str = "extensions";
const LAUNCHER: &str = "bin/codium-server";
const SERVER_MAIN: &str = "out/server-main.js";
const PROJECTS_MAX: usize = 256;
/// How long in all the engine's stop waits for start locks. A blocking call left by a
/// timed-out open can hold one for as long as its lookup hangs.
const STOP_LEFTOVER_WAIT: Duration = Duration::from_secs(10);
const GROUP_BYTES_MAX: u64 = 256;
const READY_POLL: Duration = Duration::from_millis(100);
const CONNECT_TIME: Duration = Duration::from_millis(500);
const WATCH_POLL: Duration = Duration::from_secs(1);
const STOP_POLL: Duration = Duration::from_millis(20);
const KILL_WAIT: Duration = Duration::from_secs(2);
#[cfg(target_os = "macos")]
const LISTENERS_BYTES_MAX: usize = 64 * 1024;

/// Where one project's running IDE is reached: the workbench URL on the engine proxy at
/// 127.0.0.1, the proxy's port, which also serves the webview hosts, the server's own port
/// behind it, the per-launch token file the desktop sets the session cookie from, the
/// capability file the server's extension host reads, and the workspace file the workbench
/// is pinned to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub url: String,
    pub port: u16,
    pub server_port: u16,
    pub token_path: PathBuf,
    pub capability_path: PathBuf,
    pub workspace: PathBuf,
}

/// The engine's IDE servers: at most one per project, each in its own process group on
/// 127.0.0.1 with its own token and its built-in extension's capability, stopped when idle,
/// on [`Servers::stop_all`] and on drop.
pub struct Servers {
    folder: PathBuf,
    idle: Duration,
    start: Duration,
    grants: Arc<bridge::Grants>,
    socket: PathBuf,
    settings_files: Option<(PathBuf, PathBuf)>,
    slots: Mutex<HashMap<ProjectId, Arc<Slot>>>,
    launches: AtomicU64,
    closed: AtomicBool,
    deleting: Mutex<HashMap<ProjectId, usize>>,
}

/// Keeps a project's IDE refused while its Delete everything runs. Made by
/// [`Servers::hold_for_delete`]; dropping it ends the refusal, on every way out of the delete.
pub struct DeleteHold<'a> {
    servers: &'a Servers,
    project: ProjectId,
}

impl Drop for DeleteHold<'_> {
    fn drop(&mut self) {
        let mut deleting = locked(&self.servers.deleting);
        let holds = deleting
            .get_mut(&self.project)
            .expect("a live hold is counted");
        assert!(*holds > 0);
        *holds -= 1;
        if *holds == 0 {
            deleting.remove(&self.project);
        }
    }
}

#[derive(Default)]
struct Slot {
    start: Arc<tokio::sync::Mutex<()>>,
    state: Mutex<Option<Running>>,
}

struct Running {
    launch: u64,
    project: ProjectId,
    grants: Arc<bridge::Grants>,
    child: Child,
    group: Pid,
    info: ServerInfo,
    folder: PathBuf,
    /// The saved policy this launch started under; [`reuse`] hands it back only while that
    /// still [`IdePolicy::holds`].
    policy: IdePolicy,
    touched: Instant,
    started: Instant,
    /// Whether the launch passed [`ready`]: it listens on 127.0.0.1 and no other process
    /// holds its port. Only a ready launch is handed back to a later open.
    ready: bool,
    proxy: tokio::task::JoinHandle<()>,
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The extension preparations under way, one lock per server extensions folder: a
/// preparation that runs on after the IDE tab's timeout cut its open still holds its lock,
/// so the next open of that project waits for it instead of installing beside it.
static PREPARING: Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>> = Mutex::new(BTreeMap::new());

/// The lock of one server extensions folder's preparation, the same one for every caller
/// while any holds it; locks nobody holds are dropped as another is handed out. Input: the
/// extensions folder. Output: its lock.
fn preparation(extensions: &Path) -> Arc<Mutex<()>> {
    let mut preparing = locked(&PREPARING);
    preparing.retain(|_, held| Arc::strong_count(held) > 1);
    Arc::clone(preparing.entry(extensions.to_path_buf()).or_default())
}

impl Servers {
    /// The IDE servers of one engine, none running yet. Inputs: the engine's data folder,
    /// whose [`super::FOLDER`] folder holds their state, how long a server may go untouched
    /// and how long a launch may take to listen, [`IDLE_LIMIT`] and [`START_LIMIT`] outside
    /// tests, the owner server's IDE capability grants and its owner socket,
    /// `<data>/<root>/owner.sock`, the one the IDE sandbox lets a server reach, and the
    /// engine's two settings files, none for an engine opened without them. Output: the
    /// register.
    pub(crate) fn new(
        data: &Path,
        idle: Duration,
        start: Duration,
        grants: Arc<bridge::Grants>,
        socket: PathBuf,
        settings_files: Option<(PathBuf, PathBuf)>,
    ) -> Self {
        assert!(!idle.is_zero());
        assert!(!start.is_zero());
        assert_eq!(socket.parent().and_then(Path::parent), Some(data));
        assert_eq!(socket.file_name(), Some(std::ffi::OsStr::new("owner.sock")));
        Self {
            folder: data.join(super::FOLDER),
            idle,
            start,
            grants,
            socket,
            settings_files,
            slots: Mutex::default(),
            launches: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            deleting: Mutex::default(),
        }
    }

    /// The engine's two settings files, for a terminal that reads them as it starts. Output:
    /// the paths, or none for an engine opened without them.
    pub(crate) fn settings_files(&self) -> Option<(PathBuf, PathBuf)> {
        self.settings_files.clone()
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
    /// recorded ports while free, a new token, the allowlisted environment with the owner
    /// socket, its own process group, its output redacted into [`LOG`], the built-in extension
    /// installed with a new capability for this launch ([`Servers::spawn`]), its extensions
    /// brought to the owner's approvals ([`extensions::on_server_start`]) and the engine proxy
    /// in front of it. Returns
    /// once the server listens on 127.0.0.1 and no other process holds its port. The server
    /// runs at the project's saved access level ([`super::workspace::ide_policy`]), under its
    /// Seatbelt profile from [`sandbox::profile`] below Full access; one left running under a
    /// level or network the owner has since changed is stopped first. A start reads the
    /// owner's local model ports for that profile to keep ([`super::workspace::kept_ports`]),
    /// and settings that do not read stop it. Where no sandbox is qualified it never starts. The idle watch starts with the launch, so a launch whose
    /// open is cut while it starts is still stopped once it exits, idles or misses its start
    /// limit, and is never handed back to a later open. Inputs: the project, its canonical
    /// folder, the canonical root of a verified build and whether the owner lets the IDE run
    /// the project's git hooks in that folder ([`super::git::shim_text`]). Output: where the
    /// IDE is reached, or a plain refusal, `ide_project_deleting` while a
    /// [`Servers::hold_for_delete`] lives.
    pub async fn open(
        &self,
        project: &ProjectId,
        root: &Path,
        build: &Path,
        project_hooks: bool,
    ) -> Result<ServerInfo, EngineError> {
        if !sandbox::qualified() {
            return Err(failure("ide_platform_unqualified"));
        }
        let slot = self.slot(project)?;
        let mut start = Some(Arc::clone(&slot.start).lock_owned().await);
        if self.closed.load(Ordering::SeqCst) {
            return Err(failure("ide_servers_closed"));
        }
        if self.deleting(project) {
            return Err(failure("ide_project_deleting"));
        }
        let policy = holding_start(&mut start, {
            let (data, project) = (self.data_root(), project.clone());
            move || super::workspace::ide_policy(&data, &project)
        })
        .await??;
        if let Some(info) = reuse(&slot, policy).await {
            return Ok(info);
        }
        let kept_ports = holding_start(&mut start, {
            let settings = self.settings_files.clone();
            move || super::workspace::kept_ports(settings.as_ref())
        })
        .await??;
        if self.running() >= SERVERS_MAX {
            return Err(failure("ide_servers_full"));
        }
        let build = checked_build(build)?;
        let folder = self.prepare(project)?;
        sweep(&folder).await;
        let recorded = ports::recorded(&folder);
        let (listener, port) = proxy::bind(recorded.map(|known| known.proxy))
            .await
            .map_err(|_| failure("ide_port_unavailable"))?;
        let server_port = match recorded {
            Some(known) if known.server != port && ports::free(known.server) => known.server,
            _ => ports::pick(port).map_err(|_| failure("ide_port_unavailable"))?,
        };
        let chosen = Ports {
            server: server_port,
            proxy: port,
        };
        if recorded != Some(chosen) {
            ports::record(&folder, chosen).map_err(|_| failure("ide_state_unavailable"))?;
        }
        let (token_path, token) =
            token::issue(&folder).map_err(|_| failure("ide_token_unavailable"))?;
        let info = ServerInfo {
            url: format!("http://127.0.0.1:{port}/"),
            port,
            server_port,
            token_path,
            capability_path: folder.join(CAPABILITY),
            workspace: folder.join(WORKSPACE),
        };
        let Some(workspace) = info.workspace.to_str().map(str::to_owned) else {
            let _ = token::remove(&folder);
            return Err(failure("ide_folder_unsafe"));
        };
        let started = async {
            let output = output::start(&folder.join(LOG), &token)
                .map_err(|_| failure("ide_state_unavailable"))?;
            let launch = Launch {
                project,
                build: &build,
                folder: &folder,
                root,
                socket: &self.socket,
                policy,
                kept_ports: &kept_ports,
            };
            let mut command =
                command_off_thread(launch, &info, output, project_hooks, &mut start).await?;
            self.prepare_extensions(&build, &folder).await?;
            self.spawn(project, &slot, &info, &mut command)
        }
        .await;
        let child = started.inspect_err(|_| {
            let _ = token::remove(&folder);
        })?;
        let group = Pid::from_child(&child);
        assert!(group.as_raw_nonzero().get() > 1);
        record_group(&folder, group);
        let launch = self.launches.fetch_add(1, Ordering::SeqCst);
        let proxy = tokio::spawn(proxy::serve(
            listener,
            proxy::Site {
                port,
                upstream: server_port,
                pre: build.join(proxy::PRE),
                token,
                workspace,
            },
        ));
        *locked(&slot.state) = Some(Running {
            launch,
            project: project.clone(),
            grants: Arc::clone(&self.grants),
            child,
            group,
            info: info.clone(),
            folder,
            policy,
            touched: Instant::now(),
            started: Instant::now(),
            ready: false,
            proxy,
        });
        tokio::spawn(watch(Arc::downgrade(&slot), launch, self.idle, self.start));
        let listening = ready(&slot, launch, server_port, group, self.start, &self.closed).await;
        if let Err(error) = listening {
            stop_launch(&slot, launch).await;
            return Err(error);
        }
        if let Some(running) = locked(&slot.state)
            .as_mut()
            .filter(|running| running.launch == launch)
        {
            running.ready = true;
            running.touched = Instant::now();
        }
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

    /// Whether a Delete everything holds this project's IDE refused. Asked before an open does
    /// any work, so a refused open writes nothing; [`Servers::open`] asks again under the
    /// start lock. Input: the project. Output: true while a hold lives.
    pub fn deleting(&self, project: &ProjectId) -> bool {
        locked(&self.deleting).contains_key(project)
    }

    /// Stops a project's server for Delete everything and refuses its opens while the returned
    /// hold lives, so no server runs inside folders on their way to the Trash. The hold counts
    /// before the stop waits for the start lock, so an open already starting is stopped here
    /// and a later one is refused. Input: the project. Output: the hold.
    pub async fn hold_for_delete(&self, project: &ProjectId) -> DeleteHold<'_> {
        *locked(&self.deleting).entry(project.clone()).or_default() += 1;
        let hold = DeleteHold {
            servers: self,
            project: project.clone(),
        };
        self.stop(project).await;
        hold
    }

    /// Stops every server, as the engine stops, and refuses later opens. Every group gets
    /// SIGTERM at once, so a launch still starting fails at once instead of holding the
    /// stop back. The wait for start locks shares one cap, so a blocking call left hung by
    /// a timed-out open cannot hold up the engine's stop.
    /// Output: none; each server is gone or its lingering group was logged.
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
        let deadline = tokio::time::Instant::now() + STOP_LEFTOVER_WAIT;
        for slot in slots {
            let _start = tokio::time::timeout_at(deadline, slot.start.lock()).await;
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
            slots.retain(|_, slot| {
                Arc::strong_count(slot) > 1
                    || Arc::strong_count(&slot.start) > 1
                    || locked(&slot.state).is_some()
            });
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

    /// Brings a project's server extensions to the owner's approvals before it starts, after
    /// [`command`] wrote the pin list into its Remote settings: the built-in extension
    /// installed and listed ([`bridge::install_extension`]), every approved extension
    /// installed from its verified VSIX and every other one but the built-in removed
    /// ([`extensions::on_server_start`]), off the async runtime and after any preparation of
    /// the same folder still running ([`preparation`]). A failure stops the start. Inputs:
    /// the build root and the project's IDE folder. Output: Ok, or a plain refusal.
    async fn prepare_extensions(&self, build: &Path, folder: &Path) -> Result<(), EngineError> {
        let data = self
            .folder
            .parent()
            .ok_or_else(|| failure("ide_state_unavailable"))?
            .to_path_buf();
        assert!(folder.starts_with(&self.folder));
        let (build, extensions, home) = (
            build.to_path_buf(),
            folder.join(EXTENSIONS),
            folder.join(SERVER_DATA),
        );
        tokio::task::spawn_blocking(move || {
            let preparing = preparation(&extensions);
            let _preparing = locked(&preparing);
            bridge::install_extension(&extensions)?;
            extensions::on_server_start(
                &data,
                &extensions::Registry::open_vsx(),
                &build,
                &extensions,
                &home,
                crate::team::now_ms(),
            )
        })
        .await
        .map_err(|_| failure("ide_extension_install_failed"))?
    }

    /// Starts a launch's server: refuses once [`Servers::stop_all`] began (a stop that begins
    /// after this check is caught by [`ready`]), issues the project's capability for the
    /// built-in extension at the launch's capability path, replacing any earlier one, then
    /// spawns; a spawn that fails revokes it again. Inputs: the project, its slot with no
    /// launch, the launch's info and its command. Output: the leader, or a plain refusal.
    fn spawn(
        &self,
        project: &ProjectId,
        slot: &Slot,
        info: &ServerInfo,
        command: &mut Command,
    ) -> Result<Child, EngineError> {
        {
            let state = locked(&slot.state);
            assert!(state.is_none());
            if self.closed.load(Ordering::SeqCst) {
                return Err(failure("ide_servers_closed"));
            }
        }
        let data = self
            .folder
            .parent()
            .ok_or_else(|| failure("ide_state_unavailable"))?;
        let capability = self.grants.issue(data, project)?;
        assert_eq!(capability, info.capability_path);
        crate::spawn_gate::spawn(command).map_err(|_| {
            let _ = self.grants.revoke(project);
            failure("ide_server_start_failed")
        })
    }

    /// The engine data root this register's IDE folder sits in, which holds the database the
    /// projects' saved policies are read from.
    fn data_root(&self) -> PathBuf {
        let data = self.folder.parent();
        data.expect("the IDE folder is inside the data root")
            .to_path_buf()
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
            running.proxy.abort();
            let _ = running.grants.revoke(&running.project);
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

/// Hands back the running server of a slot and marks it used. A server that exited, one
/// that never passed [`ready`] because the open that started it was cut, or one the saved
/// policy no longer [`IdePolicy::holds`] for is stopped instead, so an open only ever gets a
/// server that listens, owns its port and runs at the project's level. Inputs: the slot, its
/// start lock held, and the saved policy. Output: the server, if alive, ready and current.
async fn reuse(slot: &Slot, policy: IdePolicy) -> Option<ServerInfo> {
    let unusable = {
        let mut state = locked(&slot.state);
        let running = state.as_mut()?;
        if running.ready
            && running.policy.holds(policy)
            && matches!(running.child.try_wait(), Ok(None))
        {
            running.touched = Instant::now();
            return Some(running.info.clone());
        }
        state.take()
    };
    if let Some(unusable) = unusable {
        finish(unusable).await;
    }
    None
}

/// Waits until a launch listens on its port, the launch ends, [`Servers::stop_all`] began
/// (`closed`) or its `start` limit passes.
async fn ready(
    slot: &Slot,
    launch: u64,
    port: u16,
    group: Pid,
    start: Duration,
    closed: &AtomicBool,
) -> Result<(), EngineError> {
    let deadline = Instant::now() + start;
    loop {
        if closed.load(Ordering::SeqCst) {
            return Err(failure("ide_servers_closed"));
        }
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

/// Stops a launch once it exited, went untouched for `idle`, or was not ready within
/// `start` of its start because the open that started it was cut, checking again under the
/// start lock so an open that is still starting or just reused it keeps it. Runs from the
/// moment the launch is stored and ends with the launch.
async fn watch(weak: Weak<Slot>, launch: u64, idle: Duration, start: Duration) {
    loop {
        tokio::time::sleep(WATCH_POLL.min(idle).min(start)).await;
        let Some(slot) = weak.upgrade() else {
            return;
        };
        match due(&slot, launch, idle, start) {
            None => return,
            Some(false) => continue,
            Some(true) => {}
        }
        let _start = slot.start.lock().await;
        match due(&slot, launch, idle, start) {
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
/// whether it exited, went untouched for `idle`, or is still not ready `start` after it
/// started.
fn due(slot: &Slot, launch: u64, idle: Duration, start: Duration) -> Option<bool> {
    let mut state = locked(&slot.state);
    let running = state.as_mut().filter(|running| running.launch == launch)?;
    let exited = !matches!(running.child.try_wait(), Ok(None));
    let unready = !running.ready && running.started.elapsed() >= start;
    Some(exited || unready || running.touched.elapsed() >= idle)
}

async fn stop_launch(slot: &Slot, launch: u64) {
    let running = locked(&slot.state).take_if(|running| running.launch == launch);
    if let Some(running) = running {
        finish(running).await;
    }
}

/// Ends a launch: the proxy, its built-in extension's capability, the process group, its
/// token and group record.
async fn finish(mut running: Running) {
    running.proxy.abort();
    let _ = running.grants.revoke(&running.project);
    let _ = (&mut running.proxy).await;
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
        || !kind(real.join(LAUNCHER)).is_ok_and(|found| found.is_file())
        || !kind(real.join("node")).is_ok_and(|found| found.is_file())
        || !kind(real.join(SERVER_MAIN)).is_ok_and(|found| found.is_file())
        || !kind(real.join(proxy::PRE)).is_ok_and(|found| found.is_dir())
    {
        return Err(failure("ide_build_invalid"));
    }
    Ok(real)
}

/// Where one IDE server launch runs: its project, the verified build, its state folder,
/// the project root, the owner socket its built-in extension connects to, the saved policy
/// it runs at and the owner's local model ports its profile keeps.
struct Launch<'a> {
    project: &'a ProjectId,
    build: &'a Path,
    folder: &'a Path,
    root: &'a Path,
    socket: &'a Path,
    policy: IdePolicy,
    kept_ports: &'a [u16],
}

/// The folders and files of one server launch: the engine data root, the launch's server
/// data folder, short temp folder, user data folder and extensions folder, the locked-keys
/// file and the path of the engine-written git config.
struct Places<'a> {
    data: &'a Path,
    home: PathBuf,
    tmp: PathBuf,
    user_data: PathBuf,
    extensions: PathBuf,
    locked: &'a Path,
    git_config: PathBuf,
}

/// [`command`] on a blocking thread, so its git lookup, which runs xcrun on a Mac without the
/// Command Line Tools and retries at every start until one succeeds, never holds an async
/// worker. Inputs: as [`command`], and the start lock [`holding_start`] keeps with the thread.
/// Output: its command, or `ide_state_unavailable` when the thread died.
async fn command_off_thread(
    launch: Launch<'_>,
    info: &ServerInfo,
    output: PipeWriter,
    project_hooks: bool,
    start: &mut Option<tokio::sync::OwnedMutexGuard<()>>,
) -> Result<Command, EngineError> {
    let (project, policy) = (launch.project.clone(), launch.policy);
    let kept_ports = launch.kept_ports.to_vec();
    let (build, folder, root, socket) = (
        launch.build.to_path_buf(),
        launch.folder.to_path_buf(),
        launch.root.to_path_buf(),
        launch.socket.to_path_buf(),
    );
    let info = info.clone();
    holding_start(start, move || {
        let launch = Launch {
            project: &project,
            build: &build,
            folder: &folder,
            root: &root,
            socket: &socket,
            policy,
            kept_ports: &kept_ports,
        };
        command(launch, &info, output, project_hooks)
    })
    .await?
}

/// Runs blocking work with the project's start lock moved onto its thread, so when the open
/// is dropped by its timeout and the thread runs on, a retry waits for the leftover instead
/// of writing beside it. Inputs: the held lock, handed back when the work returns, and the
/// work. Output: what the work returned, or `ide_state_unavailable` when its thread died.
async fn holding_start<T: Send + 'static>(
    start: &mut Option<tokio::sync::OwnedMutexGuard<()>>,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, EngineError> {
    let held = start.take();
    assert!(held.is_some(), "blocking work runs under the start lock");
    let (held, made) = tokio::task::spawn_blocking(move || {
        let made = work();
        (held, made)
    })
    .await
    .map_err(|_| failure("ide_state_unavailable"))?;
    *start = held;
    Ok(made)
}

/// The server's command line, IDE-1 verdict 3, after the AI lock is written
/// ([`settings::write`]); a lock that cannot be written stops the start. The build's launcher
/// at the project's level ([`launcher_command`]) with [`server_arguments`]; a cleared
/// environment holding only [`env::server_env`], with the owner `socket` the built-in
/// extension connects to and TMPDIR at its short temp folder; stdout and stderr into the
/// redacting `output` pipe, and its own process group. In a confined server the git shim lets
/// the project's hooks run in `root` only when `project_hooks` is on, and every git it starts
/// is held by [`confined_git`]. A Full access server's git is the owner's own, hooks and
/// config included, so its shim holds no hooks path, whose count line needs the forced keys.
/// Inputs: the launch, the server's info, the output pipe and the hooks switch.
fn command(
    launch: Launch<'_>,
    info: &ServerInfo,
    output: PipeWriter,
    project_hooks: bool,
) -> Result<Command, EngineError> {
    let (folder, policy) = (launch.folder, launch.policy);
    let errors = output
        .try_clone()
        .map_err(|_| failure("ide_state_unavailable"))?;
    let data = folder
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| failure("ide_state_unavailable"))?;
    let home = folder.join(SERVER_DATA);
    let confined = policy.access != AgentAccess::Full;
    let hooks = (confined && project_hooks).then_some(launch.root);
    let written = settings::write(folder, &home, data, hooks, policy.access)?;
    let ceiling = confined.then(|| confined_git(data, &launch)).transpose()?;
    let places = Places {
        data,
        tmp: crate::sandbox::toolchains::make_tmp(&home)
            .map_err(|_| failure("ide_state_unavailable"))?,
        user_data: settings::user_data_folder(&home),
        extensions: folder.join(EXTENSIONS),
        locked: &written.locked,
        git_config: folder.join(super::git::GLOBAL_CONFIG),
        home,
    };
    let mut command = launcher_command(&launch, info, &places)?;
    server_arguments(&mut command, info, &places);
    let git = ceiling.as_deref().map(|ceiling| env::ServerGit {
        ceiling,
        config: &places.git_config,
    });
    command
        .env_clear()
        .envs(env::server_env(
            std::env::vars_os(),
            &info.capability_path,
            launch.socket,
            places.locked,
            git,
        ))
        .env("TMPDIR", &places.tmp)
        .stdin(Stdio::null())
        .stdout(output)
        .stderr(errors)
        .process_group(0);
    assert!(
        command
            .get_args()
            .all(|arg| arg != "--connection-token" && arg != "--without-connection-token")
    );
    assert!(command.get_envs().count() <= env::ENV_MAX);
    assert_eq!(
        policy.access == AgentAccess::Full,
        command.get_program() != crate::sandbox::SANDBOX_EXEC
    );
    Ok(command)
}

/// Holds the git of a confined server: writes the engine's own user-level config into the IDE
/// folder with the owner's name and email ([`super::git::write_global`]), since the profile
/// denies the one in the owner's home, and finds where git's search stops: `root`'s agents
/// root, and its older one while an engine-made worktree of the project stays there
/// ([`super::workspace::older_agents`]). Inputs: the engine data root and the launch. Output:
/// the ceiling, or a refusal, as for a `root` holding a `:`.
fn confined_git(data: &Path, launch: &Launch<'_>) -> Result<OsString, EngineError> {
    let older_agents = super::workspace::older_agents(data, launch.project, launch.root)?;
    let ceiling = super::git::ceiling(launch.root, older_agents)?;
    let owner_home = std::env::var_os("HOME");
    super::git::write_global(
        launch.folder,
        launch.root,
        owner_home.as_deref().map(Path::new),
    )?;
    Ok(ceiling)
}

/// The launcher's command at the project's level: under the Seatbelt profile installed for
/// this launch at Sandboxed and Own branch, with the network
/// [`crate::sandbox::policy::ide_network`] gives that level, and with no profile at Full
/// access. The profile keeps the Remote settings folder locked and the extensions folder read
/// only. Inputs: the launch, the server's info and its places. Output: the command, or the
/// profile's refusal.
fn launcher_command(
    launch: &Launch<'_>,
    info: &ServerInfo,
    places: &Places<'_>,
) -> Result<Command, EngineError> {
    let launcher = launch.build.join(LAUNCHER);
    let level = match launch.policy.access {
        AgentAccess::Full => return Ok(Command::new(&launcher)),
        level @ (AgentAccess::Sandboxed | AgentAccess::OwnBranch) => level,
    };
    let machine = settings::machine_folder(&places.home);
    assert!(machine.starts_with(&places.user_data) && places.user_data.starts_with(&places.home));
    let grants = sandbox::Grants {
        data: places.data,
        root: launch.root,
        build: launch.build,
        home: &places.home,
        tmp: &places.tmp,
        writable: &[],
        read_only: &[places.extensions.as_path()],
        readable: &[
            info.token_path.as_path(),
            info.capability_path.as_path(),
            info.workspace.as_path(),
            places.locked,
            places.git_config.as_path(),
        ],
        locked: &[machine.as_path()],
    };
    let network = crate::sandbox::policy::ide_network(level, launch.policy.network);
    let profile = sandbox::install(
        launch.folder,
        &grants,
        info.server_port,
        network,
        launch.kept_ports,
    )?;
    Ok(sandbox::command(&profile, &launcher))
}

/// Adds the server's command line: 127.0.0.1 at the server port, the token file (never the
/// token), its own server data folder, the user data folder inside it, the extensions folder,
/// license accepted, telemetry off, workspace trust off and a 30 s reconnection grace, with
/// its server data folder as the working directory, since a profile lets it read no folder
/// above. Inputs: the command, the server's info and the launch's places.
fn server_arguments(command: &mut Command, info: &ServerInfo, places: &Places<'_>) {
    command
        .args(["--host", "127.0.0.1", "--port"])
        .arg(info.server_port.to_string())
        .arg("--connection-token-file")
        .arg(&info.token_path)
        .arg("--server-data-dir")
        .arg(&places.home)
        .arg("--user-data-dir")
        .arg(&places.user_data)
        .arg("--extensions-dir")
        .arg(&places.extensions)
        .arg("--accept-server-license-terms")
        .args(["--telemetry-level", "off"])
        .arg("--disable-workspace-trust")
        .arg("--reconnection-grace-time")
        .arg(RECONNECTION_GRACE.as_secs().to_string())
        .current_dir(&places.home);
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
pub(crate) mod tests;
