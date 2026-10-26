#[cfg(unix)]
use crate::harnesses::launch::{self};
use crate::harnesses::launch::{
    AccountBinding, LaunchError, LaunchEvent, LaunchLimits, LaunchRequest, Secret, Session,
};
use crate::protocol::{CredentialRef, ProjectId, SessionId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const LIST_MAX: usize = 200;
pub const LABEL_BYTES_MAX: usize = 128;
pub const PROMPT_BYTES_MAX: usize = 32 * 1024;
pub const LAST_TEXT_BYTES_MAX: usize = 1024;
const AGENTS: &str = "agents";
const POLL: Duration = Duration::from_millis(50);
#[cfg(unix)]
const MONITOR_POLL: Duration = Duration::from_millis(250);
const REPLY_WAIT: Duration = Duration::from_secs(40);
#[cfg(unix)]
const KILL_WAIT: Duration = Duration::from_secs(2);
#[cfg(target_os = "macos")]
const LSOF: &str = "/usr/sbin/lsof";
const STOP_INCOMPLETE: &str = "agent_stop_incomplete";
const STOP_GRACE: Duration = Duration::from_secs(2);
const RUN_TIME_SECS_MAX: u64 = 86_400;
const HOST_WAIT: Duration = Duration::from_secs(10);
const RESTARTS_MAX: usize = LIST_MAX;
#[cfg(unix)]
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS agent_runs(
    id TEXT PRIMARY KEY CHECK(length(id) = 32),
    project TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('master','sub_agent','research')),
    label TEXT NOT NULL,
    harness TEXT NOT NULL,
    spec TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('running','stopped','exited','failed','closed')),
    reattached INTEGER NOT NULL CHECK(reattached IN (0,1)),
    process_group INTEGER CHECK(process_group > 1),
    attempt INTEGER NOT NULL CHECK(attempt >= 1),
    epoch TEXT NOT NULL,
    exit_code INTEGER,
    reason TEXT,
    last_text TEXT,
    started_ms INTEGER NOT NULL,
    ended_ms INTEGER
);
CREATE INDEX IF NOT EXISTS agent_runs_project ON agent_runs(project, started_ms);";
const COLUMNS: &str = "id,project,role,label,harness,state,reattached,attempt,exit_code,reason,last_text,started_ms,ended_ms,substr(spec,1,8)='{\"host\":'";

fn failure(code: &str) -> LaunchError {
    LaunchError(code.into())
}

fn database<E>(_: E) -> LaunchError {
    failure("agent_registry_unavailable")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

fn new_id() -> Result<String, LaunchError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| failure("agent_identity_unavailable"))?;
    let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(id.len(), 32);
    Ok(id)
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn clip(text: &str, bytes: usize) -> String {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// Which part of a project's team an agent plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Master,
    SubAgent,
    Research,
}

impl AgentRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Master => "master",
            Self::SubAgent => "sub_agent",
            Self::Research => "research",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "master" => Some(Self::Master),
            "sub_agent" => Some(Self::SubAgent),
            "research" => Some(Self::Research),
            _ => None,
        }
    }
}

/// Where an agent is in its life. `Closed` means it was running when CrazyAgent closed
/// and no live process of it was found afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Running,
    Stopped,
    Exited,
    Failed,
    Closed,
}

impl AgentState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Exited => "exited",
            Self::Failed => "failed",
            Self::Closed => "closed",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "running" => Some(Self::Running),
            "stopped" => Some(Self::Stopped),
            "exited" => Some(Self::Exited),
            "failed" => Some(Self::Failed),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

/// How a restart reaches the agent's account again. A keyed account's key is kept only in
/// memory by the engine that launched it, so after CrazyAgent restarts it must be launched
/// again from its project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentAccount {
    Subscription,
    Endpoint {
        base_url: Option<String>,
        keyed: bool,
    },
}

/// Everything needed to launch, and later restart, one agent, without any secret.
/// `context` gives the agent the project's approved skills, MCP servers, brief, context
/// index and approved memories; `prompt` is its first task, sent again on restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    pub project: ProjectId,
    pub role: AgentRole,
    pub label: String,
    pub harness: String,
    pub executable: PathBuf,
    pub working_dir: PathBuf,
    pub home: PathBuf,
    pub account: AgentAccount,
    pub model: Option<String>,
    pub tools: bool,
    pub sandbox_profile: Option<PathBuf>,
    pub run_time_secs: u64,
    pub context: bool,
    pub prompt: Option<String>,
}

impl AgentSpec {
    fn validate(&self, key: Option<&Secret>) -> Result<(), LaunchError> {
        let printable = |text: &str| !text.chars().any(char::is_control);
        if self.label.trim().is_empty()
            || self.label.len() > LABEL_BYTES_MAX
            || !printable(&self.label)
        {
            return Err(failure("agent_label_invalid"));
        }
        if self
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.trim().is_empty() || prompt.len() > PROMPT_BYTES_MAX)
        {
            return Err(failure("agent_prompt_invalid"));
        }
        if !(1..=RUN_TIME_SECS_MAX).contains(&self.run_time_secs) {
            return Err(failure("agent_run_time_invalid"));
        }
        if self.sandbox_profile.is_none() {
            return Err(failure("launch_sandbox_required"));
        }
        let keyed = matches!(self.account, AgentAccount::Endpoint { keyed: true, .. });
        if keyed != key.is_some()
            || matches!(
                self.account,
                AgentAccount::Endpoint {
                    base_url: None,
                    keyed: false
                }
            )
        {
            return Err(failure("agent_account_invalid"));
        }
        Ok(())
    }

    fn request(&self, key: Option<Secret>) -> Result<LaunchRequest, LaunchError> {
        let home = self.home.clone();
        let account = match (&self.account, key) {
            (AgentAccount::Subscription, None) => AccountBinding::Subscription { home },
            (
                AgentAccount::Endpoint {
                    base_url: Some(base_url),
                    ..
                },
                key,
            ) => AccountBinding::Url {
                home,
                base_url: base_url.clone(),
                key,
            },
            (AgentAccount::Endpoint { base_url: None, .. }, Some(key)) => AccountBinding::ApiKey {
                home,
                key,
                base_url: None,
            },
            _ => return Err(failure("agent_account_invalid")),
        };
        Ok(LaunchRequest {
            harness: self.harness.clone(),
            executable: self.executable.clone(),
            working_dir: self.working_dir.clone(),
            account,
            model: self.model.clone(),
            tools: self.tools,
            sandbox_profile: self.sandbox_profile.clone(),
            limits: LaunchLimits {
                run_time: Duration::from_secs(self.run_time_secs),
                stop_grace: STOP_GRACE,
                ..LaunchLimits::default()
            },
        })
    }
}

/// What the owner sees and can do for one agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentView {
    pub id: String,
    pub project: ProjectId,
    pub role: AgentRole,
    pub label: String,
    pub harness: String,
    pub state: AgentState,
    pub status: String,
    pub reattached: bool,
    pub busy: bool,
    pub attempt: u32,
    pub can_stop: bool,
    pub can_interrupt: bool,
    pub can_restart: bool,
    pub exit_code: Option<i32>,
    pub reason: Option<String>,
    pub last_text: Option<String>,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
}

/// Which engine part drives a hosted agent, and that part's own reference for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostedSpec {
    host: String,
    reference: String,
}

/// What the register shows for one agent that another engine part starts and drives,
/// such as a team agent: its project, role, owner-facing label and harness, the host's
/// name, the host's own reference for it, handed back with every owner restart, and the
/// account it signs in with, so a refused sign-out can name and stop it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedAgent {
    pub project: ProjectId,
    pub role: AgentRole,
    pub label: String,
    pub harness: String,
    pub host: &'static str,
    pub reference: String,
    pub account: Option<CredentialRef>,
}

/// What the owner asked of a hosted agent. A restart also stops it; once it ends, the
/// restart is handed to its host through `Agents::take_restarts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedRequest {
    Stop,
    Restart,
}

/// One restart the owner asked for a hosted agent that is no longer running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedRestart {
    pub id: String,
    pub host: String,
    pub reference: String,
}

/// Something other than a hosted agent that holds an account's native profile while it
/// runs, so a refused sign-out can name it and the owner can end it: a master
/// conversation's turn, ended by closing that conversation, or a research job, ended by
/// cancelling it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Holder {
    Master {
        project: ProjectId,
        session: SessionId,
    },
    Research {
        project: ProjectId,
        job: i64,
        title: String,
    },
}

/// What holds one account's native profile now: its running hosted agents and its other
/// holders, so a refused sign-out can name them and the owner can end them all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountUse {
    pub agents: Vec<AgentView>,
    pub holders: Vec<Holder>,
}

impl AccountUse {
    /// Whether nothing holds the account, so it can be signed out. Input: the use.
    /// Output: true when no agent and no other holder is listed.
    pub fn is_free(&self) -> bool {
        self.agents.is_empty() && self.holders.is_empty()
    }
}

type Holds = Arc<Mutex<BTreeMap<u64, (CredentialRef, Holder)>>>;

/// One holder's place in the register, kept beside the profile lease it names. Dropping
/// it, on every path that ends the lease, removes the holder.
pub struct Hold {
    holds: Holds,
    key: u64,
}

impl Drop for Hold {
    /// Removes this holder from the register. Input: the hold being dropped. Output: none.
    fn drop(&mut self) {
        let mut holds = self.holds.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = holds.remove(&self.key);
    }
}

#[derive(Default)]
struct Control {
    stop: AtomicBool,
    restart: AtomicBool,
    busy: AtomicBool,
    interrupt: Arc<AtomicBool>,
    account: Option<CredentialRef>,
}

/// A hosted agent's place in the register, kept by the engine part that drives it.
/// The host reads the owner's stop and restart with `requested`, gives `interrupt_flag`
/// to the thread that drives the harness and records the end with `finish`. Dropping it
/// records the agent as "Stopped when CrazyAgent closed".
pub struct Hosted {
    id: String,
    control: Arc<Control>,
    recorder: Recorder,
    agents: Arc<Agents>,
    ended: bool,
}

impl Hosted {
    /// Returns the agent's register id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Reads what the owner asked of the agent. Output: a restart, a stop, or none.
    pub fn requested(&self) -> Option<HostedRequest> {
        if self.control.restart.load(Ordering::Acquire) {
            Some(HostedRequest::Restart)
        } else if self.control.stop.load(Ordering::Acquire) {
            Some(HostedRequest::Stop)
        } else {
            None
        }
    }

    /// Returns the flag the owner's interrupt sets. The thread that drives the harness
    /// clears it before each turn and interrupts the turn once it is set.
    pub fn interrupt_flag(&self) -> Arc<AtomicBool> {
        self.control.interrupt.clone()
    }

    /// Records whether the agent is working on a turn; only then can the owner interrupt it.
    pub fn set_busy(&self, busy: bool) {
        self.control.busy.store(busy, Ordering::Release);
    }

    /// Records the process group of the agent's current harness process. Input: the group.
    pub fn started(&self, group: u32) {
        assert!(group > 1);
        if let Ok(db) = self.recorder.db.lock() {
            let _ = db.execute(
                "UPDATE agent_runs SET process_group=?4 WHERE id=?1 AND epoch=?2 AND attempt=?3 AND state='running'",
                params![
                    self.id,
                    self.recorder.epoch,
                    self.recorder.attempt,
                    i64::from(group)
                ],
            );
        }
    }

    /// Records the agent's last reply, clipped to `LAST_TEXT_BYTES_MAX` bytes.
    pub fn text(&self, text: &str) {
        self.recorder.text(text);
    }

    /// Records how the agent ended and hands an owner restart to its host.
    /// Input: `Stopped`, `Exited` or `Failed`, and a plain reason.
    pub fn finish(mut self, state: AgentState, reason: Option<&str>) {
        assert!(state != AgentState::Running && state != AgentState::Closed);
        self.end(state, reason);
    }

    fn end(&mut self, state: AgentState, reason: Option<&str>) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.recorder.finish(state, None, reason);
        if let Ok(mut live) = self.agents.live.lock()
            && matches!(live.get(&self.id), Some(Live::Hosted { control }) if Arc::ptr_eq(control, &self.control))
        {
            live.remove(&self.id);
        }
        if self.control.restart.load(Ordering::Acquire) {
            let _ = self.agents.queue_restart(&self.id);
        }
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        self.end(AgentState::Closed, None);
    }
}

enum Command {
    Send(String, SyncSender<Result<(), LaunchError>>),
    Interrupt(SyncSender<Result<(), LaunchError>>),
    Stop(AgentState, SyncSender<Result<(), LaunchError>>),
}

enum Live {
    Owned {
        commands: SyncSender<Command>,
        thread: Option<JoinHandle<()>>,
        busy: Arc<AtomicBool>,
    },
    #[cfg(unix)]
    Reattached {
        group: u32,
        output: Arc<PathBuf>,
        probe: Arc<File>,
        quit: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    },
    Hosted {
        control: Arc<Control>,
    },
}

#[derive(Clone)]
struct Recorder {
    db: Arc<Mutex<Connection>>,
    id: String,
    epoch: String,
    attempt: u32,
}

impl Recorder {
    fn finish(&self, state: AgentState, exit_code: Option<i32>, reason: Option<&str>) {
        assert!(state != AgentState::Running);
        if let Ok(db) = self.db.lock() {
            let _ = db.execute(
                "UPDATE agent_runs SET state=?4,exit_code=?5,reason=?6,ended_ms=?7
                 WHERE id=?1 AND epoch=?2 AND attempt=?3 AND state='running'",
                params![
                    self.id,
                    self.epoch,
                    self.attempt,
                    state.as_str(),
                    exit_code,
                    reason,
                    now_ms() as i64
                ],
            );
        }
    }

    /// Records how a stopped agent ended: the given state and reason once no process
    /// holding its output log is left, otherwise Failed with `agent_stop_incomplete`.
    /// Inputs: the state, exit code and reason, and whether the stop completed.
    fn ended(
        &self,
        state: AgentState,
        exit_code: Option<i32>,
        reason: Option<&str>,
        complete: bool,
    ) {
        if complete {
            self.finish(state, exit_code, reason);
        } else {
            self.finish(AgentState::Failed, exit_code, Some(STOP_INCOMPLETE));
        }
    }

    fn text(&self, text: &str) {
        if let Ok(db) = self.db.lock() {
            let _ = db.execute(
                "UPDATE agent_runs SET last_text=?4 WHERE id=?1 AND epoch=?2 AND attempt=?3",
                params![
                    self.id,
                    self.epoch,
                    self.attempt,
                    clip(text, LAST_TEXT_BYTES_MAX)
                ],
            );
        }
    }
}

/// The engine's register of every agent it launched, with one control channel per live
/// agent. It survives an engine restart through `engine.db`: opening it reattaches each
/// agent whose process group still holds its output lock and marks the rest
/// "Stopped when CrazyAgent closed". Dropping it stops every agent it controls.
pub struct Agents {
    root: PathBuf,
    epoch: String,
    db: Arc<Mutex<Connection>>,
    live: Mutex<HashMap<String, Live>>,
    keys: Mutex<HashMap<String, Secret>>,
    restarts: Mutex<Vec<HostedRestart>>,
    hosts: Mutex<Vec<String>>,
    holds: Holds,
    next_hold: AtomicU64,
}

impl Agents {
    /// Opens the register for one engine and recovers what was running before.
    /// Input: the engine root holding `engine.db`, an absolute 0700 directory.
    /// Output: the register, or a plain code when the root or database is unusable.
    #[cfg(unix)]
    pub fn open(root: &Path) -> Result<Self, LaunchError> {
        let metadata = std::fs::symlink_metadata(root).map_err(|_| failure("agent_root_unsafe"))?;
        if !root.is_absolute()
            || !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(failure("agent_root_unsafe"));
        }
        let connection = Connection::open(root.join("engine.db")).map_err(database)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(database)?;
        connection.execute_batch(SCHEMA).map_err(database)?;
        let folder = root.join(AGENTS);
        match std::fs::DirBuilder::new().mode(0o700).create(&folder) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(failure("agent_root_unsafe")),
        }
        let folder_metadata =
            std::fs::symlink_metadata(&folder).map_err(|_| failure("agent_root_unsafe"))?;
        if !folder_metadata.is_dir() || folder_metadata.mode() & 0o7777 != 0o700 {
            return Err(failure("agent_root_unsafe"));
        }
        let agents = Self {
            root: root.to_path_buf(),
            epoch: new_id()?,
            db: Arc::new(Mutex::new(connection)),
            live: Mutex::new(HashMap::new()),
            keys: Mutex::new(HashMap::new()),
            restarts: Mutex::new(Vec::new()),
            hosts: Mutex::new(Vec::new()),
            holds: Arc::default(),
            next_hold: AtomicU64::new(0),
        };
        agents.recover()?;
        Ok(agents)
    }

    /// Refuses the agent register where the Unix owner and 0700 checks on its root and
    /// agent folders do not exist. Takes the engine root, unused.
    /// Returns `agent_root_unavailable`.
    #[cfg(not(unix))]
    pub fn open(_root: &Path) -> Result<Self, LaunchError> {
        Err(failure("agent_root_unavailable"))
    }

    fn db(&self) -> Result<MutexGuard<'_, Connection>, LaunchError> {
        self.db
            .lock()
            .map_err(|_| failure("agent_registry_unavailable"))
    }

    fn live(&self) -> Result<MutexGuard<'_, HashMap<String, Live>>, LaunchError> {
        self.live
            .lock()
            .map_err(|_| failure("agent_registry_unavailable"))
    }

    fn folder(&self, id: &str) -> PathBuf {
        assert!(valid_id(id));
        self.root.join(AGENTS).join(id)
    }

    #[cfg(unix)]
    fn recover(&self) -> Result<(), LaunchError> {
        let running: Vec<(String, Option<i64>)> = {
            let db = self.db()?;
            let mut statement = db
                .prepare("SELECT id,process_group FROM agent_runs WHERE state='running'")
                .map_err(database)?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(database)?
                .collect::<Result<_, _>>()
                .map_err(database)?
        };
        for (id, group) in running {
            let alive = valid_id(&id)
                .then(|| self.probe(&id))
                .flatten()
                .zip(group.and_then(|group| u32::try_from(group).ok()))
                .filter(|(_, group)| *group > 1);
            let db = self.db()?;
            match alive {
                Some((probe, group)) => {
                    let claimed = db
                        .execute(
                            "UPDATE agent_runs SET epoch=?2,reattached=1 WHERE id=?1 AND state='running'",
                            params![id, self.epoch],
                        )
                        .map_err(database)?;
                    let attempt: u32 = db
                        .query_row("SELECT attempt FROM agent_runs WHERE id=?1", [&id], |row| {
                            row.get(0)
                        })
                        .map_err(database)?;
                    drop(db);
                    if claimed == 1 {
                        self.watch(id, group, probe, attempt)?;
                    }
                }
                None => {
                    db.execute(
                        "UPDATE agent_runs SET state='closed',ended_ms=?2 WHERE id=?1 AND state='running'",
                        params![id, now_ms() as i64],
                    )
                    .map_err(database)?;
                }
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    fn probe(&self, id: &str) -> Option<File> {
        log_probe(&self.folder(id).join(launch::DURABLE_OUTPUT)).filter(launch::holder_alive)
    }

    #[cfg(unix)]
    fn watch(&self, id: String, group: u32, probe: File, attempt: u32) -> Result<(), LaunchError> {
        let output = Arc::new(self.folder(&id).join(launch::DURABLE_OUTPUT));
        let probe = Arc::new(probe);
        let quit = Arc::new(AtomicBool::new(false));
        let recorder = Recorder {
            db: self.db.clone(),
            id: id.clone(),
            epoch: self.epoch.clone(),
            attempt,
        };
        let limit = LaunchLimits::default().output_bytes as u64;
        let thread = {
            let (output, probe, quit) = (output.clone(), probe.clone(), quit.clone());
            std::thread::Builder::new()
                .name("agent-reattached".into())
                .spawn(move || {
                    while !quit.load(Ordering::Acquire) {
                        if !launch::holder_alive(&probe) {
                            recorder.finish(AgentState::Exited, None, None);
                            return;
                        }
                        if probe
                            .metadata()
                            .is_ok_and(|metadata| metadata.len() > limit)
                        {
                            recorder.ended(
                                AgentState::Failed,
                                None,
                                Some("launch_output_limit"),
                                kill_holders(group, &output, &probe),
                            );
                            return;
                        }
                        std::thread::sleep(MONITOR_POLL);
                    }
                })
                .map_err(|_| failure("agent_thread_unavailable"))?
        };
        self.live()?.insert(
            id,
            Live::Reattached {
                group,
                output,
                probe,
                quit,
                thread: Some(thread),
            },
        );
        Ok(())
    }

    /// Launches one agent that keeps running if the engine dies and records it.
    /// Input: the agent's spec and, for a keyed account, its key, kept only in memory for
    /// restarts. Output: the agent as the owner sees it, or a plain refusal code.
    pub fn launch(&self, spec: AgentSpec, key: Option<Secret>) -> Result<AgentView, LaunchError> {
        spec.validate(key.as_ref())?;
        let id = new_id()?;
        let encoded = serde_json::to_string(&spec).map_err(|_| failure("agent_spec_invalid"))?;
        self.db()?
            .execute(
                "INSERT INTO agent_runs(id,project,role,label,harness,spec,state,reattached,attempt,epoch,started_ms)
                 VALUES(?1,?2,?3,?4,?5,?6,'running',0,1,?7,?8)",
                params![
                    id,
                    spec.project.as_str(),
                    spec.role.as_str(),
                    spec.label,
                    spec.harness,
                    encoded,
                    self.epoch,
                    now_ms() as i64
                ],
            )
            .map_err(database)?;
        if let Some(key) = &key {
            self.keys
                .lock()
                .map_err(|_| failure("agent_registry_unavailable"))?
                .insert(id.clone(), key.duplicate());
        }
        self.start(&id, &spec, key, 1)?;
        self.view(&id)
    }

    fn start(
        &self,
        id: &str,
        spec: &AgentSpec,
        key: Option<Secret>,
        attempt: u32,
    ) -> Result<(), LaunchError> {
        let recorder = Recorder {
            db: self.db.clone(),
            id: id.to_owned(),
            epoch: self.epoch.clone(),
            attempt,
        };
        let started = self.spawn(id, spec, key);
        let mut session = match started {
            Ok(session) => session,
            Err(error) => {
                recorder.finish(AgentState::Failed, None, Some(&error.0));
                return Err(error);
            }
        };
        let busy = Arc::new(AtomicBool::new(false));
        let group = session.process_group();
        let prepared = match &spec.prompt {
            Some(prompt) => session.send(prompt),
            None => Ok(()),
        }
        .and_then(|()| {
            self.db()?
                .execute(
                    "UPDATE agent_runs SET process_group=?4 WHERE id=?1 AND epoch=?2 AND attempt=?3",
                    params![id, self.epoch, attempt, i64::from(group)],
                )
                .map_err(database)
        });
        let output = self
            .folder(id)
            .join(crate::harnesses::launch::DURABLE_OUTPUT);
        if let Err(error) = prepared {
            let (_, complete) = end_session(session, &output);
            recorder.ended(AgentState::Failed, None, Some(&error.0), complete);
            return Err(error);
        }
        busy.store(spec.prompt.is_some(), Ordering::Release);
        let (commands, inbox) = sync_channel(4);
        let failed = recorder.clone();
        let thread = {
            let busy = busy.clone();
            std::thread::Builder::new()
                .name("agent".into())
                .spawn(move || drive(session, &output, inbox, busy, recorder))
                .map_err(|_| {
                    failed.finish(AgentState::Failed, None, Some("agent_thread_unavailable"));
                    failure("agent_thread_unavailable")
                })?
        };
        self.live()?.insert(
            id.to_owned(),
            Live::Owned {
                commands,
                thread: Some(thread),
                busy,
            },
        );
        Ok(())
    }

    fn spawn(
        &self,
        id: &str,
        spec: &AgentSpec,
        key: Option<Secret>,
    ) -> Result<Session, LaunchError> {
        let folder = self.folder(id);
        if folder.exists() {
            std::fs::remove_dir_all(&folder).map_err(|_| failure("agent_state_unavailable"))?;
        }
        #[cfg(unix)]
        let created = std::fs::DirBuilder::new().mode(0o700).create(&folder);
        #[cfg(not(unix))]
        let created = std::fs::DirBuilder::new().create(&folder);
        created.map_err(|_| failure("agent_state_unavailable"))?;
        let request = spec.request(key)?;
        let context = if spec.context {
            Some(crate::launch_context::for_project(
                &*self.db()?,
                &spec.project,
            )?)
        } else {
            None
        };
        Session::start_durable(request, context, &folder)
    }

    /// Lists agents, newest first, for one project or for every project.
    /// Inputs: the project filter, how many agents to skip and at most how many to
    /// return, up to `LIST_MAX`. Output: the agents with their controls.
    pub fn list(
        &self,
        project: Option<&ProjectId>,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<AgentView>, LaunchError> {
        let limit = limit.min(LIST_MAX);
        let rows: Vec<Stored> = {
            let db = self.db()?;
            let sql = format!(
                "SELECT {COLUMNS} FROM agent_runs WHERE ?1 IS NULL OR project=?1 ORDER BY started_ms DESC, id LIMIT ?2 OFFSET ?3"
            );
            let mut statement = db.prepare(&sql).map_err(database)?;
            let offset = i64::try_from(offset).map_err(|_| failure("agents_request_bounds"))?;
            let bound = i64::try_from(limit).map_err(|_| failure("agents_request_bounds"))?;
            statement
                .query_map(
                    params![project.map(ProjectId::as_str), bound, offset],
                    stored,
                )
                .map_err(database)?
                .collect::<Result<_, _>>()
                .map_err(database)?
        };
        let served = self.served();
        let live = self.live()?;
        let views: Vec<AgentView> = rows
            .into_iter()
            .map(|row| {
                let entry = live.get(&row.id);
                row.view(entry, served)
            })
            .collect();
        assert!(views.len() <= limit);
        Ok(views)
    }

    /// Reads one agent. Input: its id. Output: the agent, or `agent_not_found`.
    pub fn view(&self, id: &str) -> Result<AgentView, LaunchError> {
        if !valid_id(id) {
            return Err(failure("agent_not_found"));
        }
        let row = self
            .db()?
            .query_row(
                &format!("SELECT {COLUMNS} FROM agent_runs WHERE id=?1"),
                [id],
                stored,
            )
            .optional()
            .map_err(database)?
            .ok_or_else(|| failure("agent_not_found"))?;
        let served = self.served();
        let live = self.live()?;
        Ok(row.view(live.get(id), served))
    }

    /// Sends one more prompt to a live agent. Input: id and text. Output: Ok, or
    /// `agent_busy`, `agent_not_running` or `agent_reconnected_read_only`.
    pub fn send(&self, id: &str, text: &str) -> Result<(), LaunchError> {
        if text.trim().is_empty() || text.len() > PROMPT_BYTES_MAX {
            return Err(failure("agent_prompt_invalid"));
        }
        let (reply, answer) = sync_channel(1);
        self.command(id, Command::Send(text.to_owned(), reply))?;
        wait(answer).map_err(|error| match error.0.as_str() {
            "launch_busy" => failure("agent_busy"),
            _ => error,
        })
    }

    /// Interrupts the current turn of one agent through its harness protocol; the
    /// process keeps running. Input: id. Output: Ok, or `agent_not_busy`.
    pub fn interrupt(&self, id: &str) -> Result<(), LaunchError> {
        if let Some(control) = self.hosted(id)? {
            if !control.busy.load(Ordering::Acquire) {
                return Err(failure("agent_not_busy"));
            }
            control.interrupt.store(true, Ordering::Release);
            return Ok(());
        }
        let (reply, answer) = sync_channel(1);
        self.command(id, Command::Interrupt(reply))?;
        wait(answer).map_err(|error| match error.0.as_str() {
            "launch_not_running" => failure("agent_not_busy"),
            _ => error,
        })
    }

    fn command(&self, id: &str, command: Command) -> Result<(), LaunchError> {
        if !valid_id(id) {
            return Err(failure("agent_not_found"));
        }
        match self.live()?.get(id) {
            Some(Live::Owned { commands, .. }) => {
                commands.try_send(command).map_err(|error| match error {
                    TrySendError::Full(_) => failure("agent_busy"),
                    TrySendError::Disconnected(_) => failure("agent_not_running"),
                })
            }
            #[cfg(unix)]
            Some(Live::Reattached { .. }) => Err(failure("agent_reconnected_read_only")),
            Some(Live::Hosted { .. }) => Err(failure("agent_hosted")),
            None => Err(failure("agent_not_running")),
        }
    }

    /// Stops one agent: its whole process group gets TERM, then KILL after the grace
    /// time, and whatever still holds its output log, such as a child that left the
    /// group, is ended too; the agent is recorded Failed when that cannot be done.
    /// Other agents are untouched. Input: id. Output: the stopped agent.
    pub fn stop(&self, id: &str) -> Result<AgentView, LaunchError> {
        if let Some(control) = self.hosted(id)? {
            control.stop.store(true, Ordering::Release);
            self.await_host(id, &control, Instant::now() + HOST_WAIT);
            return self.view(id);
        }
        let live = self.take(id)?.ok_or_else(|| failure("agent_not_running"))?;
        end(live, AgentState::Stopped, self.recorder(id)?);
        self.view(id)
    }

    /// Lists the running hosted agents that sign in with one account, newest first, so a
    /// refused sign-out can name them. Input: the account. Output: the agents.
    pub fn using(&self, account: &CredentialRef) -> Result<Vec<AgentView>, LaunchError> {
        let mut views = self
            .hosted_using(account)?
            .iter()
            .map(|(id, _)| self.view(id))
            .collect::<Result<Vec<_>, _>>()?;
        views.sort_by(|a, b| {
            b.started_ms
                .cmp(&a.started_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        views.truncate(LIST_MAX);
        Ok(views)
    }

    /// Stops every running hosted agent that signs in with one account and waits, at most
    /// `HOST_WAIT` in all, for their hosts to end them. Input: the account. Output: those
    /// agents as they are after the wait.
    pub fn stop_using(&self, account: &CredentialRef) -> Result<Vec<AgentView>, LaunchError> {
        let hosted = self.hosted_using(account)?;
        for (_, control) in &hosted {
            control.stop.store(true, Ordering::Release);
        }
        let deadline = Instant::now() + HOST_WAIT;
        for (id, control) in &hosted {
            self.await_host(id, control, deadline);
        }
        hosted.iter().map(|(id, _)| self.view(id)).collect()
    }

    /// Registers one holder of an account's profile until the returned hold is dropped.
    /// Inputs: the account and the holder. Output: the hold to keep beside the lease.
    pub fn hold(&self, account: CredentialRef, holder: Holder) -> Hold {
        let key = self.next_hold.fetch_add(1, Ordering::Relaxed);
        let mut holds = self.holds.lock().unwrap_or_else(PoisonError::into_inner);
        holds.insert(key, (account, holder));
        Hold {
            holds: self.holds.clone(),
            key,
        }
    }

    /// The holders of one account's profile other than hosted agents, newest first, at
    /// most `LIST_MAX`. Input: the account. Output: the holders.
    pub fn holders(&self, account: &CredentialRef) -> Vec<Holder> {
        let holds = self.holds.lock().unwrap_or_else(PoisonError::into_inner);
        holds
            .values()
            .rev()
            .filter(|(held, _)| held == account)
            .map(|(_, holder)| holder.clone())
            .take(LIST_MAX)
            .collect()
    }

    fn hosted_using(
        &self,
        account: &CredentialRef,
    ) -> Result<Vec<(String, Arc<Control>)>, LaunchError> {
        Ok(self
            .live()?
            .iter()
            .filter_map(|(id, live)| match live {
                Live::Hosted { control } if control.account.as_ref() == Some(account) => {
                    Some((id.clone(), control.clone()))
                }
                _ => None,
            })
            .collect())
    }

    /// Restarts one agent from its spec with the same id: a live agent is stopped first,
    /// a finished, failed or closed one is launched again. Input: id. Output: the running
    /// agent, or `agent_restart_needs_key` when a keyed account's key is no longer held.
    pub fn restart(&self, id: &str) -> Result<AgentView, LaunchError> {
        if !valid_id(id) {
            return Err(failure("agent_not_found"));
        }
        let (encoded, attempt): (String, u32) = self
            .db()?
            .query_row(
                "SELECT spec,attempt FROM agent_runs WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(database)?
            .ok_or_else(|| failure("agent_not_found"))?;
        if let Ok(hosted) = serde_json::from_str::<HostedSpec>(&encoded) {
            return self.restart_hosted(id, &hosted);
        }
        let spec: AgentSpec =
            serde_json::from_str(&encoded).map_err(|_| failure("agent_spec_invalid"))?;
        let key = self
            .keys
            .lock()
            .map_err(|_| failure("agent_registry_unavailable"))?
            .get(id)
            .map(Secret::duplicate);
        if matches!(spec.account, AgentAccount::Endpoint { keyed: true, .. }) && key.is_none() {
            return Err(failure("agent_restart_needs_key"));
        }
        if let Some(live) = self.take(id)? {
            end(live, AgentState::Stopped, self.recorder(id)?);
        }
        let next = attempt
            .checked_add(1)
            .ok_or_else(|| failure("agent_restart_limit"))?;
        let changed = self
            .db()?
            .execute(
                "UPDATE agent_runs SET state='running',reattached=0,process_group=NULL,attempt=?2,epoch=?3,
                 exit_code=NULL,reason=NULL,last_text=NULL,started_ms=?4,ended_ms=NULL
                 WHERE id=?1 AND state<>'running'",
                params![id, next, self.epoch, now_ms() as i64],
            )
            .map_err(database)?;
        if changed != 1 {
            return Err(failure("agent_restart_conflict"));
        }
        self.start(id, &spec, key, next)?;
        self.view(id)
    }

    /// Records one agent that `agent.host` starts and drives itself, so the owner sees and
    /// controls it with every other agent. Inputs: what to show, and the id of an earlier
    /// run of the same host and reference to continue as its next attempt. Output: the
    /// handle the host keeps while the agent lives.
    pub fn host(
        self: &Arc<Self>,
        agent: HostedAgent,
        again: Option<&str>,
    ) -> Result<Hosted, LaunchError> {
        if agent.label.trim().is_empty()
            || agent.label.len() > LABEL_BYTES_MAX
            || agent.label.chars().any(char::is_control)
        {
            return Err(failure("agent_label_invalid"));
        }
        let spec = serde_json::to_string(&HostedSpec {
            host: agent.host.to_owned(),
            reference: agent.reference.clone(),
        })
        .map_err(|_| failure("agent_spec_invalid"))?;
        let now = now_ms() as i64;
        let db = self.db()?;
        let mut run = None;
        if let Some(id) = again.filter(|id| valid_id(id)) {
            let attempt: Option<u32> = db
                .query_row(
                    "SELECT attempt FROM agent_runs WHERE id=?1 AND spec=?2 AND state<>'running'",
                    params![id, spec],
                    |row| row.get(0),
                )
                .optional()
                .map_err(database)?;
            if let Some(next) = attempt.and_then(|attempt| attempt.checked_add(1)) {
                let changed = db
                    .execute(
                        "UPDATE agent_runs SET state='running',reattached=0,process_group=NULL,attempt=?3,epoch=?4,
                         exit_code=NULL,reason=NULL,last_text=NULL,started_ms=?5,ended_ms=NULL
                         WHERE id=?1 AND spec=?2 AND state<>'running'",
                        params![id, spec, next, self.epoch, now],
                    )
                    .map_err(database)?;
                if changed == 1 {
                    run = Some((id.to_owned(), next));
                }
            }
        }
        let (id, attempt) = match run {
            Some(run) => run,
            None => {
                let id = new_id()?;
                db.execute(
                    "INSERT INTO agent_runs(id,project,role,label,harness,spec,state,reattached,attempt,epoch,started_ms)
                     VALUES(?1,?2,?3,?4,?5,?6,'running',0,1,?7,?8)",
                    params![
                        id,
                        agent.project.as_str(),
                        agent.role.as_str(),
                        agent.label,
                        agent.harness,
                        spec,
                        self.epoch,
                        now
                    ],
                )
                .map_err(database)?;
                (id, 1)
            }
        };
        drop(db);
        let control = Arc::new(Control {
            account: agent.account.clone(),
            ..Control::default()
        });
        self.live()?.insert(
            id.clone(),
            Live::Hosted {
                control: control.clone(),
            },
        );
        Ok(Hosted {
            recorder: Recorder {
                db: self.db.clone(),
                id: id.clone(),
                epoch: self.epoch.clone(),
                attempt,
            },
            id,
            control,
            agents: self.clone(),
            ended: false,
        })
    }

    /// Hands one host the restarts the owner asked for its agents that no longer run, and
    /// from then on accepts such restarts for it. Input: the host name. Output: each
    /// restart with the agent's register id and the host's own reference.
    pub fn take_restarts(&self, host: &str) -> Vec<HostedRestart> {
        if let Ok(mut hosts) = self.hosts.lock()
            && !hosts.iter().any(|known| known == host)
        {
            hosts.push(host.to_owned());
        }
        let Ok(mut restarts) = self.restarts.lock() else {
            return Vec::new();
        };
        let (taken, kept): (Vec<HostedRestart>, Vec<HostedRestart>) =
            std::mem::take(&mut *restarts)
                .into_iter()
                .partition(|restart| restart.host == host);
        *restarts = kept;
        assert!(restarts.len() <= RESTARTS_MAX);
        taken
    }

    fn served(&self) -> bool {
        self.hosts.lock().is_ok_and(|hosts| !hosts.is_empty())
    }

    fn queue_restart(&self, id: &str) -> Result<(), LaunchError> {
        let encoded: String = self
            .db()?
            .query_row("SELECT spec FROM agent_runs WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .map_err(database)?;
        let spec: HostedSpec =
            serde_json::from_str(&encoded).map_err(|_| failure("agent_spec_invalid"))?;
        let mut restarts = self
            .restarts
            .lock()
            .map_err(|_| failure("agent_registry_unavailable"))?;
        if restarts.iter().any(|restart| restart.id == id) {
            return Ok(());
        }
        if restarts.len() >= RESTARTS_MAX {
            return Err(failure("agent_restart_limit"));
        }
        restarts.push(HostedRestart {
            id: id.to_owned(),
            host: spec.host,
            reference: spec.reference,
        });
        Ok(())
    }

    fn restart_hosted(&self, id: &str, spec: &HostedSpec) -> Result<AgentView, LaunchError> {
        if let Some(control) = self.hosted(id)? {
            control.restart.store(true, Ordering::Release);
            control.stop.store(true, Ordering::Release);
            self.await_host(id, &control, Instant::now() + HOST_WAIT);
            return self.view(id);
        }
        let served = self
            .hosts
            .lock()
            .map_err(|_| failure("agent_registry_unavailable"))?
            .contains(&spec.host);
        if !served {
            return Err(failure("agent_restart_unavailable"));
        }
        self.queue_restart(id)?;
        self.view(id)
    }

    fn hosted(&self, id: &str) -> Result<Option<Arc<Control>>, LaunchError> {
        if !valid_id(id) {
            return Err(failure("agent_not_found"));
        }
        Ok(match self.live()?.get(id) {
            Some(Live::Hosted { control }) => Some(control.clone()),
            _ => None,
        })
    }

    fn await_host(&self, id: &str, control: &Arc<Control>, deadline: Instant) {
        while Instant::now() < deadline {
            let held = match self.live() {
                Ok(live) => {
                    matches!(live.get(id), Some(Live::Hosted { control: current }) if Arc::ptr_eq(current, control))
                }
                Err(_) => return,
            };
            if !held {
                return;
            }
            std::thread::sleep(POLL);
        }
    }

    fn take(&self, id: &str) -> Result<Option<Live>, LaunchError> {
        if !valid_id(id) {
            return Err(failure("agent_not_found"));
        }
        Ok(self.live()?.remove(id))
    }

    fn recorder(&self, id: &str) -> Result<Recorder, LaunchError> {
        let attempt = self
            .db()?
            .query_row("SELECT attempt FROM agent_runs WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .map_err(database)?;
        Ok(Recorder {
            db: self.db.clone(),
            id: id.to_owned(),
            epoch: self.epoch.clone(),
            attempt,
        })
    }
}

impl Drop for Agents {
    fn drop(&mut self) {
        let live: Vec<(String, Live)> = match self.live.lock() {
            Ok(mut live) => live.drain().collect(),
            Err(_) => return,
        };
        for (id, live) in live {
            if let Ok(recorder) = self.recorder(&id) {
                end(live, AgentState::Closed, recorder);
            }
        }
    }
}

fn wait(answer: Receiver<Result<(), LaunchError>>) -> Result<(), LaunchError> {
    answer
        .recv_timeout(REPLY_WAIT)
        .map_err(|_| failure("agent_not_running"))?
}

fn end(live: Live, state: AgentState, recorder: Recorder) {
    match live {
        Live::Owned {
            commands,
            mut thread,
            ..
        } => {
            let (reply, answer) = sync_channel(1);
            if commands.send(Command::Stop(state, reply)).is_ok() {
                let _ = answer.recv_timeout(REPLY_WAIT);
            }
            drop(commands);
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
        }
        #[cfg(unix)]
        Live::Reattached {
            group,
            output,
            probe,
            quit,
            mut thread,
        } => {
            quit.store(true, Ordering::Release);
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
            recorder.ended(state, None, None, kill_holders(group, &output, &probe));
        }
        Live::Hosted { control } => {
            control.stop.store(true, Ordering::Release);
            recorder.finish(state, None, None);
        }
    }
}

/// Stops an owned agent's session, then ends what still holds its output log, such as a
/// child that left the agent's group with setsid. Inputs: the session and its log path.
/// Output: the exit code, and true once no holder of the log is left.
#[cfg(unix)]
fn end_session(session: Session, output: &Path) -> (Option<i32>, bool) {
    let group = session.process_group();
    let code = session.stop();
    let complete = log_probe(output).is_some_and(|probe| kill_holders(group, output, &probe));
    (code, complete)
}

/// Stops an owned agent's session where no output log lock exists. Inputs: the session
/// and the log path, unused. Output: the exit code and true.
#[cfg(not(unix))]
fn end_session(session: Session, _output: &Path) -> (Option<i32>, bool) {
    (session.stop(), true)
}

/// Opens an agent output log read-only to probe its lock, refusing a link, anything but
/// a regular file and a file another user owns. Input: the log path. Output: the open.
#[cfg(unix)]
fn log_probe(path: &Path) -> Option<File> {
    let file = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok()
    .map(File::from)?;
    let metadata = file.metadata().ok()?;
    (metadata.is_file() && metadata.uid() == rustix::process::geteuid().as_raw()).then_some(file)
}

/// Ends what still holds a reattached agent's output log: TERM, then KILL after the
/// grace time, each sent only to a process found holding the log for writing, or to its
/// group when the holder leads it or sits in the agent's recorded group.
/// Inputs: the recorded group, the log path and a read-only open of the log.
/// Output: true once no holder is left; false when holders survive or cannot be found.
#[cfg(unix)]
fn kill_holders(group: u32, output: &Path, probe: &File) -> bool {
    let own = rustix::process::getpgrp();
    for (signal, wait) in [
        (rustix::process::Signal::TERM, STOP_GRACE),
        (rustix::process::Signal::KILL, KILL_WAIT),
    ] {
        if !launch::holder_alive(probe) {
            return true;
        }
        let Some(writers) = writers(output, probe) else {
            return false;
        };
        for writer in writers {
            match rustix::process::getpgid(Some(writer)) {
                Err(rustix::io::Errno::SRCH) => {}
                Ok(leader)
                    if leader != own
                        && (leader == writer
                            || u32::try_from(leader.as_raw_nonzero().get()) == Ok(group)) =>
                {
                    let _ = rustix::process::kill_process_group(leader, signal);
                }
                _ => {
                    let _ = rustix::process::kill_process(writer, signal);
                }
            }
        }
        let until = Instant::now() + wait;
        while launch::holder_alive(probe) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    !launch::holder_alive(probe)
}

/// Lists the other processes that hold the agent output log open for writing, through
/// `lsof`, while the path still names the probed file. Inputs: the log path and a
/// read-only open of it. Output: the writers, or None when the path changed, `lsof`
/// failed or did not list this engine's own open of the log.
#[cfg(target_os = "macos")]
fn writers(output: &Path, probe: &File) -> Option<Vec<rustix::process::Pid>> {
    let probed = probe.metadata().ok()?;
    let names_probe = || {
        std::fs::symlink_metadata(output)
            .is_ok_and(|named| (named.dev(), named.ino()) == (probed.dev(), probed.ino()))
    };
    if !names_probe() {
        return None;
    }
    let listed = crate::spawn_gate::output(
        std::process::Command::new(LSOF)
            .args(["-w", "-n", "-P", "-F", "pa", "--"])
            .arg(output)
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
    )
    .ok()?;
    if !listed.status.success() || !names_probe() {
        return None;
    }
    listed_writers(std::str::from_utf8(&listed.stdout).ok()?)
}

/// Reads `lsof -F pa` output into the processes with a write open, leaving out this
/// engine. Input: the output text. Output: the writers, or None when the text is not
/// that format or does not list this engine, whose read-only probe it must show.
#[cfg(target_os = "macos")]
fn listed_writers(text: &str) -> Option<Vec<rustix::process::Pid>> {
    let own = rustix::process::getpid();
    let (mut current, mut seen_own, mut found) = (None, false, Vec::new());
    for line in text.lines() {
        let (tag, value) = line.split_at_checked(1)?;
        match tag {
            "p" => {
                let pid = rustix::process::Pid::from_raw(value.parse().ok()?)?;
                seen_own |= pid == own;
                current = Some(pid);
            }
            "a" if matches!(value, "w" | "u") => {
                let pid = current?;
                if pid != own && !found.contains(&pid) {
                    found.push(pid);
                }
            }
            "f" | "a" => {}
            _ => return None,
        }
    }
    seen_own.then_some(found)
}

/// Lists the other processes that hold the agent output log open for writing, by
/// matching every readable `/proc/<pid>/fd` entry against the probed file. Inputs: the
/// log path, unused, and a read-only open of it. Output: the writers, or None when
/// `/proc` is unreadable or does not show this engine's own open of the log.
#[cfg(target_os = "linux")]
fn writers(_output: &Path, probe: &File) -> Option<Vec<rustix::process::Pid>> {
    let probed = probe.metadata().ok()?;
    let own = rustix::process::getpid();
    let (mut seen_own, mut found) = (false, Vec::new());
    for process in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
            .and_then(rustix::process::Pid::from_raw)
        else {
            continue;
        };
        let Ok(descriptors) = std::fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        for descriptor in descriptors.flatten() {
            let held = std::fs::metadata(descriptor.path())
                .is_ok_and(|held| (held.dev(), held.ino()) == (probed.dev(), probed.ino()));
            if !held {
                continue;
            }
            if pid == own {
                seen_own = true;
                continue;
            }
            let flags =
                std::fs::read_to_string(process.path().join("fdinfo").join(descriptor.file_name()))
                    .ok()
                    .and_then(|info| {
                        info.lines()
                            .find_map(|line| line.strip_prefix("flags:"))
                            .and_then(|flags| u32::from_str_radix(flags.trim(), 8).ok())
                    });
            if flags.is_some_and(|flags| flags & 3 != 0) && !found.contains(&pid) {
                found.push(pid);
            }
        }
    }
    seen_own.then_some(found)
}

/// Reports that no other platform can list what holds the agent output log, so nothing
/// is signalled. Inputs: the log path and probe, unused. Output: None.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn writers(_output: &Path, _probe: &File) -> Option<Vec<rustix::process::Pid>> {
    None
}

fn drive(
    mut session: Session,
    output: &Path,
    inbox: Receiver<Command>,
    busy: Arc<AtomicBool>,
    recorder: Recorder,
) {
    let mut text = String::new();
    loop {
        match inbox.try_recv() {
            Ok(Command::Send(prompt, reply)) => {
                let sent = session.send(&prompt);
                if sent.is_ok() {
                    text.clear();
                    busy.store(true, Ordering::Release);
                }
                let _ = reply.send(sent);
            }
            Ok(Command::Interrupt(reply)) => {
                let _ = reply.send(session.interrupt());
            }
            Ok(Command::Stop(state, reply)) => {
                let (code, complete) = end_session(session, output);
                recorder.ended(state, code, None, complete);
                let _ = reply.send(Ok(()));
                return;
            }
            Err(TryRecvError::Disconnected) => {
                let (code, complete) = end_session(session, output);
                recorder.ended(AgentState::Closed, code, None, complete);
                return;
            }
            Err(TryRecvError::Empty) => {}
        }
        match session.next_event(POLL) {
            Ok(Some(LaunchEvent::Text { text: delta })) => {
                if text.len() < LAST_TEXT_BYTES_MAX {
                    text.push_str(&delta);
                }
            }
            Ok(Some(LaunchEvent::TurnFinished { .. })) => {
                busy.store(false, Ordering::Release);
                recorder.text(&text);
            }
            Ok(Some(LaunchEvent::Exited { code })) => {
                busy.store(false, Ordering::Release);
                if !text.is_empty() {
                    recorder.text(&text);
                }
                recorder.finish(AgentState::Exited, code, None);
                drop(session);
                return;
            }
            Ok(_) => {}
            Err(error) => {
                busy.store(false, Ordering::Release);
                let (_, complete) = end_session(session, output);
                recorder.ended(AgentState::Failed, None, Some(&error.0), complete);
                return;
            }
        }
    }
}

struct Stored {
    id: String,
    project: ProjectId,
    role: AgentRole,
    label: String,
    harness: String,
    state: AgentState,
    reattached: bool,
    attempt: u32,
    exit_code: Option<i32>,
    reason: Option<String>,
    last_text: Option<String>,
    started_ms: u64,
    ended_ms: Option<u64>,
    hosted: bool,
}

fn stored(row: &Row<'_>) -> rusqlite::Result<Stored> {
    let invalid = |index: usize| {
        rusqlite::Error::InvalidColumnType(index, "agent_runs".into(), rusqlite::types::Type::Text)
    };
    let project: String = row.get(1)?;
    let role: String = row.get(2)?;
    let state: String = row.get(5)?;
    let started: i64 = row.get(11)?;
    let ended: Option<i64> = row.get(12)?;
    Ok(Stored {
        id: row.get(0)?,
        project: ProjectId::new(project).map_err(|_| invalid(1))?,
        role: AgentRole::parse(&role).ok_or_else(|| invalid(2))?,
        label: row.get(3)?,
        harness: row.get(4)?,
        state: AgentState::parse(&state).ok_or_else(|| invalid(5))?,
        reattached: row.get::<_, i64>(6)? == 1,
        attempt: row.get(7)?,
        exit_code: row.get(8)?,
        reason: row.get(9)?,
        last_text: row.get(10)?,
        started_ms: u64::try_from(started).unwrap_or(0),
        ended_ms: ended.and_then(|ended| u64::try_from(ended).ok()),
        hosted: row.get::<_, i64>(13)? == 1,
    })
}

impl Stored {
    fn view(self, live: Option<&Live>, served: bool) -> AgentView {
        let running = self.state == AgentState::Running;
        let driven = matches!(live, Some(Live::Owned { .. } | Live::Hosted { .. }));
        let busy = match live {
            Some(Live::Owned { busy, .. }) => busy.load(Ordering::Acquire),
            Some(Live::Hosted { control }) => control.busy.load(Ordering::Acquire),
            _ => false,
        };
        let asked = match live {
            Some(Live::Hosted { control }) if control.stop.load(Ordering::Acquire) => {
                Some(control.restart.load(Ordering::Acquire))
            }
            _ => None,
        };
        let can_restart = if self.hosted {
            live.is_some() || (!running && served)
        } else {
            !running || live.is_some()
        };
        let status = match self.state {
            AgentState::Running if self.reattached => {
                "Running, reconnected after CrazyAgent restarted".to_owned()
            }
            AgentState::Running if asked == Some(true) => "Restarting".to_owned(),
            AgentState::Running if asked == Some(false) => "Stopping".to_owned(),
            AgentState::Running if busy => "Working".to_owned(),
            AgentState::Running => "Running".to_owned(),
            AgentState::Stopped => "Stopped".to_owned(),
            AgentState::Exited => match self.exit_code {
                Some(0) | None => "Finished".to_owned(),
                Some(code) => format!("Exited with code {code}"),
            },
            AgentState::Failed => "Failed".to_owned(),
            AgentState::Closed => "Stopped when CrazyAgent closed".to_owned(),
        };
        AgentView {
            id: self.id,
            project: self.project,
            role: self.role,
            label: self.label,
            harness: self.harness,
            state: self.state,
            status,
            reattached: self.reattached,
            busy,
            attempt: self.attempt,
            can_stop: running && live.is_some() && asked.is_none(),
            can_interrupt: running && driven && busy,
            can_restart,
            exit_code: self.exit_code,
            reason: self.reason,
            last_text: self.last_text,
            started_ms: self.started_ms,
            ended_ms: self.ended_ms,
        }
    }
}

#[cfg(all(test, unix))]
#[path = "agents/tests.rs"]
pub(crate) mod tests;
