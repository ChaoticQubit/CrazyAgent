use super::{
    AGENT_COLUMNS, Accounts, AgentRole, AgentStatus, DELEGATIONS_PER_REPLY_MAX, Delegation,
    MessageAuthor, MessageState, NOTE_BYTES_MAX, TaskSource, TaskStatus, TeamAccount, TeamReach,
    admit, agent_row, append_message, bound_text, eligible_accounts, launch_harness, names, now_ms,
    parse_delegations, parse_wakes, pick_account, tool,
};
use crate::agent_git::Layout;
use crate::agents::{
    AgentState as RunState, Agents, Hosted, HostedAgent, HostedRequest, HostedRestart,
};
use crate::config::ModelAccess;
use crate::engine::bootstrap::{AgentCredential, NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::owner::HostStamp;
use crate::engine::owner::native_auth::NativeAuthFact;
use crate::engine::{EngineError, NativeProfileCustody, RegisteredNativeAuthHost};
use crate::harnesses::launch::{
    AccountBinding, GitIdentity, LaunchError, LaunchEvent, LaunchLimits, LaunchRequest,
    RUN_TIME_MAX,
};
use crate::harnesses::leftovers::Unstopped;
use crate::harnesses::terminal_session::{
    AgentSession, PaneRecord, Resume, TerminalSession, end_left_pane,
};
use crate::harnesses::transcript::TranscriptEvent;
use crate::launch_context::AgentContext;
use crate::master_sessions::ResumePlan;
use crate::project_setup::{StoredSetup, load_setup};
use crate::protocol::{CredentialRef, ProjectId, RequestId, SessionId};
use crate::research::runner::{
    ResearchAccount, RunError, add_worktree, confined_unavailable, ensure_repository, model_remote,
};
use crate::sandbox::policy::{AgentAccess, AgentPolicy, Narrowing};
use crate::sandbox::toolchains::{self, ToolHomes, Toolchain, Toolchains};
use crate::sandbox::{ProjectSandbox, SandboxError, SandboxNetwork, SandboxRemote};
use crate::terminal::{AgentId, Host, PANES_PER_PROJECT_MAX};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const BRIEF_BYTES_MAX: usize = 6000;
const RESULT_IN_MESSAGE_BYTES_MAX: usize = 3000;
/// The most bytes one sub-agent results message holds, so neither the stored message nor
/// the text typed into a live master, with its speaker tag, ever loses a line.
const RESULTS_MESSAGE_BYTES_MAX: usize = super::MESSAGE_BYTES_MAX * 2 - 64;
const RESULTS_TRAILER: &str = "\nTell the owner what was done.";
const _: () = assert!(RESULTS_MESSAGE_BYTES_MAX + 8 <= super::master::INPUT_TEXT_MAX);
const _: () = assert!(RESULT_IN_MESSAGE_BYTES_MAX * 2 + 256 < RESULTS_MESSAGE_BYTES_MAX);
const DELEGATION_ROUNDS_MAX: i64 = 3;
const PROGRESS_EVERY: Duration = Duration::from_secs(1);
const EVENT_WAIT: Duration = Duration::from_millis(200);
const ORDER_WAIT: Duration = Duration::from_millis(100);
const IDLE_EVENT_WAIT: Duration = Duration::from_millis(5);
const JOIN_WAIT: Duration = Duration::from_secs(15);
const SWEEP_EVERY: Duration = Duration::from_secs(2);
const SESSION_EVENTS_MAX: usize = 200_000;
const HISTORY_MESSAGES_MAX: i64 = 12;
const HISTORY_MESSAGE_BYTES_MAX: usize = 1500;
const HISTORY_BYTES_MAX: usize = 6000;
pub(crate) const TEAM_HOST: &str = "team";
const SIGN_IN_REJECTED: &str = "master_sign_in_rejected";
const LEFT_RUNNING_NOTE: &str = "Kept running while CrazyAgent was closed";
const TURN_LIMIT: &str = "team_turn_limit";
const TIME_LIMIT_NOTE: &str = "stopped by a time limit";

/// Which of the owner's time limits bounds an agent's turn, with the limit as the owner set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnBound {
    Task(Duration),
    Session(Duration),
}
const EMPTY_REPLY_NOTE: &str = "The master replied with no text, so no work was handed out. Send the message again, or hand a task to a sub-agent yourself.";

/// Bounds every team agent obeys. The task, session and sub-agent idle limits are the
/// owner's team settings, read per project when a turn or an agent starts or an idle agent
/// is checked, so none is set here. `output_bytes` caps the harness output of one process,
/// `attempts_max` the tries per task, `master_idle_stop` how long an idle master is kept
/// once no work is in flight and `rate_limit_pause` how long an account that hit its limit
/// is skipped.
#[derive(Debug, Clone, Copy)]
pub struct TeamLimits {
    pub output_bytes: usize,
    pub attempts_max: i64,
    pub master_idle_stop: Duration,
    pub rate_limit_pause: Duration,
    pub stop_grace: Duration,
}

impl Default for TeamLimits {
    fn default() -> Self {
        Self {
            output_bytes: 8 * 1024 * 1024,
            attempts_max: 2,
            master_idle_stop: Duration::from_secs(super::MASTER_IDLE_STOP_SECS.into()),
            rate_limit_pause: Duration::from_secs(10 * 60),
            stop_grace: Duration::from_secs(2),
        }
    }
}

impl TeamLimits {
    fn validate(&self) -> Result<(), EngineError> {
        let ok = (1..=64 * 1024 * 1024).contains(&self.output_bytes)
            && (1..=10).contains(&self.attempts_max)
            && !self.master_idle_stop.is_zero()
            && self.stop_grace <= Duration::from_secs(30);
        ok.then_some(())
            .ok_or_else(|| EngineError("team_limits_invalid".into()))
    }
}

/// What a worker thread is told to do next: start a live master with the instructions its
/// harness reads at launch and the signed-in account it needs, run one turn, with its
/// prompt, the signed-in account it needs and the owner's task time limit for it (none for
/// no limit), or stop.
enum Order {
    Start(String, Option<Box<Grant>>),
    Turn(String, Option<Box<Grant>>, Option<Duration>),
    Stop,
}

/// What one turn of an agent on a signed-in managed account needs: the profile, held
/// under the engine's custody lock for that turn only, the project's launch context, and
/// the access level the owner has saved now, read again for every turn (AL-2).
struct Grant {
    profile: NativeProfileUse,
    context: AgentContext,
    access: AgentAccess,
}

/// Everything about an agent's harness launch except the account, so a managed agent
/// can start a fresh process for each turn. `pane` is the terminal host and the agent's
/// pane name when the agent runs its interactive app in a pane; `resume` the harness
/// conversation a live master continues instead of starting one; `toolchains` the families
/// its sandbox profile, built once for every turn, lets it use.
struct Plan {
    harness: String,
    executable: PathBuf,
    working_dir: PathBuf,
    model: Option<String>,
    tools: bool,
    sandbox_profile: PathBuf,
    access: AgentAccess,
    toolchains: Vec<Toolchain>,
    layout: Layout,
    git_identity: Option<GitIdentity>,
    limits: LaunchLimits,
    pane: Option<(Arc<dyn Host>, AgentId)>,
    live: bool,
    resume: Option<Resume>,
    continues: bool,
}

impl Plan {
    /// Builds one start of the plan at `access`, the level the owner saved when this start
    /// happens (AL-2): Full access runs without Seatbelt, any other level in the agent's
    /// profile, and a clone at Sandboxed is refused, see [`confinement`]. Takes the account
    /// and the level. Returns the request, or the plain refusal.
    fn request(
        &self,
        account: AccountBinding,
        access: AgentAccess,
    ) -> Result<LaunchRequest, String> {
        let unconfined = confinement(self.layout, access)?;
        Ok(LaunchRequest {
            harness: self.harness.clone(),
            executable: self.executable.clone(),
            working_dir: self.working_dir.clone(),
            account,
            model: self.model.clone(),
            tools: self.tools,
            sandbox_profile: (!unconfined).then(|| self.sandbox_profile.clone()),
            unconfined,
            git_identity: self.git_identity.clone(),
            limits: self.limits,
        })
    }
}

/// Decides how an agent in `layout` starts at `access`, the one rule every launch, wake and
/// per-turn start follows: a clone at Sandboxed is refused, since the owner lowered the
/// level after the agent began on its own branch and its changes still wait in Changes;
/// Full access runs without Seatbelt; every other pair runs in the agent's profile.
/// Inputs: the agent's layout and the saved level. Output: true to run unconfined, or the
/// plain refusal with `agent_access_lowered`.
fn confinement(layout: Layout, access: AgentAccess) -> Result<bool, String> {
    match (layout, access) {
        (Layout::Clone, AgentAccess::Sandboxed) => Err(with_code(
            "this agent started with its own branch, but the project is Sandboxed now; its changes are still in Changes",
            Some("agent_access_lowered"),
        )),
        (_, AgentAccess::Full) => Ok(true),
        (_, AgentAccess::Sandboxed | AgentAccess::OwnBranch) => Ok(false),
    }
}

/// How an agent's process lives: kept across turns on an endpoint account, or started
/// for each turn on a managed profile whose home is given.
enum Mode {
    Kept {
        account: AccountBinding,
        context: AgentContext,
    },
    PerTurn {
        home: PathBuf,
    },
}

/// How an agent's thread gets its harness: a fresh launch from its plan, or the pane of
/// an agent a previous engine left running, taken over from the pane's stored record.
enum Launch {
    Fresh(Box<Plan>, Box<Mode>),
    Adopt(Box<Adoption>),
}

struct Adoption {
    host: Arc<dyn Host>,
    agent: AgentId,
    record: PaneRecord,
    limits: LaunchLimits,
    live: bool,
    managed: Option<Managed>,
    access: AgentAccess,
    toolchains: Option<Vec<Toolchain>>,
}

/// What a live master on a signed-in account needs when a later engine takes its pane over:
/// its private home, where the credential copy goes again, and the harness executable that
/// renews it.
struct Managed {
    home: PathBuf,
    executable: PathBuf,
}

/// Who one agent is in its team: the project, role, team harness, account and tools
/// switch, with its entry in the engine's agent register when there is one.
struct Seat {
    project: ProjectId,
    role: AgentRole,
    harness: String,
    connection: CredentialRef,
    tools: bool,
    hosted: Option<Hosted>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnEnd {
    Success,
    Failed,
    RateLimited,
    TimedOut,
    TooLong,
    Cancelled,
    Interrupted,
    Closed,
}

impl TurnEnd {
    /// The stored word for how a live master turn ended.
    /// Input: the turn end. Output: a fixed lowercase word.
    fn name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::RateLimited => "rate_limited",
            Self::TimedOut => "timed_out",
            Self::TooLong => "too_long",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Closed => "closed",
        }
    }
}

enum Report {
    Ready {
        group: u32,
        pane: Option<PaneRecord>,
    },
    Progress {
        bytes: usize,
    },
    Usage {
        tokens: crate::usage::capture::Tokens,
    },
    Finished {
        text: String,
        end: TurnEnd,
        cause: Option<String>,
    },
    Ended {
        problem: Option<String>,
    },
    Turn {
        end: Option<TurnEnd>,
        text: String,
    },
    Conversation(Resume),
    ResumeRefused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    Task(i64),
    Message(i64),
}

/// Why the engine stopped an agent whose running process holds more than the owner saved:
/// what the saved policy no longer allows, or toolchains a pane from before the engine kept
/// them may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Narrowed {
    Saved(Narrowing),
    Unrecorded,
}

impl Narrowed {
    /// The plain reason the stopped agent shows, with its code. Input: the cause.
    /// Output: the sentence.
    fn problem(self) -> String {
        let (text, code) = match self {
            Self::Saved(Narrowing::Access) => (
                "the project's agent access was lowered, so the agent was stopped; its next start uses the new level",
                Narrowing::Access.code(),
            ),
            Self::Saved(Narrowing::TurnedOff) => (
                "the project's toolchain access was turned off, so the agent was stopped; its next start goes without it",
                Narrowing::TurnedOff.code(),
            ),
            Self::Saved(Narrowing::NotAllowed) => (
                "a toolchain the agent started with is no longer allowed for the project (turned off, or no longer used by the project), so the agent was stopped; its next start goes without it",
                Narrowing::NotAllowed.code(),
            ),
            Self::Saved(Narrowing::PolicyUnreadable) => (
                "the project's agent access could not be read, so the agent was stopped; it starts again once it can be read",
                Narrowing::PolicyUnreadable.code(),
            ),
            Self::Saved(Narrowing::FolderUnreadable) => (
                "the project folder could not be read to check the agent's toolchains, so the agent was stopped",
                Narrowing::FolderUnreadable.code(),
            ),
            Self::Unrecorded => (
                "the agent started before the engine kept the toolchains it was given, so it was stopped; its next start uses the project's toolchain settings",
                "agent_toolchains_unrecorded",
            ),
        };
        with_code(text, Some(code))
    }
}

struct Worker {
    project: ProjectId,
    role: AgentRole,
    harness: String,
    connection: CredentialRef,
    orders: Sender<Order>,
    reports: Receiver<Report>,
    cancel: Arc<AtomicBool>,
    leave: Arc<AtomicBool>,
    busy: Option<Busy>,
    idle_since: Instant,
    stopping: bool,
    held: bool,
    restarting: bool,
    warm: bool,
    worked: bool,
    managed: bool,
    tools: bool,
    team_tool: bool,
    grants: u64,
    thread: Option<JoinHandle<()>>,
    hosted: Option<Hosted>,
    live: bool,
    briefed: bool,
    turn_started: Option<i64>,
    session: Option<String>,
    seed: Option<String>,
    bound: Option<TurnBound>,
    spawned: Instant,
    handoff: Option<String>,
    access: AgentAccess,
    ran_before: Duration,
    told_finished: BTreeSet<i64>,
    toolchains: Option<Vec<Toolchain>>,
    checked: Option<AgentPolicy>,
    narrowed: Option<Narrowed>,
}

impl Worker {
    /// How long the agent has run toward its session: since its terminal started, including
    /// the time before an engine restart adopted it. Takes the worker. Returns the time.
    fn ran(&self) -> Duration {
        self.ran_before + self.spawned.elapsed()
    }
}

/// Reads the live model access on every tick; none means no pool is configured.
pub type AccessSource = Box<dyn Fn() -> Option<ModelAccess> + Send>;

/// Runs each project's team: the master on the team plan's master harness and
/// sub-agents on its sub-harnesses, each in the project sandbox, sub-agents in
/// their own git worktrees. The engine admits every task; agents only propose.
pub struct TeamRuntime {
    connection: Connection,
    root: PathBuf,
    state: PathBuf,
    executables: BTreeMap<&'static str, PathBuf>,
    access: AccessSource,
    limits: TeamLimits,
    workers: BTreeMap<i64, Worker>,
    custody: Result<NativeProfileCustody, EngineError>,
    delegation_tool: Option<RegisteredNativeAuthHost>,
    agents: Option<Arc<Agents>>,
    terminal: Option<Arc<dyn Host>>,
    swept: Option<Instant>,
    events: Option<super::master::TeamEvents>,
    watch: super::events::Watch,
    live_starts: BTreeMap<ProjectId, Vec<Instant>>,
    resuming: Option<(i64, Resume)>,
    held_restarts: Vec<HostedRestart>,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// How one agent start ended: started, held back because a projects home move holds the
/// project folder, so the work that asked for it waits for the move to end with nothing
/// spent or switched off, or refused for the plain reason.
#[derive(Debug, PartialEq, Eq)]
enum Start<T> {
    Started(T),
    Held,
    Refused(String),
}

/// The most stopped masters of one project the last-master lookup reads.
const LAST_MASTERS_READ: i64 = 64;

/// Finds the project's newest stopped or failed master on a harness with its worktree still
/// recorded, so it starts again in its own worktree and home. The harness matches by catalog
/// entry, so a master recorded under an alias ("claude") serves a plan that names the entry
/// ("claude-code"). Takes the database, project and harness. Returns the agent, or None.
pub(super) fn last_master(
    connection: &Connection,
    project: &ProjectId,
    harness: &str,
) -> Result<Option<i64>, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT a.id,CASE WHEN length(a.harness)<=64 THEN a.harness END FROM team_agents a
             JOIN team_worktrees w ON w.agent=a.id AND w.project=a.project
             WHERE a.project=?1 AND a.role='master' AND a.status IN ('stopped','failed')
             ORDER BY a.id DESC LIMIT ?2",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(params![project.as_str(), LAST_MASTERS_READ], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    assert!(rows.len() <= LAST_MASTERS_READ as usize);
    Ok(rows.into_iter().find_map(|(id, stored)| {
        stored
            .is_some_and(|stored| crate::harnesses::same(&stored, harness))
            .then_some(id)
    }))
}

/// Refuses team state where no 0700 directory can be made or checked.
/// Takes the path, unused. Returns `team_state_unavailable`.
#[cfg(not(unix))]
fn private_directory(_path: &Path) -> Result<(), EngineError> {
    Err(EngineError("team_state_unavailable".into()))
}

#[cfg(unix)]
fn private_directory(path: &Path) -> Result<(), EngineError> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(error(e)),
    }
    let metadata = std::fs::symlink_metadata(path).map_err(error)?;
    if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(EngineError("team_state_not_private".into()));
    }
    Ok(())
}

impl TeamRuntime {
    /// Opens the team runtime over the engine database.
    /// Takes the database, an engine-owned state directory outside every project, the
    /// launch executables by harness, a source of live model access and the limits.
    /// Returns the runtime after returning work interrupted by an earlier engine to the queue.
    pub fn open(
        database: &Path,
        state: &Path,
        executables: BTreeMap<&'static str, PathBuf>,
        access: AccessSource,
        limits: TeamLimits,
    ) -> Result<Self, EngineError> {
        limits.validate()?;
        if !state.is_absolute() {
            return Err(EngineError("team_state_not_canonical".into()));
        }
        private_directory(state)?;
        if state.canonicalize().map_err(error)? != state {
            return Err(EngineError("team_state_not_canonical".into()));
        }
        for directory in ["agents", "profiles"] {
            private_directory(&state.join(directory))?;
        }
        for (harness, executable) in &executables {
            if launch_harness(harness) != Some(*harness) || !executable.is_absolute() {
                return Err(EngineError("team_executable_invalid".into()));
            }
        }
        let connection = Connection::open_with_flags(
            database,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(error)?;
        connection
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
            .map_err(error)?;
        super::initialize(&connection)?;
        crate::project_setup::initialize(&connection)?;
        let now = now_ms();
        connection
            .execute_batch(&format!(
                "BEGIN IMMEDIATE;
                 UPDATE team_agents SET status='stopped', task=NULL, error='Stopped when CrazyAgent closed', stopped_ms={now}
                   WHERE status IN ('starting','idle','working');
                 UPDATE team_tasks SET status='queued', agent=NULL, cancel_requested=0
                   WHERE status='running' AND cancel_requested=0;
                 UPDATE team_tasks SET status='cancelled', agent=NULL, error='Cancelled by you', finished_ms={now}
                   WHERE status='running' AND cancel_requested=1;
                 UPDATE team_messages SET state='pending' WHERE state='sent';
                 COMMIT;"
            ))
            .map_err(error)?;
        let custody = std::fs::symlink_metadata(database)
            .map_err(error)
            .and_then(|metadata| NativeProfileCustody::capture(database, &metadata));
        let root = database
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| EngineError("team_state_not_canonical".into()))?;
        Ok(Self {
            connection,
            root,
            state: state.to_path_buf(),
            executables,
            access,
            limits,
            workers: BTreeMap::new(),
            custody,
            delegation_tool: None,
            agents: None,
            terminal: None,
            swept: None,
            events: None,
            watch: super::events::Watch::default(),
            live_starts: BTreeMap::new(),
            resuming: None,
            held_restarts: Vec::new(),
        })
    }

    /// Records every team agent in the engine's agent register, so the owner can stop,
    /// interrupt and restart each one with every other agent.
    /// Takes the register of the same engine database. Returns the runtime.
    pub fn with_agents(mut self, agents: Arc<Agents>) -> Self {
        self.agents = Some(agents);
        self
    }

    /// Gives a live master's `message`, `answer` and `status` tools their team event
    /// handlers. Takes the handlers. Returns the runtime; without them those tools answer
    /// that team messaging is not available.
    pub fn with_events(mut self, events: super::master::TeamEvents) -> Self {
        self.events = Some(events);
        self
    }

    /// Runs every team agent whose harness has an interactive app in a pane of `host` named
    /// `team-<id>`, where the owner can watch and type. Panes an earlier engine left are
    /// taken over, oldest agent first, when their agent is still in the team plan on a usable
    /// account, so the agent goes on without a relaunch; every other team pane ends and its
    /// project hears why. Takes the host. Returns the runtime, or the host's plain code.
    pub fn with_terminal(mut self, host: Arc<dyn Host>) -> Result<Self, EngineError> {
        let records = self.pane_records()?;
        self.terminal = Some(host.clone());
        let access = (self.access)().unwrap_or_default();
        let panes: BTreeMap<i64, AgentId> = host
            .alive()?
            .into_iter()
            .filter_map(|agent| agent.team_agent().map(|id| (id, agent)))
            .collect();
        for (id, agent) in panes {
            let refusal = match records.get(&id) {
                Some(record) => self.adopt(&host, id, record, &access)?.err(),
                None => Some("the engine has no record of it".to_owned()),
            };
            if let Some(reason) = refusal {
                match end_left_pane(host.as_ref(), &agent, "team") {
                    Ok(_) => {}
                    Err(problem) if problem.0 == "terminal_agent_unknown" => {}
                    Err(problem) => return Err(problem.into()),
                }
                if let Some(record) = records.get(&id) {
                    record.remove_context_files();
                }
                self.closed_note(id, &reason)?;
            }
        }
        Ok(self)
    }

    /// Reads and forgets the pane records an earlier engine kept, so each is used once.
    /// Returns the records by team agent id; malformed ones are dropped.
    fn pane_records(&mut self) -> Result<BTreeMap<i64, PaneRecord>, EngineError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let rows: Vec<(i64, String)> = {
            let mut statement = tx
                .prepare("SELECT agent,record FROM team_panes")
                .map_err(error)?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        tx.execute("DELETE FROM team_panes", []).map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, text)| PaneRecord::decode(&text).map(|record| (id, record)))
            .collect())
    }

    /// Takes over one live team pane when its agent may still run: the project's setup and
    /// team plan still hold its role and harness, the team is not paused, the project has
    /// room, and its account is still an endpoint in the project's pool. The pane keeps the
    /// access level its record says it started at, Full access for a record without one, and
    /// the toolchains it started with, every family for a record without them, so a level
    /// lowered or a toolchain turned off while no engine ran stops it on the next tick. Takes the host, the agent id, its pane record
    /// and the live access. Returns Ok, or why the pane must end.
    fn adopt(
        &mut self,
        host: &Arc<dyn Host>,
        id: i64,
        record: &PaneRecord,
        access: &ModelAccess,
    ) -> Result<Result<(), String>, EngineError> {
        let row: Option<(String, String, String, bool, String)> = self
            .connection
            .query_row(
                "SELECT project,role,harness,tools,connection FROM team_agents WHERE id=?1",
                [id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(error)?;
        let Some((project, role, harness, tools, connection)) = row else {
            return Ok(Err("the engine has no record of it".into()));
        };
        let Some(launch) = launch_harness(&harness) else {
            return Ok(Err("its harness cannot run team agents".into()));
        };
        let (Ok(project), Some(role), Ok(connection)) = (
            ProjectId::new(project),
            AgentRole::parse(&role),
            CredentialRef::new(connection),
        ) else {
            return Ok(Err("its record is not valid".into()));
        };
        let Some(stored) = self.stored(&project)? else {
            return Ok(Err("its project was removed".into()));
        };
        if super::paused(&self.connection, &project)? {
            return Ok(Err("the team is paused".into()));
        }
        let in_plan = match role {
            AgentRole::Master => {
                crate::harnesses::same(&stored.team.master, &harness)
                    && stored.team.master_tools == tools
                    && self.master(&project).is_none()
            }
            AgentRole::Sub => super::slot(&stored.team, &harness).is_some_and(|slot| {
                slot.tools == tools
                    && self.live(&project, AgentRole::Sub, Some(&harness))
                        < usize::from(slot.agents)
                    && self.live(&project, AgentRole::Sub, None)
                        < usize::from(stored.team.max_agents.saturating_sub(1))
            }),
        };
        if !in_plan {
            return Ok(Err("the team plan no longer has room for it".into()));
        }
        if self.panes(&project) >= PANES_PER_PROJECT_MAX {
            return Ok(Err(format!(
                "the project already runs {PANES_PER_PROJECT_MAX} agents"
            )));
        }
        let live = self.live_master(&project, role, launch);
        let reach = match eligible_accounts(access, &project, launch) {
            Accounts::Usable(accounts) => accounts
                .iter()
                .find(|account| account.connection == connection)
                .map(|account| matches!(account.reach, TeamReach::Endpoint { .. })),
            _ => None,
        };
        let managed = match reach {
            Some(true) => None,
            Some(false) if live => match self.executables.get(launch) {
                Some(executable) => Some(Managed {
                    home: self.agent_folder(id).join("home"),
                    executable: executable.clone(),
                }),
                None => return Ok(Err("its harness is not installed".into())),
            },
            _ => return Ok(Err("its account is no longer in the project's pool".into())),
        };
        let Ok(agent) = AgentId::team(id) else {
            return Ok(Err("its record is not valid".into()));
        };
        let hosted = match self.host(&stored, id, role, launch, &connection, None) {
            Ok(hosted) => hosted,
            Err(problem) => return Ok(Err(problem)),
        };
        self.connection
            .execute(
                "UPDATE team_agents SET status='starting', task=NULL, error=NULL, stopped_ms=NULL, heartbeat_ms=?2 WHERE id=?1",
                params![id, now_ms()],
            )
            .map_err(error)?;
        let adoption = Adoption {
            host: host.clone(),
            agent,
            record: record.clone(),
            limits: self.launch_limits(),
            live,
            managed,
            access: record.access.unwrap_or(AgentAccess::Full),
            toolchains: record.toolchains.clone(),
        };
        let session = if live {
            crate::master_sessions::live_session(&self.connection, &project)?
        } else {
            None
        };
        self.run_worker(
            id,
            Launch::Adopt(Box::new(adoption)),
            Seat {
                project,
                role,
                harness,
                connection,
                tools,
                hosted,
            },
        )?;
        self.workers.get_mut(&id).expect("adopted worker").session = session;
        Ok(Ok(()))
    }

    /// Tells the agent's project that its terminal was closed and why, or the engine log
    /// when the agent is not known. Takes the team agent id and the plain reason.
    fn closed_note(&mut self, id: i64, reason: &str) -> Result<(), EngineError> {
        let project: Option<String> = self
            .connection
            .query_row("SELECT project FROM team_agents WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(error)?;
        match project.and_then(|project| ProjectId::new(project).ok()) {
            Some(project) => self.note(
                &project,
                &format!(
                    "The terminal of team agent {id} was closed: {}.",
                    bound_text(reason, 300)
                ),
            ),
            None => {
                eprintln!("team runtime: closed terminal team-{id}: {reason}");
                Ok(())
            }
        }
    }

    /// Ends team panes that no live agent of this engine owns, at most every `SWEEP_EVERY`
    /// once any agent has run, so no pane outlives its agent. Each project hears which.
    fn sweep(&mut self) -> Result<(), EngineError> {
        let Some(host) = self.terminal.clone() else {
            return Ok(());
        };
        if self.swept.is_none_or(|at| at.elapsed() < SWEEP_EVERY) {
            return Ok(());
        }
        self.swept = Some(Instant::now());
        let Ok(alive) = host.alive() else {
            return Ok(());
        };
        for agent in alive {
            let Some(id) = agent.team_agent() else {
                continue;
            };
            if self.workers.contains_key(&id) {
                continue;
            }
            match end_left_pane(host.as_ref(), &agent, "team") {
                Ok(_) => self.closed_note(id, "its agent had stopped")?,
                Err(problem) if problem.0 == "terminal_agent_unknown" => {}
                Err(problem) => return Err(problem.into()),
            }
        }
        Ok(())
    }

    /// Gives every team master with tools on the engine's `delegate` tool, served by `executable`.
    /// Takes the engine executable; the file at its canonical path is captured, so every later
    /// master launch serves the tool from that same file or is refused.
    /// Returns the runtime, or a plain code when the executable is not a file or is unsafe.
    pub fn with_delegation_tool(mut self, executable: &Path) -> Result<Self, EngineError> {
        let canonical = executable
            .canonicalize()
            .ok()
            .filter(|path| path.is_file())
            .ok_or_else(|| EngineError("team_executable_invalid".into()))?;
        self.delegation_tool = Some(RegisteredNativeAuthHost::capture(&canonical)?);
        Ok(self)
    }

    /// Runs one step for every project with team work: applies agent reports and the
    /// owner's stop and restart from the agent register, sends pending messages to the
    /// master, delivers finished results, starts or reuses sub-agents within the plan's
    /// limits, stops idle or cancelled agents and ends team panes no agent owns.
    pub fn tick(&mut self) -> Result<(), EngineError> {
        self.collect()?;
        self.owner_controls()?;
        let access = (self.access)().unwrap_or_default();
        self.owner_restarts(&access)?;
        self.owner_wakes(&access)?;
        for project in self.active_projects()? {
            self.step(&project, &access)?;
        }
        self.collect()?;
        self.owner_controls()?;
        self.note_conversations()?;
        self.watch_questions()?;
        self.sweep()
    }

    /// Reads each busy sub-agent's terminal for a question it waits on, at most once each
    /// `events::SCAN_EVERY`, so the master hears it as an event. Returns any SQLite failure.
    fn watch_questions(&mut self) -> Result<(), EngineError> {
        let Some(host) = self.terminal.clone() else {
            return Ok(());
        };
        if !self.watch.due() {
            return Ok(());
        }
        let watched: Vec<super::events::Watched> = self
            .workers
            .iter()
            .filter(|(_, worker)| worker.role == AgentRole::Sub && !worker.stopping)
            .filter_map(|(id, worker)| match worker.busy {
                Some(Busy::Task(task)) => Some(super::events::Watched {
                    agent: *id,
                    project: worker.project.clone(),
                    task: Some(task),
                }),
                _ => None,
            })
            .collect();
        self.watch
            .scan(&self.connection, host.as_ref(), &watched, now_ms())
    }

    /// Writes the engine's note on each task a conversation master handed off that has
    /// ended, at most `NOTES_PER_TICK_MAX` a tick, so that conversation and the master's
    /// next turn learn how it went. Each task is noted once. Returns any SQLite failure.
    fn note_conversations(&mut self) -> Result<(), EngineError> {
        type Row = (
            i64,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<String>,
        );
        let rows: Vec<Row> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT c.task,t.harness,t.status,t.prompt,t.result,t.error,t.agent,a.worktree
                     FROM team_conversation_tasks c JOIN team_tasks t ON t.id=c.task
                     LEFT JOIN team_agents a ON a.id=t.agent
                     WHERE c.note IS NULL AND t.status IN ('done','failed','cancelled')
                     ORDER BY c.task LIMIT ?1",
                )
                .map_err(error)?;
            statement
                .query_map([NOTES_PER_TICK_MAX], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                })
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        if rows.is_empty() {
            return Ok(());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let now = now_ms();
        for (id, harness, status, prompt, result, problem, agent, worktree) in rows {
            let status = TaskStatus::parse(&status)
                .filter(|status| status.finished())
                .expect("the sweep reads ended tasks only");
            let note = conversation_note(&EndedTask {
                id,
                harness,
                status,
                prompt,
                result,
                problem,
                agent,
                worktree,
            });
            let changed = tx
                .execute(
                    "UPDATE team_conversation_tasks SET note=?2, noted_ms=?3 WHERE task=?1 AND note IS NULL",
                    params![id, note, now],
                )
                .map_err(error)?;
            assert!(changed <= 1);
        }
        tx.commit().map_err(error)
    }

    /// Applies the owner's stop and restart from the agent register to live agents and
    /// shows the register which agents are working. A stop cancels the agent's work; a
    /// restart returns it to its queue for the fresh process. What the stop stores, the live
    /// switch turned off and the owner's stop of a sub-agent (AW-1), is written before the
    /// agent is touched, so a failed write leaves the stop to the next tick.
    fn owner_controls(&mut self) -> Result<(), EngineError> {
        for (id, worker) in &mut self.workers {
            let Some(hosted) = &worker.hosted else {
                continue;
            };
            hosted.set_busy(worker.busy.is_some());
            let asked = hosted.requested();
            let restart = asked == Some(HostedRequest::Restart) && !worker.held;
            if !restart && (asked != Some(HostedRequest::Stop) || worker.stopping) {
                continue;
            }
            if worker.live {
                super::master::set_live(&self.connection, &worker.project, false)?;
            }
            if worker.role == AgentRole::Sub {
                self.connection
                    .execute(
                        "INSERT OR REPLACE INTO team_owner_stops(agent,project,stopped_ms) VALUES (?1,?2,?3)",
                        params![*id, worker.project.as_str(), now_ms()],
                    )
                    .map_err(error)?;
            }
            if restart {
                worker.held = true;
                worker.restarting = true;
            }
            worker.stopping = true;
            worker.cancel.store(true, Ordering::SeqCst);
            let _ = worker.orders.send(Order::Stop);
        }
        Ok(())
    }

    fn owner_restarts(&mut self, access: &ModelAccess) -> Result<(), EngineError> {
        let Some(agents) = self.agents.clone() else {
            return Ok(());
        };
        let mut restarts = std::mem::take(&mut self.held_restarts);
        restarts.extend(agents.take_restarts(TEAM_HOST));
        for restart in restarts {
            if let Ok(reference) = restart.reference.parse::<i64>() {
                self.start_again(reference, Some(&restart), access)?;
            }
        }
        Ok(())
    }

    /// Serves the owner's Wake buttons (DEC-102), at most `OWNER_WAKES_PER_TICK_MAX` a tick,
    /// oldest first: each request is taken once and the agent wakes through
    /// [`TeamRuntime::wake`], never as a new agent. Returns any SQLite failure.
    fn owner_wakes(&mut self, access: &ModelAccess) -> Result<(), EngineError> {
        let requests: Vec<i64> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT agent FROM team_wake_requests ORDER BY requested_ms, agent LIMIT ?1",
                )
                .map_err(error)?;
            statement
                .query_map([OWNER_WAKES_PER_TICK_MAX as i64], |row| row.get(0))
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        assert!(requests.len() <= OWNER_WAKES_PER_TICK_MAX);
        for agent in requests {
            self.connection
                .execute("DELETE FROM team_wake_requests WHERE agent=?1", [agent])
                .map_err(error)?;
            self.start_again(agent, None, access)?;
        }
        Ok(())
    }

    /// Starts a team agent the owner restarted or woke again, when the team plan and the
    /// pool still allow it; otherwise notes why not. A stopped or failed sub-agent whose
    /// worktree is still recorded wakes through [`TeamRuntime::wake`] under its own id, name,
    /// worktree and register entry; no other sub-agent starts again, so a register entry
    /// never moves to another agent. A restarted master starts as a fresh process under its
    /// register entry. A Wake (no `again`) of an agent that is no longer finished does
    /// nothing, so a stale Wake never stops or replaces a running agent. A restart a
    /// projects home move holds back waits and is tried again on each tick until it starts.
    fn start_again(
        &mut self,
        reference: i64,
        again: Option<&HostedRestart>,
        access: &ModelAccess,
    ) -> Result<(), EngineError> {
        let row: Option<(String, String, String, bool)> = self
            .connection
            .query_row(
                "SELECT a.project,a.role,a.harness,
                        a.status IN ('stopped','failed') AND EXISTS(SELECT 1 FROM team_worktrees w WHERE w.agent=a.id AND w.project=a.project)
                 FROM team_agents a WHERE a.id=?1",
                [reference],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((project, role, harness, wakes)) = row else {
            return Ok(());
        };
        let (Ok(project), Some(role)) = (ProjectId::new(project), AgentRole::parse(&role)) else {
            return Ok(());
        };
        match (role, wakes, again) {
            (AgentRole::Sub, false, None) | (AgentRole::Master, _, None) => return Ok(()),
            (AgentRole::Sub, false, Some(_)) => {
                return self.note(
                    &project,
                    "That sub-agent was not restarted: only a stopped agent whose worktree is still kept starts again, under its own name.",
                );
            }
            _ => {}
        }
        let Some(stored) = self.stored(&project)? else {
            return Ok(());
        };
        if super::paused(&self.connection, &project)? {
            return self.note(
                &project,
                "The agent was not restarted because the team is paused. Resume the team, then restart it.",
            );
        }
        let subs_max = usize::from(stored.team.max_agents.saturating_sub(1));
        let (harness, tools) = match role {
            AgentRole::Master if self.master(&project).is_some() => {
                return self.note(
                    &project,
                    "The master is already running, so no second master was started.",
                );
            }
            AgentRole::Master => (stored.team.master.clone(), stored.team.master_tools),
            AgentRole::Sub => match super::slot(&stored.team, &harness) {
                None => {
                    return self.note(
                        &project,
                        "That sub-agent's harness is no longer in the team plan, so it was not restarted.",
                    );
                }
                Some(slot)
                    if self.live(&project, AgentRole::Sub, Some(&slot.harness))
                        >= usize::from(slot.agents)
                        || self.live(&project, AgentRole::Sub, None) >= subs_max =>
                {
                    return self.note(
                        &project,
                        "The team already runs as many sub-agents as its plan allows, so no other was started.",
                    );
                }
                Some(slot) => (slot.harness.clone(), slot.tools),
            },
        };
        let paused = self.paused()?;
        let started = match self.account_for(access, &project, &harness, &paused) {
            Ok(account) if role == AgentRole::Sub => {
                match self.wake(&stored, reference, &harness, tools, &account, again)? {
                    Ok((id, resumed)) => {
                        self.owner_woke(&project, id, resumed, again.is_some())?;
                        Ok(id)
                    }
                    Err(reason) => Err(reason),
                }
            }
            Ok(account) => match self.spawn_run(&stored, role, &harness, tools, &account, again)? {
                Start::Started(id) => Ok(id),
                Start::Held => {
                    self.held_restarts.extend(again.cloned());
                    return Ok(());
                }
                Start::Refused(reason) => Err(reason),
            },
            Err(reason) => Err(reason),
        };
        match started {
            Ok(_) => Ok(()),
            Err(reason) => self.note(
                &project,
                &format!("The agent could not restart: {}.", bound_text(&reason, 300)),
            ),
        }
    }

    /// After the owner's Wake or restart starts a sub-agent again with no task (WK-R4): when
    /// its harness conversation could not continue, keeps its last result for its next task's
    /// prompt, as a master's wake gives it at once, and tells the master the owner woke or
    /// restarted it. Takes the project, the agent, whether its conversation continues and
    /// whether it was a restart. Returns any SQLite failure.
    fn owner_woke(
        &mut self,
        project: &ProjectId,
        id: i64,
        resumed: bool,
        restarted: bool,
    ) -> Result<(), EngineError> {
        let handoff = if resumed {
            None
        } else {
            last_result(&self.connection, id)?
        };
        if let Some(worker) = self.workers.get_mut(&id) {
            worker.handoff = handoff;
        }
        super::events::record(
            &self.connection,
            project,
            id,
            None,
            super::events::EventKind::Progress,
            if restarted {
                OWNER_RESTARTED
            } else {
                OWNER_WOKE
            },
            now_ms(),
        )?;
        Ok(())
    }

    /// Tells whether any agent process is still running.
    pub fn has_live_agents(&self) -> bool {
        !self.workers.is_empty()
    }

    /// Ends the engine's hold on every agent as the engine stops and returns running tasks
    /// to the queue. An agent in a pane a later engine can take over keeps running there with
    /// its turn stopped, so agent sessions outlive the engine; every other agent process group
    /// stops. Waits a bounded time for each agent thread. Returns the panes left running.
    pub fn shutdown(&mut self) -> Result<Vec<AgentId>, EngineError> {
        let mut kept = BTreeSet::new();
        for (id, worker) in &self.workers {
            if !worker.stopping && self.has_pane_record(*id)? {
                kept.insert(*id);
            }
        }
        for (id, worker) in &mut self.workers {
            worker.leave.store(kept.contains(id), Ordering::SeqCst);
            worker.cancel.store(true, Ordering::SeqCst);
            let _ = worker.orders.send(Order::Stop);
            worker.stopping = true;
        }
        let deadline = Instant::now() + JOIN_WAIT;
        for worker in self.workers.values_mut() {
            if let Some(thread) = worker.thread.take() {
                while !thread.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                if thread.is_finished() {
                    let _ = thread.join();
                }
            }
        }
        let now = now_ms();
        let ids: Vec<i64> = self.workers.keys().copied().collect();
        self.workers.clear();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        for id in ids {
            tx.execute(
                "UPDATE team_tasks SET status='queued', agent=NULL WHERE agent=?1 AND status='running' AND cancel_requested=0",
                [id],
            )
            .map_err(error)?;
            let note = kept.contains(&id).then_some(LEFT_RUNNING_NOTE);
            tx.execute(
                "UPDATE team_agents SET status='stopped', task=NULL, error=?3, stopped_ms=?2 WHERE id=?1 AND status IN ('starting','idle','working')",
                params![id, now, note],
            )
            .map_err(error)?;
            if !kept.contains(&id) {
                tx.execute("DELETE FROM team_panes WHERE agent=?1", [id])
                    .map_err(error)?;
            }
        }
        tx.execute(
            "UPDATE team_messages SET state='pending' WHERE state='sent'",
            [],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(kept
            .into_iter()
            .filter_map(|id| AgentId::team(id).ok())
            .collect())
    }

    /// Tells whether a later engine could take over the agent's pane from its stored record.
    fn has_pane_record(&self, id: i64) -> Result<bool, EngineError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM team_panes WHERE agent=?1)",
                [id],
                |row| row.get(0),
            )
            .map_err(error)
    }

    fn active_projects(&self) -> Result<Vec<ProjectId>, EngineError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT project FROM team_messages WHERE state='pending'
                 UNION SELECT project FROM team_tasks WHERE status IN ('queued','needs_pool','running') OR (delivered=0 AND source='master' AND status IN ('done','failed','cancelled')
                     AND (session IS NULL OR session IN (SELECT id FROM master_sessions WHERE phase='configured')))
                 UNION SELECT project FROM team_settings WHERE keep_warm=1
                 UNION SELECT project FROM team_agents WHERE status IN ('starting','idle','working')
                 UNION SELECT project FROM team_live_masters WHERE live=1",
            )
            .map_err(error)?;
        let mut projects: BTreeSet<ProjectId> = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(error)?
            .filter_map(|row| row.ok().and_then(|p| ProjectId::new(p).ok()))
            .collect();
        projects.extend(self.workers.values().map(|w| w.project.clone()));
        Ok(projects.into_iter().collect())
    }

    fn stored(&mut self, project: &ProjectId) -> Result<Option<StoredSetup>, EngineError> {
        let tx = self.connection.transaction().map_err(error)?;
        let stored = load_setup(&tx, project)?;
        tx.commit().map_err(error)?;
        Ok(stored)
    }

    fn paused(&self) -> Result<BTreeSet<CredentialRef>, EngineError> {
        let mut statement = self
            .connection
            .prepare("SELECT connection FROM team_paused_accounts WHERE until_ms > ?1")
            .map_err(error)?;
        let paused = statement
            .query_map([now_ms()], |row| row.get::<_, String>(0))
            .map_err(error)?
            .filter_map(|row| row.ok().and_then(|c| CredentialRef::new(c).ok()))
            .collect();
        Ok(paused)
    }

    fn step(&mut self, project: &ProjectId, access: &ModelAccess) -> Result<(), EngineError> {
        let Some(stored) = self.stored(project)? else {
            self.hold(project);
            return Ok(());
        };
        if super::paused(&self.connection, project)? {
            self.hold(project);
            return Ok(());
        }
        if crate::projects_home::move_holds(&self.connection, &stored.project_folder())
            .map_err(error)?
        {
            return Ok(());
        }
        let paused = self.paused()?;
        self.retire_rejected(project, access);
        self.retire_narrowed(&stored);
        self.sync_pool(&stored, access)?;
        self.request_cancels(project)?;
        self.keep_live(&stored, access, &paused)?;
        self.serve_live_calls(&stored, access)?;
        self.feed_master(&stored, access, &paused)?;
        self.deliver(&stored)?;
        let master_live = self.master(project).is_some();
        super::events::deliver_by_message(&self.connection, project, master_live, now_ms())?;
        self.dispatch(&stored, access, &paused)?;
        self.keep_warm(&stored, access, &paused)?;
        self.reap_idle(&stored)
    }

    fn sync_pool(&mut self, stored: &StoredSetup, access: &ModelAccess) -> Result<(), EngineError> {
        for harness in super::plan_harnesses(&stored.team) {
            let Some(launch) = launch_harness(&harness) else {
                continue;
            };
            let usable = matches!(
                eligible_accounts(access, &stored.project_id, launch),
                Accounts::Usable(accounts) if accounts.iter().any(|a| self.refusal(access, a).is_none())
            );
            let (from, to) = if usable {
                ("needs_pool", "queued")
            } else {
                ("queued", "needs_pool")
            };
            self.connection
                .execute(
                    "UPDATE team_tasks SET status=?3 WHERE project=?1 AND harness=?2 AND status=?4",
                    params![stored.project_id.as_str(), harness, to, from],
                )
                .map_err(error)?;
        }
        Ok(())
    }

    /// Stops every agent of a paused project; their work goes back to wait for the resume.
    fn hold(&mut self, project: &ProjectId) {
        for worker in self.workers.values_mut() {
            if &worker.project == project && !worker.held {
                worker.held = true;
                worker.stopping = true;
                worker.cancel.store(true, Ordering::SeqCst);
                let _ = worker.orders.send(Order::Stop);
            }
        }
    }

    /// Returns a held agent's work to its queue without spending an attempt: a task waits
    /// again (or is cancelled when the owner asked for that) and a message waits again.
    fn release(&mut self, busy: Busy) -> Result<(), EngineError> {
        match busy {
            Busy::Task(task) => {
                self.connection
                    .execute(
                        "UPDATE team_tasks SET status='queued', agent=NULL, attempts=MAX(attempts-1,0)
                         WHERE id=?1 AND status='running' AND cancel_requested=0",
                        [task],
                    )
                    .map_err(error)?;
                self.connection
                    .execute(
                        "UPDATE team_tasks SET status='cancelled', error='Cancelled by you', finished_ms=?2
                         WHERE id=?1 AND status='running'",
                        params![task, now_ms()],
                    )
                    .map_err(error)?;
            }
            Busy::Message(message) => {
                self.connection
                    .execute(
                        "UPDATE team_messages SET state='pending' WHERE id=?1 AND state='sent'",
                        [message],
                    )
                    .map_err(error)?;
            }
        }
        Ok(())
    }

    fn request_cancels(&mut self, project: &ProjectId) -> Result<(), EngineError> {
        let mut statement = self
            .connection
            .prepare("SELECT agent FROM team_tasks WHERE project=?1 AND status='running' AND cancel_requested=1 AND agent IS NOT NULL")
            .map_err(error)?;
        let agents: Vec<i64> = statement
            .query_map([project.as_str()], |row| row.get(0))
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?;
        for agent in agents {
            if let Some(worker) = self.workers.get(&agent) {
                worker.cancel.store(true, Ordering::SeqCst);
            }
        }
        Ok(())
    }

    fn live(&self, project: &ProjectId, role: AgentRole, harness: Option<&str>) -> usize {
        self.workers
            .values()
            .filter(|w| {
                &w.project == project
                    && w.role == role
                    && (!w.stopping || w.restarting)
                    && harness.is_none_or(|h| w.harness == h)
            })
            .count()
    }

    /// Counts a project's agents that may still hold a terminal, stopping ones included.
    fn panes(&self, project: &ProjectId) -> usize {
        self.workers
            .values()
            .filter(|w| &w.project == project)
            .count()
    }

    fn load(&self, connection: &CredentialRef) -> usize {
        self.workers
            .values()
            .filter(|w| &w.connection == connection && !w.stopping)
            .count()
    }

    fn account_for(
        &self,
        access: &ModelAccess,
        project: &ProjectId,
        harness: &str,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<TeamAccount, String> {
        let Some(launch) = launch_harness(harness) else {
            return Err(format!("{harness} cannot run team agents yet"));
        };
        let accounts = eligible_accounts(access, project, launch);
        if accounts != Accounts::NoPool && !self.executables.contains_key(launch) {
            return Err(format!("{harness} is not installed"));
        }
        let Accounts::Usable(fitting) = accounts else {
            return Err("Needs a pool".into());
        };
        if fitting.is_empty() {
            return Err(format!(
                "No account in this project's pool can run {harness}"
            ));
        }
        let mut refused = Vec::new();
        let accounts: Vec<TeamAccount> = fitting
            .into_iter()
            .filter(|account| match self.refusal(access, account) {
                None => true,
                Some(code) => {
                    refused.push(code);
                    false
                }
            })
            .collect();
        if accounts.is_empty() {
            assert!(!refused.is_empty());
            if refused.iter().all(|code| code == SIGN_IN_REJECTED) {
                return Err(format!(
                    "Sign in again: the provider rejected the sign-in of every account in this project's pool that can run {harness}"
                ));
            }
            return Err(format!(
                "Sign in to an account in this project's pool to run {harness}"
            ));
        }
        let pools: Vec<&CredentialRef> = access
            .pools
            .iter()
            .filter(|pool| {
                pool.projects
                    .iter()
                    .any(|id| id.as_str() == project.as_str())
            })
            .map(|pool| &pool.id)
            .collect();
        let choice = crate::scheduling::Choice::read(
            &self.connection,
            access,
            Some(project.as_str()),
            &pools,
            super::now_ms(),
        )
        .map_err(|_| "Could not read the account scheduling settings".to_owned())?;
        if let Some(account) = pick_account(
            &accounts,
            &|c: &CredentialRef| self.load(c),
            paused,
            &choice,
        ) {
            return Ok(account.clone());
        }
        if accounts
            .iter()
            .any(|account| !paused.contains(&account.connection))
        {
            return Err("Every account in this project's pool is at its usage limit, held in reserve or set to Never use".into());
        }
        Err("Every account in this project's pool is resting after a rate limit".into())
    }

    /// Tells why an account cannot take agent work now: endpoints always can, managed
    /// profiles only while their sign-in passes the master's check for this engine run.
    /// Takes the live access and the account. Returns the refusal code, or none.
    fn refusal(&self, access: &ModelAccess, account: &TeamAccount) -> Option<String> {
        match account.reach {
            TeamReach::Endpoint { .. } => None,
            TeamReach::Managed { .. } => self.sign_in(access, &account.connection).err(),
        }
    }

    /// Stops the project's idle agents whose account's sign-in the provider rejected, so
    /// the next message or task starts on another pool account instead of failing on it.
    /// Takes the project and the live access. Busy agents finish their turn first.
    fn retire_rejected(&mut self, project: &ProjectId, access: &ModelAccess) {
        let rejected: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, w)| &w.project == project && w.managed && w.busy.is_none() && !w.stopping)
            .filter(|(_, w)| {
                self.sign_in(access, &w.connection)
                    .is_err_and(|code| code == SIGN_IN_REJECTED)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in rejected {
            let worker = self.workers.get_mut(&id).expect("worker exists");
            worker.stopping = true;
            let _ = worker.orders.send(Order::Stop);
        }
    }

    /// Stops the project's agents whose running process holds more than the owner saved now.
    /// At a wider access level (AL-2) a kept or live agent stops at once and a per-turn agent
    /// while its turn runs, since an idle one reads the level again in its next grant. With a
    /// toolchain the saved policy no longer allows any agent stops, since its sandbox profile
    /// was built once at its start; a pane whose record kept no toolchains counts as holding
    /// every family. A stopped agent is held, so its task or message waits again without a
    /// spent attempt, and every next start reads the policy again, so it starts
    /// with the new level and without that toolchain. Each agent is checked once per saved
    /// policy. An unreadable policy stops every agent holding more than Sandboxed with no
    /// toolchain, and a project folder that toolchain detection cannot read stops every agent
    /// holding a toolchain the owner made no choice for, each under its own reason.
    /// Takes the project's stored setup.
    fn retire_narrowed(&mut self, stored: &StoredSetup) {
        let project = &stored.project_id;
        let saved = crate::sandbox::policy::load(&self.connection, project.as_str()).ok();
        let stale: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, w)| &w.project == project && !w.stopping)
            .filter(|(_, w)| saved.is_none() || w.checked != saved)
            .map(|(id, _)| *id)
            .collect();
        if stale.is_empty() {
            return;
        }
        let root = stored.project_folder().canonicalize().ok();
        for id in stale {
            let worker = self.workers.get_mut(&id).expect("worker exists");
            let running = !worker.managed || worker.live || worker.busy.is_some();
            let narrowed = crate::sandbox::policy::narrowing(
                saved.as_ref(),
                root.as_deref(),
                running.then_some(worker.access),
                worker.toolchains.as_deref().unwrap_or(&Toolchain::ALL),
            )
            .map(|cause| match cause {
                Narrowing::TurnedOff | Narrowing::NotAllowed if worker.toolchains.is_none() => {
                    Narrowed::Unrecorded
                }
                cause => Narrowed::Saved(cause),
            });
            let Some(narrowed) = narrowed else {
                worker.checked = saved.clone();
                continue;
            };
            worker.stopping = true;
            worker.held = true;
            worker.narrowed = Some(narrowed);
            worker.cancel.store(true, Ordering::SeqCst);
            let _ = worker.orders.send(Order::Stop);
        }
    }

    /// Proves a managed pool account is signed in for this engine run, and that the
    /// provider did not reject that sign-in, with the same check every master turn uses.
    /// Takes the live access and the pool member.
    /// Returns the active host and the sign-in, or a plain refusal code.
    fn sign_in(
        &self,
        access: &ModelAccess,
        account: &CredentialRef,
    ) -> Result<(HostStamp, NativeAuthFact), String> {
        let connection = access
            .connections
            .iter()
            .find(|c| &c.id == account)
            .ok_or("team_account_left_pool")?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(|_| "team_state_unavailable")?;
        let host = tx
            .query_row(
                "SELECT db_instance,engine_epoch FROM owner_meta WHERE singleton=1 AND schema_version=1 AND runtime_state='active'",
                [],
                |row| {
                    Ok(HostStamp {
                        db_instance: row.get(0)?,
                        engine_epoch: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(|_| "team_state_unavailable")?
            .ok_or("team_engine_not_ready")?;
        let fact = crate::storage::native_sign_in(&tx, &host, connection, None).map_err(|e| e.0)?;
        if crate::model_gateway::sign_in_rejected(&tx, connection).map_err(|e| e.0)? {
            return Err(SIGN_IN_REJECTED.into());
        }
        drop(tx);
        assert_eq!(fact.binding.engine_epoch, host.engine_epoch);
        Ok((host, fact))
    }

    /// Takes a managed agent's profile for its next turn under the engine's custody lock
    /// and reads the project's launch context for the fresh process.
    /// Returns none while another process holds the profile, so the turn waits.
    fn grant(
        &mut self,
        access: &ModelAccess,
        id: i64,
        project: &ProjectId,
    ) -> Result<Option<Box<Grant>>, String> {
        let connection = self.workers[&id].connection.clone();
        let (host, fact) = match self.sign_in(access, &connection) {
            Ok(signed) => signed,
            Err(code) if code == "native_profile_in_use" => return Ok(None),
            Err(code) => return Err(sign_in_problem(&code)),
        };
        let generation = self.workers[&id].grants + 1;
        let operation = RequestId::new(format!("team-agent-{id}-turn-{generation}"))
            .map_err(|e| with_code("the turn could not be named", Some(&e.0)))?;
        let custody = self
            .custody
            .as_mut()
            .map_err(|e| with_code("the account custody is not available", Some(&e.0)))?;
        let profile = match custody.acquire_use(
            &host,
            &fact.binding,
            &operation,
            generation,
            NativeProfileUsePurpose::Execution,
        ) {
            Ok(profile) => profile,
            Err(problem) if problem.0 == "native_profile_in_use" => return Ok(None),
            Err(problem) => {
                return Err(with_code(
                    "the account could not be handed to the agent",
                    Some(&problem.0),
                ));
            }
        };
        let layout = self
            .agent_layout(project, id)
            .map_err(|_| with_code("the agent worktree record could not be read", None))?;
        let context = self.context(
            project,
            id,
            self.workers[&id].role,
            self.workers[&id].tools,
            layout,
        )?;
        let access = crate::sandbox::policy::load(&self.connection, project.as_str())
            .map_err(|_| with_code("the project's agent access could not be read", None))?
            .access;
        if let Some(reason) = access.unavailable() {
            return Err(with_code(reason, Some("agent_access_unavailable")));
        }
        let worker = self.workers.get_mut(&id).expect("worker exists");
        worker.grants = generation;
        worker.access = access;
        Ok(Some(Box::new(Grant {
            profile,
            context,
            access,
        })))
    }

    fn note(&mut self, project: &ProjectId, text: &str) -> Result<(), EngineError> {
        let tx = self.connection.transaction().map_err(error)?;
        let _ = append_message(
            &tx,
            project,
            MessageAuthor::Engine,
            text,
            MessageState::Note,
            now_ms(),
        )?;
        tx.commit().map_err(error)
    }

    fn master(&self, project: &ProjectId) -> Option<i64> {
        self.workers
            .iter()
            .find(|(_, w)| &w.project == project && w.role == AgentRole::Master && !w.stopping)
            .map(|(id, _)| *id)
    }

    /// Sends a project's oldest pending message to its master, starting a per-message master
    /// when none runs. Nothing is sent while a master of the project is still stopping or
    /// restarting, as after the live switch goes off, so the project never runs two masters
    /// at once. Takes the setup, live access and paused accounts.
    fn feed_master(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        if super::master::live(&self.connection, project)? {
            return self.feed_live(project);
        }
        let settling = self.workers.values().any(|w| {
            &w.project == project && w.role == AgentRole::Master && (w.restarting || w.stopping)
        });
        let master = self.master(project);
        if settling || master.is_some_and(|id| self.workers[&id].busy.is_some()) {
            return Ok(());
        }
        let pending: Option<(i64, String, String)> = self
            .connection
            .query_row(
                "SELECT id,author,text FROM team_messages WHERE project=?1 AND state='pending' ORDER BY id LIMIT 1",
                [project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((message, author, text)) = pending else {
            return Ok(());
        };
        let (id, fresh) = match master {
            Some(id) => (id, false),
            None if self.panes(project) >= PANES_PER_PROJECT_MAX => return Ok(()),
            None => {
                let harness = stored.team.master.clone();
                let started = match self.account_for(access, project, &harness, paused) {
                    Ok(account) => match self.spawn(
                        stored,
                        AgentRole::Master,
                        &harness,
                        stored.team.master_tools,
                        &account,
                    )? {
                        Start::Started(id) => Ok(id),
                        Start::Held => return Ok(()),
                        Start::Refused(reason) => Err(reason),
                    },
                    Err(reason) => Err(reason),
                };
                match started {
                    Ok(id) => (id, true),
                    Err(reason) => {
                        self.connection
                            .execute(
                                "UPDATE team_messages SET state='failed' WHERE id=?1",
                                [message],
                            )
                            .map_err(error)?;
                        return self.note(
                            project,
                            &full_stop(format!("The master could not start: {reason}")),
                        );
                    }
                }
            }
        };
        if !fresh && self.end_session(id)? {
            return Ok(());
        }
        let grant = if self.workers[&id].managed {
            match self.grant(access, id, project) {
                Ok(Some(grant)) => Some(grant),
                Ok(None) => return Ok(()),
                Err(reason) => {
                    let worker = self.workers.get_mut(&id).expect("master worker exists");
                    worker.stopping = true;
                    let _ = worker.orders.send(Order::Stop);
                    self.connection
                        .execute(
                            "UPDATE team_messages SET state='failed' WHERE id=?1",
                            [message],
                        )
                        .map_err(error)?;
                    return self.note(
                        project,
                        &full_stop(format!(
                            "The master could not start: {}",
                            bound_text(&reason, 300)
                        )),
                    );
                }
            }
        } else {
            None
        };
        let fresh = fresh || grant.is_some();
        let speaker = if author == "owner" {
            "Message from the owner"
        } else {
            "Message from the engine"
        };
        let mut prompt = String::new();
        let team_tool = self.workers[&id].team_tool;
        let finished = super::events::finished_agents(&self.connection, project, now_ms())?;
        let told: BTreeSet<i64> = finished.iter().map(|(agent, _)| *agent).collect();
        let lines: Vec<String> = finished.into_iter().map(|(_, line)| line).collect();
        if !fresh && !told.is_subset(&self.workers[&id].told_finished) {
            prompt.push_str(&wake_text(&lines));
        }
        if fresh {
            prompt.push_str(&framing(
                stored,
                team_tool,
                &super::available_harnesses(access, project),
                &lines,
            ));
            let history =
                conversation_before(&self.connection, project, message, HISTORY_BYTES_MAX)?;
            if !history.is_empty() {
                prompt.push_str("Conversation so far:\n");
                prompt.push_str(&history);
                prompt.push('\n');
            }
        }
        let (heard, newest) = super::direct::notes(&self.connection, project)?;
        prompt.push_str(&heard);
        prompt.push_str(&format!("{speaker}:\n{text}"));
        if team_tool {
            tool::clear(&self.tool_record(id));
        }
        let bound = turn_bound(
            super::time_limits(&self.connection, project)?,
            self.workers[&id].ran(),
        );
        let worker = self.workers.get_mut(&id).expect("master worker exists");
        if worker
            .orders
            .send(Order::Turn(prompt, grant, bound.map(|(limit, _)| limit)))
            .is_err()
        {
            return Ok(());
        }
        worker.bound = bound.map(|(_, by)| by);
        worker.told_finished = told;
        worker.busy = Some(Busy::Message(message));
        self.connection
            .execute(
                "UPDATE team_messages SET state='sent' WHERE id=?1",
                [message],
            )
            .map_err(error)?;
        if let Some(newest) = newest {
            super::direct::delivered(&self.connection, project, newest)?;
        }
        self.mark_agent(id, AgentStatus::Working, None)
    }

    /// Keeps a project's live master running while its switch is on: starts it on a ready
    /// pool account of the master harness, briefs it once its thread waits, and stops a
    /// master whose mode or session no longer matches the switch. Nothing starts while a
    /// master of the project is still stopping or restarting, so the next start finds it
    /// stopped and starts it again in its own worktree and home. A master that cannot start
    /// leaves one team note per distinct reason. Takes the setup, live access and paused
    /// accounts.
    fn keep_live(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let on = super::master::live(&self.connection, project)?;
        if self.workers.values().any(|w| {
            &w.project == project && w.role == AgentRole::Master && (w.restarting || w.stopping)
        }) {
            return Ok(());
        }
        let session = crate::master_sessions::live_session(&self.connection, project)?;
        if let Some(id) = self.master(project) {
            let worker = self.workers.get_mut(&id).expect("master worker exists");
            let moved = on && worker.live && worker.session != session;
            if worker.live != on || moved {
                if worker.busy.is_none() {
                    worker.stopping = true;
                    let _ = worker.orders.send(Order::Stop);
                }
                if moved {
                    self.live_starts.remove(project);
                }
                return Ok(());
            }
            if on && !worker.briefed {
                return self.brief(stored, access, id);
            }
            return Ok(());
        }
        let harness = stored.team.master.clone();
        let live_capable = launch_harness(&harness)
            .is_some_and(|launch| self.live_master(project, AgentRole::Master, launch));
        if !on || !live_capable || self.panes(project) >= PANES_PER_PROJECT_MAX {
            return Ok(());
        }
        let starts = self.live_starts.entry(project.clone()).or_default();
        starts.retain(|at| at.elapsed() < LIVE_STARTS_WINDOW);
        if starts.len() >= LIVE_STARTS_MAX {
            return self.live_off(
                project,
                &format!(
                    "it had to start {LIVE_STARTS_MAX} times within {} minutes",
                    LIVE_STARTS_WINDOW.as_secs() / 60
                ),
            );
        }
        starts.push(Instant::now());
        let started = match self.account_for(access, project, &harness, paused) {
            Ok(account) => self.start_master(stored, &harness, &account, session.as_deref())?,
            Err(reason) => Start::Refused(reason),
        };
        match started {
            Start::Started((id, note)) => {
                super::master::problem(&self.connection, project, None)?;
                if let Some(note) = note {
                    self.note(project, &note)?;
                }
                self.brief(stored, access, id)
            }
            Start::Held => {
                if let Some(starts) = self.live_starts.get_mut(project) {
                    starts.pop();
                }
                Ok(())
            }
            Start::Refused(reason) => self.live_off(
                project,
                &format!("it could not start: {}", bound_text(&reason, 300)),
            ),
        }
    }

    /// Starts a project's live master for its live session. A session whose harness
    /// conversation can continue starts the master that kept it, in its worktree and home,
    /// resuming that conversation; otherwise the project's last master on this harness
    /// starts again in its own worktree and home, so nothing it left there is lost, or a new
    /// one starts. A session with earlier turns that cannot continue gets a fresh harness
    /// conversation seeded with them, and its note says so. A session whose conversation ran
    /// on another harness follows the owner's On harness switch setting: Carry over seeds
    /// the new conversation with a bounded handoff, Start fresh seeds nothing, and either
    /// note marks the switch. Takes the setup, the master harness, the account and the live
    /// session. Returns the agent id with the note to show, Held while a projects home move
    /// holds the project folder, or the plain reason it could not start.
    fn start_master(
        &mut self,
        stored: &StoredSetup,
        harness: &str,
        account: &TeamAccount,
        session: Option<&str>,
    ) -> Result<Start<(i64, Option<String>)>, EngineError> {
        let project = &stored.project_id;
        let tools = stored.team.master_tools;
        let launch = launch_harness(harness).unwrap_or(harness);
        let session = session.map(SessionId::new).transpose().map_err(error)?;
        let switched = match &session {
            Some(session) => {
                crate::master_sessions::switched_from(&self.connection, project, session, launch)?
            }
            None => None,
        };
        let mut switch_note = None;
        let mut switch_mode = None;
        let mut plan = match (&session, switched) {
            (Some(session), Some(from)) => {
                let to = crate::master_sessions::harness_label(launch);
                let mode = super::on_harness_switch(&self.connection, project)?;
                switch_mode = Some(mode);
                match mode {
                    super::SwitchMode::Carry => {
                        let kept = super::master::session_conversation(
                            &self.connection,
                            project,
                            session.as_str(),
                        )?;
                        ResumePlan::Fresh {
                            seed: crate::master_sessions::handoff(
                                &self.connection,
                                project,
                                session,
                                kept.as_ref(),
                                crate::master_sessions::harness_label(&from),
                            )?,
                            note: crate::master_sessions::switched_note(to, mode),
                        }
                    }
                    super::SwitchMode::Fresh => {
                        switch_note = Some(crate::master_sessions::switched_note(to, mode));
                        ResumePlan::New
                    }
                }
            }
            (Some(session), None)
                if crate::master_sessions::fresh_after_switch(
                    &self.connection,
                    project,
                    session,
                    launch,
                )? =>
            {
                ResumePlan::New
            }
            (Some(session), None) => crate::master_sessions::resume_harness(
                &self.connection,
                project,
                session,
                launch,
                account.connection.as_str(),
            )?,
            (None, _) => ResumePlan::New,
        };
        if let ResumePlan::Resume {
            agent,
            conversation,
            record,
        } = &plan
        {
            let resume = Resume {
                id: conversation.clone(),
                record: record.clone(),
            };
            self.resuming = Some((*agent, resume));
            let revived = self.revive(
                stored,
                *agent,
                AgentRole::Master,
                harness,
                tools,
                account,
                None,
            );
            self.resuming = None;
            if let Ok(id) = revived? {
                assert_eq!(id, *agent);
                return Ok(Start::Started(self.seat_session(
                    id,
                    session.as_ref(),
                    None,
                )));
            }
            let session = session.as_ref().expect("only a session resumes");
            let kept =
                super::master::session_conversation(&self.connection, project, session.as_str())?;
            plan = crate::master_sessions::fresh_harness(
                &self.connection,
                project,
                session,
                kept.as_ref(),
            )?;
        }
        let seeded = match plan {
            ResumePlan::Fresh { seed, note } => Some((seed, note)),
            ResumePlan::New => None,
            ResumePlan::Resume { .. } => unreachable!("a resume plan was handled above"),
        };
        let last = last_master(&self.connection, project, harness)?;
        let started = match last {
            Some(last) => {
                match self.revive(
                    stored,
                    last,
                    AgentRole::Master,
                    harness,
                    tools,
                    account,
                    None,
                )? {
                    Ok(id) => {
                        assert_eq!(id, last);
                        Some(id)
                    }
                    Err(_) => None,
                }
            }
            None => None,
        };
        let started = match started {
            Some(id) => id,
            None => match self.spawn(stored, AgentRole::Master, harness, tools, account)? {
                Start::Started(id) => id,
                Start::Held => return Ok(Start::Held),
                Start::Refused(reason) => return Ok(Start::Refused(reason)),
            },
        };
        if let (Some(session), Some(mode)) = (&session, switch_mode) {
            super::record_switch(&self.connection, project, session.as_str(), launch, mode)?;
        }
        let (id, note) = self.seat_session(started, session.as_ref(), seeded);
        Ok(Start::Started((id, note.or(switch_note))))
    }

    /// Binds a live master that just started to the session it runs, with the seed its
    /// briefing carries for a fresh conversation of a session with earlier turns. Takes the
    /// agent, the session and the seed with its note. Returns the agent and the note.
    fn seat_session(
        &mut self,
        id: i64,
        session: Option<&SessionId>,
        seeded: Option<(String, String)>,
    ) -> (i64, Option<String>) {
        let worker = self
            .workers
            .get_mut(&id)
            .expect("a started master has a worker");
        assert!(worker.live && !worker.briefed);
        worker.session = session.map(|session| session.as_str().to_owned());
        let (seed, note) = seeded.unzip();
        worker.seed = seed;
        (id, note)
    }

    /// Turns a project's live master off after it could not keep running, so the runtime
    /// starts it again only once the owner turns it on, and says why in one team note and
    /// the master's problem line. Takes the project and the reason. Returns any SQLite failure.
    fn live_off(&mut self, project: &ProjectId, reason: &str) -> Result<(), EngineError> {
        super::master::set_live(&self.connection, project, false)?;
        self.live_starts.remove(project);
        let line = full_stop(format!(
            "{LIVE_STOPPED}: {reason}. Start it again from the master view"
        ));
        if super::master::problem(&self.connection, project, Some(&line))? {
            self.note(project, &line)?;
        }
        Ok(())
    }

    /// Starts a live master that waits for its start: hands it the instructions its harness
    /// reads at launch as system context, never as a typed message, with the seed of a fresh
    /// conversation for a session with earlier turns, and the signed-in account's profile on
    /// a managed account. The master then waits for the owner's first line. Takes the setup,
    /// live access and the agent. Returns once sent; a profile still in use waits for the
    /// next tick.
    fn brief(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        id: i64,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let grant = if self.workers[&id].managed {
            match self.grant(access, id, project) {
                Ok(Some(grant)) => Some(grant),
                Ok(None) => return Ok(()),
                Err(reason) => {
                    let worker = self.workers.get_mut(&id).expect("master worker exists");
                    worker.stopping = true;
                    let _ = worker.orders.send(Order::Stop);
                    return self.note(
                        project,
                        &full_stop(format!(
                            "The live master could not start: {}",
                            bound_text(&reason, 300)
                        )),
                    );
                }
            }
        } else {
            None
        };
        let mut instructions = live_instructions(stored, self.workers[&id].team_tool);
        let worker = self.workers.get_mut(&id).expect("master worker exists");
        if let Some(seed) = worker.seed.take() {
            assert!(seed.len() <= crate::master_sessions::SEED_BYTES_MAX);
            instructions.push_str("\n\n");
            instructions.push_str(&seed);
        }
        assert!(instructions.len() <= crate::launch_context::LIVE_INSTRUCTIONS_BYTES_MAX);
        if worker
            .orders
            .send(Order::Start(instructions, grant))
            .is_ok()
        {
            worker.briefed = true;
        }
        Ok(())
    }

    /// Answers the team calls a project's live master made since the last tick.
    /// Takes the setup and live access. Returns any SQLite failure.
    fn serve_live_calls(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
    ) -> Result<(), EngineError> {
        let Some(id) = self.master(&stored.project_id) else {
            return Ok(());
        };
        if !self.workers[&id].live || !self.workers[&id].team_tool {
            return Ok(());
        }
        let mailbox = self.mailbox(id);
        let answering = super::master::Answering {
            access,
            stored,
            landing: &self.root,
            events: self.events.as_ref(),
            host: self.terminal.as_deref(),
        };
        super::master::serve_calls(&mut self.connection, &answering, &mailbox).map(drop)
    }

    /// Types the oldest waiting team message into a project's live master once it is idle
    /// and the owner is not typing, with the owner's prompts to sub-agents it has not heard.
    /// Takes the project. Returns any SQLite failure; a message that cannot be typed is
    /// failed with a team note, one that must wait stays pending.
    fn feed_live(&mut self, project: &ProjectId) -> Result<(), EngineError> {
        let Some(id) = self.master(project) else {
            return Ok(());
        };
        let Some(host) = self.terminal.clone() else {
            return Ok(());
        };
        if !self.workers[&id].live || !self.workers[&id].briefed {
            return Ok(());
        }
        let pending: Option<(i64, String, String)> = self
            .connection
            .query_row(
                "SELECT id,author,text FROM team_messages WHERE project=?1 AND state='pending' ORDER BY id LIMIT 1",
                [project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((message, author, text)) = pending else {
            return Ok(());
        };
        let (heard, newest) = super::direct::notes(&self.connection, project)?;
        let (typed, noted) = live_input(&heard, &author, &text);
        let newest = newest.filter(|_| noted);
        let state = match super::master::master_input(
            &self.connection,
            host.as_ref(),
            self.agents.as_deref().map(Agents::typing),
            project,
            &typed,
        ) {
            Ok(()) => "answered",
            Err(super::master::InputRefused::Invalid) => "failed",
            Err(_) => return Ok(()),
        };
        self.connection
            .execute(
                "UPDATE team_messages SET state=?2 WHERE id=?1",
                params![message, state],
            )
            .map_err(error)?;
        if state == "failed" {
            return self.note(
                project,
                "A team message could not be typed into the master because it is empty or not plain text.",
            );
        }
        if let Some(newest) = newest {
            super::direct::delivered(&self.connection, project, newest)?;
        }
        Ok(())
    }

    /// Hands the tasks a tools-off live master wrote as reply lines to the team, each
    /// through `admit` like any master task; refused ones are listed in one team note.
    /// Takes the project and the turn's reply text. Returns any SQLite failure.
    fn admit_reply_lines(&mut self, project: &ProjectId, text: &str) -> Result<(), EngineError> {
        let proposed = parse_delegations(text);
        if proposed.is_empty() {
            return Ok(());
        }
        let (Some(stored), Some(access)) = (self.stored(project)?, (self.access)()) else {
            return Ok(());
        };
        let now = now_ms();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let mut refused = Vec::new();
        for delegation in &proposed {
            if let Err(code) = admit(
                &tx,
                &access,
                &stored,
                delegation.harness.as_deref(),
                &delegation.prompt,
                TaskSource::Master,
                None,
                now,
            )? {
                refused.push(format!(
                    "- {}: {}",
                    bound_text(&delegation.prompt, 200),
                    refusal(code)
                ));
            }
        }
        if !refused.is_empty() {
            let _ = append_message(
                &tx,
                project,
                MessageAuthor::Engine,
                &format!(
                    "The live master's tasks were not handed off:\n{}",
                    refused.join("\n")
                ),
                MessageState::Note,
                now,
            )?;
        }
        tx.commit().map_err(error)
    }

    fn mark_agent(
        &mut self,
        id: i64,
        status: AgentStatus,
        task: Option<i64>,
    ) -> Result<(), EngineError> {
        self.connection
            .execute(
                "UPDATE team_agents SET status=?2, task=?3, heartbeat_ms=?4 WHERE id=?1",
                params![id, status.name(), task, now_ms()],
            )
            .map_err(error)?;
        Ok(())
    }

    fn deliver(&mut self, stored: &StoredSetup) -> Result<(), EngineError> {
        let project = &stored.project_id;
        if self
            .master(project)
            .is_some_and(|id| self.workers[&id].busy.is_some())
        {
            return Ok(());
        }
        let pending: i64 = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM team_messages WHERE project=?1 AND state IN ('pending','sent')",
                [project.as_str()],
                |row| row.get(0),
            )
            .map_err(error)?;
        if pending > 0 {
            return Ok(());
        }
        let live = crate::master_sessions::live_session(&self.connection, project)?;
        let ready: Vec<ReadyTask> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT t.id,t.harness,t.status,t.result,t.error FROM team_tasks t
                     WHERE t.project=?1 AND t.delivered=0 AND t.source='master' AND t.status IN ('done','failed','cancelled')
                     AND (t.session IS NULL OR t.session IS ?2)
                     AND NOT EXISTS (SELECT 1 FROM team_tasks o WHERE o.project=t.project AND o.turn IS t.turn AND t.turn IS NOT NULL
                                     AND o.status IN ('queued','needs_pool','running'))
                     ORDER BY t.id LIMIT 16",
                )
                .map_err(error)?;
            statement
                .query_map(params![project.as_str(), live], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        if ready.is_empty() {
            return Ok(());
        }
        let (text, held) = results_message(&ready);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        if append_message(
            &tx,
            project,
            MessageAuthor::Engine,
            &text,
            MessageState::Pending,
            now_ms(),
        )?
        .is_ok()
        {
            for (id, ..) in &ready[..held] {
                tx.execute("UPDATE team_tasks SET delivered=1 WHERE id=?1", [id])
                    .map_err(error)?;
            }
        }
        tx.commit().map_err(error)
    }

    fn dispatch(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let queued: Vec<(i64, String, String, Option<i64>, bool)> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT t.id,t.harness,t.prompt,p.agent,t.source='master' FROM team_tasks t
                     LEFT JOIN team_task_pins p ON p.task=t.id AND p.project=t.project
                     WHERE t.project=?1 AND t.status='queued' ORDER BY t.id LIMIT 64",
                )
                .map_err(error)?;
            statement
                .query_map([project.as_str()], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        let subs_max = usize::from(stored.team.max_agents.saturating_sub(1));
        for (task, harness, prompt, pin, by_master) in queued {
            let slot = if stored.team.sub_harnesses.is_empty() {
                super::slot(&stored.team, &harness)
            } else {
                stored
                    .team
                    .sub_harnesses
                    .iter()
                    .find(|s| s.harness == harness)
                    .cloned()
            };
            let Some(slot) = slot else {
                self.fail_task(task, "The team plan no longer has this harness")?;
                continue;
            };
            let mut ended = 0;
            let idle = loop {
                let found = self
                    .workers
                    .iter()
                    .find(|(id, w)| {
                        &w.project == project
                            && w.role == AgentRole::Sub
                            && w.harness == harness
                            && w.busy.is_none()
                            && !w.stopping
                            && !paused.contains(&w.connection)
                            && pin.is_none_or(|pin| pin == **id)
                    })
                    .map(|(id, _)| *id);
                match found {
                    Some(agent) if self.end_session(agent)? => ended += 1,
                    found => break found,
                }
                assert!(ended <= self.workers.len());
            };
            let mut woken_fresh = false;
            let agent = match (idle, pin) {
                (Some(agent), _) => agent,
                (None, Some(pinned)) if self.workers.contains_key(&pinned) => continue,
                (None, Some(pinned))
                    if by_master && super::owner_stopped(&self.connection, pinned)? =>
                {
                    self.fail_task(task, OWNER_STOPPED_TASK)?;
                    continue;
                }
                (None, Some(pinned)) => {
                    if self.live(project, AgentRole::Sub, Some(&harness))
                        >= usize::from(slot.agents)
                        || self.live(project, AgentRole::Sub, None) >= subs_max
                    {
                        continue;
                    }
                    let Ok(account) = self.account_for(access, project, &harness, paused) else {
                        continue;
                    };
                    match self.wake(stored, pinned, &harness, slot.tools, &account, None)? {
                        Ok((agent, resumed)) => {
                            woken_fresh = !resumed;
                            agent
                        }
                        Err(problem) => {
                            self.fail_task(
                                task,
                                &format!(
                                    "The sub-agent could not start again in its worktree: {}",
                                    bound_text(&problem, 300)
                                ),
                            )?;
                            continue;
                        }
                    }
                }
                (None, None) => {
                    if self.live(project, AgentRole::Sub, Some(&harness))
                        >= usize::from(slot.agents)
                        || self.live(project, AgentRole::Sub, None) >= subs_max
                        || self.panes(project) >= PANES_PER_PROJECT_MAX
                    {
                        continue;
                    }
                    let Ok(account) = self.account_for(access, project, &harness, paused) else {
                        continue;
                    };
                    match self.spawn(stored, AgentRole::Sub, &harness, slot.tools, &account)? {
                        Start::Started(agent) => {
                            assert!(self.live(project, AgentRole::Sub, None) <= subs_max);
                            agent
                        }
                        Start::Held => continue,
                        Start::Refused(problem) => {
                            self.fail_task(
                                task,
                                &format!(
                                    "The sub-agent could not start: {}",
                                    bound_text(&problem, 300)
                                ),
                            )?;
                            continue;
                        }
                    }
                }
            };
            let grant = if self.workers[&agent].managed {
                match self.grant(access, agent, project) {
                    Ok(Some(grant)) => Some(grant),
                    Ok(None) => continue,
                    Err(reason) => {
                        let worker = self.workers.get_mut(&agent).expect("worker exists");
                        worker.stopping = true;
                        let _ = worker.orders.send(Order::Stop);
                        self.fail_task(
                            task,
                            &format!(
                                "The sub-agent could not start: {}",
                                bound_text(&reason, 300)
                            ),
                        )?;
                        continue;
                    }
                }
            } else {
                None
            };
            let worktree: Option<String> = self
                .connection
                .query_row(
                    "SELECT worktree FROM team_agents WHERE id=?1",
                    [agent],
                    |row| row.get(0),
                )
                .map_err(error)?;
            let folder = stored.project_folder();
            let handoff = if woken_fresh {
                last_result(&self.connection, agent)?
            } else {
                self.workers
                    .get_mut(&agent)
                    .and_then(|worker| worker.handoff.take())
            };
            let earlier = handoff
                .map(|last| {
                    format!(
                        "You worked in this worktree before and stopped. Your last result then: {last}\n"
                    )
                })
                .unwrap_or_default();
            let layout = self
                .agent_layout(&stored.project_id, agent)
                .map_err(error)?;
            let prompt = sub_agent_prompt(
                &stored.name,
                &worktree.map_or(folder.clone(), |worktree| folder.join(worktree)),
                &earlier,
                &prompt,
                layout,
            );
            let bound = turn_bound(
                super::time_limits(&self.connection, &stored.project_id)?,
                self.workers[&agent].ran(),
            );
            let worker = self.workers.get_mut(&agent).expect("worker exists");
            if worker
                .orders
                .send(Order::Turn(prompt, grant, bound.map(|(limit, _)| limit)))
                .is_err()
            {
                continue;
            }
            worker.bound = bound.map(|(_, by)| by);
            worker.busy = Some(Busy::Task(task));
            self.connection
                .execute(
                    "UPDATE team_tasks SET status='running', agent=?2, attempts=attempts+1, started_ms=?3, error=NULL WHERE id=?1",
                    params![task, agent, now_ms()],
                )
                .map_err(error)?;
            self.connection
                .execute(
                    "UPDATE team_agents SET session=(SELECT session FROM team_tasks WHERE id=?1) WHERE id=?2 AND session IS NULL",
                    params![task, agent],
                )
                .map_err(error)?;
            names::assign_task_role(&self.connection, project, agent, task)?;
            self.mark_agent(agent, AgentStatus::Working, Some(task))?;
        }
        Ok(())
    }

    fn keep_warm(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        if !super::keep_warm(&self.connection, project)? {
            return Ok(());
        }
        let floor = usize::from(stored.team.min_agents.saturating_sub(1));
        for slot in &stored.team.sub_harnesses {
            while self.live(project, AgentRole::Sub, None) < floor
                && self.live(project, AgentRole::Sub, Some(&slot.harness))
                    < usize::from(slot.agents)
                && self.panes(project) < PANES_PER_PROJECT_MAX
            {
                let Ok(account) = self.account_for(access, project, &slot.harness, paused) else {
                    break;
                };
                match self.spawn(stored, AgentRole::Sub, &slot.harness, slot.tools, &account)? {
                    Start::Started(agent) => {
                        self.workers.get_mut(&agent).expect("worker exists").warm = true;
                    }
                    Start::Held => return Ok(()),
                    Start::Refused(problem) => {
                        self.connection
                            .execute(
                                "UPDATE team_settings SET keep_warm=0 WHERE project=?1",
                                [project.as_str()],
                            )
                            .map_err(error)?;
                        return self.note(
                        project,
                        &full_stop(format!(
                            "Keeping sub-agents warm was turned off because a sub-agent could not start: {}",
                            bound_text(&problem, 300)
                        )),
                    );
                    }
                }
            }
        }
        Ok(())
    }

    /// Stops an agent past its session limit, so its next turn goes to a fresh agent. The
    /// session counts from the agent's start and the limit is read now; a live master is never
    /// stopped by it. Takes the agent. Returns whether it was stopped, or a failed read.
    fn end_session(&mut self, id: i64) -> Result<bool, EngineError> {
        let worker = &self.workers[&id];
        if worker.live {
            return Ok(false);
        }
        let session = super::time_limits(&self.connection, &worker.project)?.session_time();
        if session.is_none_or(|limit| worker.ran() < limit) {
            return Ok(false);
        }
        let worker = self.workers.get_mut(&id).expect("worker exists");
        worker.stopping = true;
        let _ = worker.orders.send(Order::Stop);
        Ok(true)
    }

    /// Stops agents idle past their bound, except the sub-agents the owner keeps warm and,
    /// with the owner's idle stop Off, every idle sub-agent. An agent past its session limit
    /// stops even when kept warm. A master is never idle while its project has work in
    /// flight, so its idle time counts from the moment the last of that work ended and its
    /// session is ended at its next turn. Takes the project. Returns any SQLite failure.
    fn reap_idle(&mut self, stored: &StoredSetup) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let warm = super::keep_warm(&self.connection, project)?;
        let floor = usize::from(stored.team.min_agents.saturating_sub(1));
        let mut live_subs = self.live(project, AgentRole::Sub, None);
        let ids: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, w)| &w.project == project && w.busy.is_none() && !w.stopping && !w.live)
            .map(|(id, _)| *id)
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        let masters = ids
            .iter()
            .any(|id| self.workers[id].role == AgentRole::Master);
        let in_flight = masters && self.work_in_flight(project)?;
        let limits = super::time_limits(&self.connection, project)?;
        for id in ids {
            let worker = self.workers.get_mut(&id).expect("worker exists");
            if worker.role == AgentRole::Master && in_flight {
                worker.idle_since = Instant::now();
                continue;
            }
            let ended = limits
                .session_time()
                .is_some_and(|session| worker.ran() >= session);
            if !ended {
                let limit = match worker.role {
                    AgentRole::Master => self.limits.master_idle_stop,
                    AgentRole::Sub => match limits.idle_time() {
                        Some(limit) => limit,
                        None => continue,
                    },
                };
                if worker.idle_since.elapsed() < limit
                    || (worker.role == AgentRole::Sub && warm && live_subs <= floor)
                {
                    continue;
                }
            }
            if worker.role == AgentRole::Sub {
                live_subs -= 1;
            }
            worker.stopping = true;
            let _ = worker.orders.send(Order::Stop);
        }
        Ok(())
    }

    /// Tells whether a project's master still has work in flight: a task queued, waiting
    /// for a pool account or running, or a message waiting for the master. Takes the
    /// project. Returns the answer, or any SQLite failure.
    fn work_in_flight(&self, project: &ProjectId) -> Result<bool, EngineError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM team_tasks WHERE project=?1 AND status IN ('queued','needs_pool','running'))
                 OR EXISTS(SELECT 1 FROM team_messages WHERE project=?1 AND state IN ('pending','sent') AND author<>'master')",
                [project.as_str()],
                |row| row.get(0),
            )
            .map_err(error)
    }

    fn fail_task(&mut self, task: i64, reason: &str) -> Result<(), EngineError> {
        self.connection
            .execute(
                "UPDATE team_tasks SET status='failed', agent=NULL, error=?2, finished_ms=?3 WHERE id=?1",
                params![task, reason, now_ms()],
            )
            .map_err(error)?;
        Ok(())
    }

    fn spawn(
        &mut self,
        stored: &StoredSetup,
        role: AgentRole,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
    ) -> Result<Start<i64>, EngineError> {
        self.spawn_run(stored, role, harness, tools, account, None)
    }

    /// Records and launches one new team agent, unless the project already runs the most
    /// panes or a projects home move holds its folder: the hold is checked in the same write
    /// transaction that records the agent, so a move planned first holds the start back.
    /// Takes the setup, the role, harness, tools switch, account and any register restart.
    /// Returns the started agent, Held, or the plain reason it could not start.
    fn spawn_run(
        &mut self,
        stored: &StoredSetup,
        role: AgentRole,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
        again: Option<&HostedRestart>,
    ) -> Result<Start<i64>, EngineError> {
        if self.panes(&stored.project_id) >= PANES_PER_PROJECT_MAX {
            return Ok(Start::Refused(format!(
                "the project already runs {PANES_PER_PROJECT_MAX} agents, the most one project may run at once"
            )));
        }
        let now = now_ms();
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(error)?;
        if crate::projects_home::move_holds(&tx, &stored.project_folder()).map_err(error)? {
            return Ok(Start::Held);
        }
        tx.execute(
            "INSERT INTO team_agents(project,role,harness,tools,connection,status,created_ms,heartbeat_ms) VALUES (?1,?2,?3,?4,?5,'starting',?6,?6)",
            params![stored.project_id.as_str(), role.name(), harness, tools, account.connection.as_str(), now],
        )
        .map_err(error)?;
        let id = tx.last_insert_rowid();
        tx.commit().map_err(error)?;
        assert!(id > 0);
        if role == AgentRole::Sub {
            names::assign_persona(&self.connection, &stored.project_id, id)?;
        }
        Ok(
            match self.launch(stored, id, role, harness, tools, account, again, None)? {
                Ok(agent) => Start::Started(agent),
                Err(reason) => Start::Refused(reason),
            },
        )
    }

    /// Wakes a stopped sub-agent (DEC-102), the one way a task pinned to it, the master's
    /// wake and the owner's Wake or restart start one again: the agent starts under its own
    /// id, name and recorded worktree, and under its own newest register entry when no
    /// restart names one, continuing the harness conversation kept for it when
    /// [`TeamRuntime::kept_conversation`] finds one that can continue. Takes the setup, the
    /// agent, its harness and tools switch, the account and the register restart if any.
    /// Returns the agent with whether its harness conversation continues, or the plain
    /// reason it could not start.
    fn wake(
        &mut self,
        stored: &StoredSetup,
        id: i64,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
        again: Option<&HostedRestart>,
    ) -> Result<Result<(i64, bool), String>, EngineError> {
        let own = match (again, &self.agents) {
            (None, Some(_)) => {
                crate::agents::hosted_entries(&self.connection, &stored.project_id, TEAM_HOST)
                    .map_err(error)?
                    .remove(&id.to_string())
                    .map(|entry| HostedRestart {
                        id: entry,
                        host: TEAM_HOST.to_owned(),
                        reference: id.to_string(),
                    })
            }
            _ => None,
        };
        let again = again.or(own.as_ref());
        let resume = self.kept_conversation(id, harness, &account.connection)?;
        let resuming = resume.is_some();
        self.resuming = resume.map(|resume| (id, resume));
        let revived = self.revive(stored, id, AgentRole::Sub, harness, tools, account, again);
        self.resuming = None;
        Ok(revived?.map(|agent| {
            assert_eq!(agent, id);
            (id, resuming)
        }))
    }

    /// The harness conversation a stopped sub-agent can continue: the one kept for it on
    /// the same harness and account, with an id the harness resumes by and its record still
    /// a plain file, while agents run in terminal panes. Takes the agent, its harness and
    /// the account. Returns the conversation, or none when it starts a fresh one.
    fn kept_conversation(
        &self,
        id: i64,
        harness: &str,
        account: &CredentialRef,
    ) -> Result<Option<Resume>, EngineError> {
        if self.terminal.is_none() {
            return Ok(None);
        }
        let launch = launch_harness(harness).unwrap_or(harness);
        let kept: Option<(String, String, String, String)> = self
            .connection
            .query_row(
                "SELECT harness,connection,conversation,record FROM team_agent_conversations WHERE agent=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(error)?;
        Ok(kept
            .filter(|(kept_harness, connection, conversation, record)| {
                kept_harness == launch
                    && connection == account.as_str()
                    && crate::harnesses::interactive::resumable(conversation)
                    && std::fs::symlink_metadata(record).is_ok_and(|meta| meta.is_file())
            })
            .map(|(_, _, conversation, record)| Resume {
                id: conversation,
                record: PathBuf::from(record),
            }))
    }

    /// Starts a stopped agent again under its own id in the worktree and home it left: a
    /// sub-agent through [`TeamRuntime::wake`], or a project's live master. Takes the setup,
    /// the agent, its role, harness and tools switch, the account to run on and the register
    /// restart if any. Returns its id, or the plain reason it could not start; the worktree
    /// must still be the one recorded when it first started.
    #[allow(clippy::too_many_arguments)]
    fn revive(
        &mut self,
        stored: &StoredSetup,
        id: i64,
        role: AgentRole,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
        again: Option<&HostedRestart>,
    ) -> Result<Result<i64, String>, EngineError> {
        if self.panes(&stored.project_id) >= PANES_PER_PROJECT_MAX {
            return Ok(Err(format!(
                "the project already runs {PANES_PER_PROJECT_MAX} agents, the most one project may run at once"
            )));
        }
        let kept: Option<(String, String, String)> = self
            .connection
            .query_row(
                "SELECT repository,worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
                params![id, stored.project_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((repository, worktree, git_dir)) = kept else {
            return Ok(Err("the agent's worktree is no longer known".into()));
        };
        let changed = self
            .connection
            .execute(
                "UPDATE team_agents SET status='starting', connection=?3, task=NULL, error=NULL, stopped_ms=NULL, turns=0, heartbeat_ms=?4
                 WHERE id=?1 AND project=?2 AND role=?5 AND status IN ('stopped','failed')",
                params![id, stored.project_id.as_str(), account.connection.as_str(), now_ms(), role.name()],
            )
            .map_err(error)?;
        if changed != 1 {
            return Ok(Err("the agent is not stopped".into()));
        }
        self.connection
            .execute("DELETE FROM team_owner_stops WHERE agent=?1", [id])
            .map_err(error)?;
        if role == AgentRole::Sub {
            names::reclaim(&self.connection, &stored.project_id, id)?;
        }
        let kept = [repository, worktree, git_dir];
        self.launch(stored, id, role, harness, tools, account, again, Some(kept))
    }

    /// Prepares and starts the agent of one `team_agents` row: a new agent in a new
    /// worktree, or with `kept` a revived one in the worktree recorded for it. Takes the
    /// setup, the row id, role, harness, tools switch, account, the register restart if any
    /// and the kept worktree. Returns the id, or the plain reason it failed, which the row
    /// then records.
    #[allow(clippy::too_many_arguments)]
    fn launch(
        &mut self,
        stored: &StoredSetup,
        id: i64,
        role: AgentRole,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
        again: Option<&HostedRestart>,
        kept: Option<[String; 3]>,
    ) -> Result<Result<i64, String>, EngineError> {
        let layout = match &kept {
            Some([_, worktree, git_dir]) => recorded_layout(worktree, git_dir),
            None => self.git_layout(&stored.project_id),
        };
        let prepared = self.prepare(
            stored,
            id,
            role,
            harness,
            tools,
            account,
            kept.is_some(),
            layout,
        );
        let resume = self
            .resuming
            .take()
            .filter(|(agent, _)| *agent == id)
            .map(|(_, resume)| resume);
        let (root, plan, mode) = match prepared {
            Ok((root, plan, mode)) => (
                root,
                Plan {
                    resume: resume.filter(|_| plan.live || role == AgentRole::Sub),
                    ..plan
                },
                mode,
            ),
            Err(problem) => {
                self.connection
                    .execute(
                        "UPDATE team_agents SET status='failed', error=?2, stopped_ms=?3 WHERE id=?1",
                        params![id, bound_text(&problem, 400), now_ms()],
                    )
                    .map_err(error)?;
                return Ok(Err(problem));
            }
        };
        let made =
            NewClone((kept.is_none() && layout == Layout::Clone).then(|| plan.working_dir.clone()));
        let worktree = (plan.working_dir != root)
            .then(|| {
                plan.working_dir
                    .strip_prefix(&root)
                    .ok()
                    .map(|p| p.display().to_string())
            })
            .flatten();
        let landing = match agent_git_dir(&root, &plan.working_dir, layout) {
            Ok(landing) if kept.as_ref().is_none_or(|kept| kept == &landing) => landing,
            Ok(_) => {
                let problem =
                    with_code("the agent worktree changed", Some("team_worktree_changed"));
                self.connection
                    .execute(
                        "UPDATE team_agents SET status='failed', error=?2, stopped_ms=?3 WHERE id=?1",
                        params![id, bound_text(&problem, 400), now_ms()],
                    )
                    .map_err(error)?;
                return Ok(Err(problem));
            }
            Err(problem) => {
                self.connection
                    .execute(
                        "UPDATE team_agents SET status='failed', error=?2, stopped_ms=?3 WHERE id=?1",
                        params![id, bound_text(&problem, 400), now_ms()],
                    )
                    .map_err(error)?;
                return Ok(Err(problem));
            }
        };
        self.connection
            .execute(
                "UPDATE team_agents SET worktree=?2 WHERE id=?1",
                params![id, worktree],
            )
            .map_err(error)?;
        self.connection
            .execute(
                "INSERT OR REPLACE INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                params![id, stored.project_id.as_str(), landing[0], landing[1], landing[2]],
            )
            .map_err(error)?;
        made.keep();
        if kept.is_none() {
            let paths = landing.each_ref().map(Path::new);
            let started = crate::landing::open(&self.root).and_then(|db| {
                crate::landing::record_worktree_start(&db, paths[0], paths[1], paths[2])
            });
            if let Err(problem) = started {
                let problem = with_code(
                    "the agent worktree's starting commit could not be recorded",
                    Some(&problem.0),
                );
                self.connection
                    .execute(
                        "UPDATE team_agents SET status='failed', error=?2, stopped_ms=?3 WHERE id=?1",
                        params![id, bound_text(&problem, 400), now_ms()],
                    )
                    .map_err(error)?;
                return Ok(Err(problem));
            }
        }
        let hosted = match self.host(stored, id, role, &plan.harness, &account.connection, again) {
            Ok(hosted) => hosted,
            Err(problem) => {
                self.connection
                    .execute(
                        "UPDATE team_agents SET status='failed', error=?2, stopped_ms=?3 WHERE id=?1",
                        params![id, bound_text(&problem, 400), now_ms()],
                    )
                    .map_err(error)?;
                return Ok(Err(problem));
            }
        };
        let managed = matches!(mode, Mode::PerTurn { .. });
        self.run_worker(
            id,
            Launch::Fresh(Box::new(plan), Box::new(mode)),
            Seat {
                project: stored.project_id.clone(),
                role,
                harness: harness.to_owned(),
                connection: account.connection.clone(),
                tools,
                hosted,
            },
        )?;
        if managed {
            self.mark_agent(id, AgentStatus::Idle, None)?;
        }
        Ok(Ok(id))
    }

    /// Starts the thread that runs one agent and records it as a live worker of its project.
    /// Takes the team agent id, how its harness starts and who it is in the team.
    /// Returns once the thread runs; the project then holds one more terminal at most.
    fn run_worker(&mut self, id: i64, launch: Launch, seat: Seat) -> Result<(), EngineError> {
        let interrupt = seat
            .hosted
            .as_ref()
            .map_or_else(|| Arc::new(AtomicBool::new(false)), Hosted::interrupt_flag);
        let (orders, inbox) = channel();
        let (outbox, reports) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let leave = Arc::new(AtomicBool::new(false));
        let (flag, kept) = (cancel.clone(), leave.clone());
        let managed = match &launch {
            Launch::Fresh(_, mode) => matches!(**mode, Mode::PerTurn { .. }),
            Launch::Adopt(adoption) => adoption.managed.is_some(),
        };
        let (live, briefed) = match &launch {
            Launch::Fresh(plan, _) => (plan.live, false),
            Launch::Adopt(adoption) => (adoption.live, adoption.managed.is_none()),
        };
        let (access, toolchains) = match &launch {
            Launch::Fresh(plan, _) => (plan.access, Some(plan.toolchains.clone())),
            Launch::Adopt(adoption) => (adoption.access, adoption.toolchains.clone()),
        };
        let thread = std::thread::Builder::new()
            .name(format!("team-agent-{id}"))
            .spawn(move || work(launch, inbox, outbox, flag, interrupt, kept))
            .map_err(error)?;
        let team_tool =
            seat.role == AgentRole::Master && seat.tools && self.delegation_tool.is_some();
        self.workers.insert(
            id,
            Worker {
                project: seat.project,
                role: seat.role,
                harness: seat.harness,
                connection: seat.connection,
                orders,
                reports,
                cancel,
                leave,
                busy: None,
                idle_since: Instant::now(),
                stopping: false,
                held: false,
                restarting: false,
                warm: false,
                worked: false,
                managed,
                tools: seat.tools,
                team_tool,
                grants: 0,
                thread: Some(thread),
                hosted: seat.hosted,
                live,
                briefed,
                turn_started: None,
                session: None,
                seed: None,
                bound: None,
                spawned: Instant::now(),
                handoff: None,
                access,
                ran_before: Duration::ZERO,
                told_finished: BTreeSet::new(),
                toolchains,
                checked: None,
                narrowed: None,
            },
        );
        assert!(self.panes(&self.workers[&id].project) <= PANES_PER_PROJECT_MAX);
        self.swept.get_or_insert_with(Instant::now);
        Ok(())
    }

    /// Where a new agent of a project keeps its git state (DEC-102): its own clone at the
    /// Own branch and Full access levels, a linked worktree at Sandboxed or when the saved
    /// level cannot be read (launch then refuses it). Input: the project. Output: the layout.
    /// Reads the git layout a started agent works in from the worktree record the engine
    /// wrote at its launch, never from the disk the agent can change. Takes the project and
    /// the agent id. Returns Clone when the recorded git folder is the agent's own clone's,
    /// Linked otherwise or with no record, or the database error.
    fn agent_layout(&self, project: &ProjectId, id: i64) -> rusqlite::Result<Layout> {
        let recorded: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
                params![id, project.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(recorded.map_or(Layout::Linked, |(worktree, git_dir)| {
            recorded_layout(&worktree, &git_dir)
        }))
    }

    fn git_layout(&self, project: &ProjectId) -> Layout {
        match crate::sandbox::policy::load(&self.connection, project.as_str()) {
            Ok(policy) if policy.access != AgentAccess::Sandboxed => Layout::Clone,
            _ => Layout::Linked,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare(
        &self,
        stored: &StoredSetup,
        id: i64,
        role: AgentRole,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
        adopt: bool,
        layout: Layout,
    ) -> Result<(PathBuf, Plan, Mode), String> {
        let launch = launch_harness(harness).ok_or("harness has no launch adapter")?;
        let executable = self
            .executables
            .get(launch)
            .ok_or("harness is not installed")?;
        if let Some(line) = confined_unavailable(std::env::consts::OS) {
            return Err(line.to_owned());
        }
        let sandboxed = |e: SandboxError| {
            with_code(
                "the agent sandbox could not be set up",
                Some(sandbox_code(&e)),
            )
        };
        let root = stored
            .project_folder()
            .canonicalize()
            .map_err(|e| with_code("the project folder could not be opened", Some(io_code(&e))))?;
        if root.starts_with(&self.state) {
            return Err("project folder overlaps engine state".into());
        }
        let agent = self.agent_folder(id);
        let own_home = agent_home(&self.state, id);
        assert_eq!(own_home.parent(), Some(agent.as_path()));
        let tmp = agent.join("tmp");
        for directory in [&agent, &own_home, &tmp] {
            private_directory(directory)
                .map_err(|e| with_code("the agent folder could not be made private", Some(&e.0)))?;
        }
        let (home, remote, model) = match &account.reach {
            TeamReach::Endpoint { base_url, model } => {
                let endpoint = ResearchAccount::Endpoint {
                    base_url: base_url.clone(),
                    model: model.clone(),
                };
                let remote = model_remote(&endpoint).map_err(|e| {
                    with_code("the endpoint address was refused", Some(run_code(&e)))
                })?;
                (own_home, remote, Some(model.clone()))
            }
            TeamReach::Managed { model, .. } => {
                (own_home, SandboxRemote::AnyHost(443), model.clone())
            }
        };
        let live = self.live_master(&stored.project_id, role, launch);
        if live && tools {
            private_directory(&tmp.join(tool::MAILBOX_FOLDER)).map_err(|e| {
                with_code("the master's tool mailbox could not be made", Some(&e.0))
            })?;
        }
        let context = self.context(&stored.project_id, id, role, tools, layout)?;
        let network = if tools {
            SandboxNetwork::Open
        } else {
            SandboxNetwork::Only(vec![remote])
        };
        let policy = crate::sandbox::policy::load(&self.connection, stored.project_id.as_str())
            .map_err(|_| with_code("the project's agent access could not be read", None))?;
        if let Some(reason) = policy.access.unavailable() {
            return Err(with_code(reason, Some("agent_access_unavailable")));
        }
        let homes = ToolHomes::owner().map_err(sandboxed)?;
        let scratch = toolchains::make_tmp(&home)
            .map_err(|e| with_code("the agent temp folder could not be made", Some(io_code(&e))))?;
        toolchains::prepare_cargo_home(&home, &homes)
            .map_err(|e| with_code("the agent cargo home could not be made", Some(io_code(&e))))?;
        let kept = if tools {
            (self.access)().map_or_else(Vec::new, |access| crate::sandbox::owner_ports(&access))
        } else {
            Vec::new()
        };
        let mut sandbox = ProjectSandbox::new(&root, &home, &tmp, network)
            .and_then(|sandbox| sandbox.keep_ports(&kept))
            .map_err(sandboxed)?;
        if let Some(runtime) = self.state.parent() {
            sandbox = sandbox.guard_engine(runtime).map_err(sandboxed)?;
        }
        let extra = crate::sandbox::extra_paths::load(&self.connection, stored.project_id.as_str())
            .map_err(|_| with_code("the project's read-only folders could not be read", None))?;
        let toolchains = policy.allowed(&root);
        (sandbox, _) = sandbox
            .allow_tools(&Toolchains::grant(&homes, &toolchains))
            .and_then(|sandbox| sandbox.allow_scratch(&scratch))
            .and_then(|sandbox| sandbox.allow_extra_reads(&extra))
            .map_err(sandboxed)?;
        let install = executable
            .canonicalize()
            .map_err(|e| with_code("the harness install could not be found", Some(io_code(&e))))?
            .parent()
            .map(Path::to_path_buf)
            .ok_or("harness install folder")?;
        sandbox = sandbox.allow_read(&install).map_err(sandboxed)?;
        if tools {
            for server in context.read_paths() {
                if let Some(folder) = server.parent() {
                    sandbox = sandbox.allow_read(folder).map_err(sandboxed)?;
                }
            }
        }
        let profile = self.state.join("profiles").join(format!("agent-{id}.sb"));
        sandbox.install(&profile).map_err(sandboxed)?;
        ensure_repository(&root, &tmp).map_err(|e| {
            with_code(
                "the project repository could not be prepared",
                Some(run_code(&e)),
            )
        })?;
        let worktree = crate::projects_home::state_folder(&root)
            .join("worktrees")
            .join(format!("agent-{id}"));
        let mut made = NewClone(None);
        match (adopt, layout) {
            (true, Layout::Linked) => {}
            (true, Layout::Clone) => crate::landing::ready_agent_clone(&worktree).map_err(|e| {
                with_code(
                    "the agent's own copy of the project could not be readied",
                    Some(&e.0),
                )
            })?,
            (false, Layout::Linked) => {
                add_worktree(&sandbox, &profile, &worktree).map_err(|e| {
                    with_code("the agent worktree could not be added", Some(run_code(&e)))
                })?
            }
            (false, Layout::Clone) => {
                let worktrees = worktree.parent().ok_or("agent worktrees folder")?;
                std::fs::create_dir_all(worktrees).map_err(|e| {
                    with_code(
                        "the agent worktrees folder could not be made",
                        Some(io_code(&e)),
                    )
                })?;
                crate::landing::add_agent_clone(&root, &worktree).map_err(|e| {
                    with_code(
                        "the agent's own copy of the project could not be made",
                        Some(&e.0),
                    )
                })?;
                made = NewClone(Some(worktree.clone()));
            }
        }
        let working_dir = worktree
            .canonicalize()
            .map_err(|e| with_code("the agent worktree could not be opened", Some(io_code(&e))))?;
        if working_dir != worktree {
            return Err("the agent worktree is not a plain folder in the project".into());
        }
        assert!(working_dir.starts_with(&root) && working_dir != root);
        confinement(layout, policy.access)?;
        sandbox = match layout {
            Layout::Linked => sandbox.confine_to_worktree(&working_dir),
            Layout::Clone => sandbox.confine_to_clone(&working_dir),
        }
        .map_err(sandboxed)?;
        if let Some(engine) = self
            .delegation_tool
            .as_ref()
            .filter(|_| role == AgentRole::Master && tools)
        {
            let changed = |e: EngineError| with_code("the delegate engine changed", Some(&e.0));
            if sandbox.writes(engine.verified_path().map_err(changed)?) {
                return Err(with_code(
                    "the delegate engine is in a folder the agent can write",
                    Some("team_delegate_writable"),
                ));
            }
        }
        sandbox.install(&profile).map_err(sandboxed)?;
        let plan = Plan {
            harness: launch.into(),
            executable: executable.to_path_buf(),
            working_dir,
            model,
            tools,
            sandbox_profile: profile,
            access: policy.access,
            toolchains,
            layout,
            git_identity: owner_git_identity(&root),
            limits: self.launch_limits(),
            pane: self.terminal.clone().zip(AgentId::team(id).ok()),
            live,
            resume: None,
            continues: role == AgentRole::Sub,
        };
        let mode = match &account.reach {
            TeamReach::Endpoint { base_url, .. } => Mode::Kept {
                account: AccountBinding::Url {
                    home,
                    base_url: base_url.clone(),
                    key: None,
                },
                context,
            },
            TeamReach::Managed { .. } => Mode::PerTurn { home },
        };
        made.keep();
        Ok((root, plan, mode))
    }

    /// The bounds of one agent process: the longest a launched harness may run. The owner's
    /// session limit is the engine's own clock on the agent, checked per turn and while it is
    /// idle, so it means the same for agents kept between turns and agents started for each
    /// turn. Returns the bounds.
    fn launch_limits(&self) -> LaunchLimits {
        LaunchLimits {
            run_time: RUN_TIME_MAX,
            stop_grace: self.limits.stop_grace,
            output_bytes: self.limits.output_bytes,
            events: SESSION_EVENTS_MAX,
        }
    }

    fn agent_folder(&self, id: i64) -> PathBuf {
        self.state.join("agents").join(format!("agent-{id}"))
    }

    fn tool_record(&self, id: i64) -> PathBuf {
        self.agent_folder(id).join("tmp").join(tool::DROP_FILE)
    }

    fn mailbox(&self, id: i64) -> PathBuf {
        self.agent_folder(id).join("tmp").join(tool::MAILBOX_FOLDER)
    }

    /// Tells whether a project's master runs live: its own app kept running in a pane the
    /// owner types into. Takes the project, the role and the launch harness. Returns true
    /// for a master while the project's live switch is on, the harness has an interactive
    /// app and the engine has a terminal host; a state read failure counts as off.
    fn live_master(&self, project: &ProjectId, role: AgentRole, harness: &str) -> bool {
        role == AgentRole::Master
            && self.terminal.is_some()
            && crate::harnesses::interactive::support(harness)
                == crate::harnesses::interactive::Support::Terminal
            && super::master::live(&self.connection, project).unwrap_or(false)
    }

    /// Reads what one team agent launch receives: the project context and approved
    /// memories, the approved skills and MCP servers with tools on, the engine's `delegate`
    /// tool for a master with tools on, served by the captured engine while the file at its
    /// path is still that one, and the team's skill and server names for a master with tools
    /// off. Takes the project, the agent id, its role, tools switch and git layout, which
    /// picks how its work lands.
    fn context(
        &self,
        project: &ProjectId,
        id: i64,
        role: AgentRole,
        tools: bool,
        layout: Layout,
    ) -> Result<AgentContext, String> {
        let unread =
            |e: LaunchError| with_code("the project context could not be read", Some(&e.0));
        let context = crate::launch_context::for_project(&self.connection, project, layout)
            .map_err(unread)?;
        let master = role == AgentRole::Master;
        let live = master
            && self
                .connection
                .query_row("SELECT harness FROM team_agents WHERE id=?1", [id], |row| {
                    row.get::<_, String>(0)
                })
                .ok()
                .and_then(|harness| launch_harness(&harness))
                .is_some_and(|harness| self.live_master(project, role, harness));
        match &self.delegation_tool {
            Some(engine) if master && tools && live => {
                let engine = engine
                    .verified_path()
                    .map_err(|e| with_code("the delegate engine changed", Some(&e.0)))?;
                context
                    .with_live_team_tool(engine, &self.mailbox(id))
                    .map_err(unread)
            }
            Some(engine) if master && tools => {
                let engine = engine
                    .verified_path()
                    .map_err(|e| with_code("the delegate engine changed", Some(&e.0)))?;
                context
                    .with_team_tool(engine, &self.tool_record(id))
                    .map_err(unread)
            }
            _ if master && !tools => context
                .with_team_index(&self.connection, project)
                .map_err(unread),
            _ => Ok(context),
        }
    }

    /// Records one team agent in the engine's agent register, continuing the register
    /// entry of an agent the owner restarted. Takes the setup, the team agent id, its role,
    /// launch harness, the account it signs in with and the restart. Returns the register
    /// handle, none without a register.
    fn host(
        &self,
        stored: &StoredSetup,
        id: i64,
        role: AgentRole,
        harness: &str,
        account: &CredentialRef,
        again: Option<&HostedRestart>,
    ) -> Result<Option<Hosted>, String> {
        let Some(agents) = &self.agents else {
            return Ok(None);
        };
        let (role, label) = match role {
            AgentRole::Master => (crate::agents::AgentRole::Master, "Team master".to_owned()),
            AgentRole::Sub => (
                crate::agents::AgentRole::SubAgent,
                format!("Team sub-agent {id}"),
            ),
        };
        let agent = HostedAgent {
            project: stored.project_id.clone(),
            role,
            label,
            harness: harness.to_owned(),
            host: TEAM_HOST,
            reference: again.map_or_else(|| id.to_string(), |restart| restart.reference.clone()),
            account: Some(account.clone()),
        };
        agents
            .host(agent, again.map(|restart| restart.id.as_str()))
            .map(Some)
            .map_err(|problem| with_code("the agent could not be recorded", Some(&problem.0)))
    }

    fn collect(&mut self) -> Result<(), EngineError> {
        let ids: Vec<i64> = self.workers.keys().copied().collect();
        for id in ids {
            while let Some(worker) = self.workers.get(&id) {
                let report = match worker.reports.try_recv() {
                    Ok(report) => report,
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => Report::Ended {
                        problem: Some("The agent stopped unexpectedly".into()),
                    },
                };
                if self.apply(id, report)? {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Counts the usage a piped agent's harness reported for a turn under the agent's
    /// project, account and agent row; a store that cannot be written leaves the turn
    /// uncounted and logs only the failure code. Inputs: the agent id and the tokens.
    fn count_usage(&self, id: i64, tokens: crate::usage::capture::Tokens) {
        let worker = &self.workers[&id];
        let attribution = crate::usage::service::team_attribution(
            id,
            worker.project.as_str(),
            worker.role == AgentRole::Master,
            &worker.harness,
            Some(worker.connection.as_str()),
            &(self.access)().unwrap_or_default(),
        );
        let counted = crate::usage::initialize(&self.connection).and_then(|()| {
            crate::usage::capture::count_reported(
                &self.connection,
                &attribution,
                tokens,
                "headless",
                now_ms(),
            )
        });
        if let Err(problem) = counted {
            eprintln!(
                "team usage not counted: {}",
                crate::usage::service::failure_code(&problem)
            );
        }
    }

    fn apply(&mut self, id: i64, report: Report) -> Result<bool, EngineError> {
        match report {
            Report::Ready { group, pane } => {
                if let Some(record) = &pane {
                    let worker = self.workers.get_mut(&id).expect("ready worker exists");
                    worker.ran_before = u64::try_from(now_ms()).map_or(Duration::ZERO, |now| {
                        Duration::from_millis(now.saturating_sub(record.since_ms))
                    });
                    worker.spawned = Instant::now();
                }
                let worker = &self.workers[&id];
                let (access, toolchains) = (Some(worker.access), worker.toolchains.clone());
                let pane = pane.map(|record| PaneRecord {
                    access,
                    toolchains,
                    ..record
                });
                if let Some(record) = pane.as_ref().and_then(PaneRecord::encode) {
                    self.connection
                        .execute(
                            "INSERT OR REPLACE INTO team_panes(agent,record) VALUES (?1,?2)",
                            params![id, record],
                        )
                        .map_err(error)?;
                }
                let busy = self.workers[&id].busy;
                if let Some(hosted) = &self.workers[&id].hosted {
                    hosted.started(group);
                }
                if self.workers[&id].live {
                    super::master::pane_ready(&self.connection, &self.workers[&id].project, id)?;
                }
                self.connection
                    .execute(
                        "UPDATE team_agents SET process_group=?2, status=?3, heartbeat_ms=?4 WHERE id=?1",
                        params![
                            id,
                            i64::from(group),
                            (if busy.is_some() { AgentStatus::Working } else { AgentStatus::Idle }).name(),
                            now_ms()
                        ],
                    )
                    .map_err(error)?;
                Ok(false)
            }
            Report::Progress { bytes } => {
                if let Some(Busy::Task(task)) = self.workers[&id].busy {
                    self.connection
                        .execute(
                            "UPDATE team_tasks SET output_bytes=?2 WHERE id=?1",
                            params![task, i64::try_from(bytes).unwrap_or(i64::MAX)],
                        )
                        .map_err(error)?;
                }
                self.connection
                    .execute(
                        "UPDATE team_agents SET heartbeat_ms=?2 WHERE id=?1",
                        params![id, now_ms()],
                    )
                    .map_err(error)?;
                Ok(false)
            }
            Report::Usage { tokens } => {
                self.count_usage(id, tokens);
                Ok(false)
            }
            Report::Conversation(resume) => {
                let worker = &self.workers[&id];
                if worker.role == AgentRole::Sub {
                    keep_agent_conversation(
                        &self.connection,
                        id,
                        &worker.project,
                        launch_harness(&worker.harness).unwrap_or(&worker.harness),
                        &worker.connection,
                        &resume,
                    )?;
                } else if worker.live
                    && let Some(session) = &worker.session
                {
                    let kept = super::master::SessionConversation {
                        agent: id,
                        harness: launch_harness(&worker.harness)
                            .unwrap_or(&worker.harness)
                            .to_owned(),
                        connection: worker.connection.as_str().to_owned(),
                        resume,
                    };
                    super::master::keep_conversation(
                        &self.connection,
                        &worker.project,
                        session,
                        &kept,
                    )?;
                }
                Ok(false)
            }
            Report::ResumeRefused => {
                let worker = self.workers.get_mut(&id).expect("worker exists");
                worker.stopping = true;
                if worker.role == AgentRole::Sub {
                    self.connection
                        .execute("DELETE FROM team_agent_conversations WHERE agent=?1", [id])
                        .map_err(error)?;
                }
                if let Some(session) = worker.session.clone() {
                    let project = worker.project.clone();
                    super::master::refuse_conversation(&self.connection, &project, &session)?;
                }
                Ok(false)
            }
            Report::Finished { text, end, cause } => {
                let worker = self.workers.get_mut(&id).expect("worker exists");
                let busy = worker.busy.take();
                let cut =
                    (end == TurnEnd::TimedOut).then(|| limit_note(cause.as_deref(), worker.bound));
                let cause = cut.or(cause);
                worker.bound = None;
                if let Some(hosted) = &worker.hosted {
                    hosted.text(&text);
                    hosted.set_busy(false);
                }
                worker.idle_since = Instant::now();
                worker.worked = true;
                let connection = worker.connection.clone();
                let project = worker.project.clone();
                let held = worker.held;
                let team_tool = worker.team_tool;
                if end == TurnEnd::RateLimited {
                    self.pause(&connection)?;
                }
                self.connection
                    .execute("UPDATE team_agents SET turns=turns+1 WHERE id=?1", [id])
                    .map_err(error)?;
                match busy {
                    Some(busy) if held && end != TurnEnd::Success => self.release(busy)?,
                    Some(Busy::Task(task)) => {
                        self.finish_task(task, &text, end, cause.as_deref())?
                    }
                    Some(Busy::Message(message)) => {
                        let proposed = if team_tool {
                            tool::take(&self.tool_record(id))
                        } else {
                            Vec::new()
                        };
                        self.finish_message(
                            &project,
                            message,
                            &text,
                            end,
                            cause.as_deref(),
                            proposed,
                        )?
                    }
                    None => {}
                }
                self.land(id, &project, busy, end)?;
                if !matches!(
                    end,
                    TurnEnd::Success | TurnEnd::Failed | TurnEnd::Interrupted
                ) {
                    self.workers.get_mut(&id).expect("worker exists").stopping = true;
                }
                self.mark_agent(id, AgentStatus::Idle, None)?;
                Ok(false)
            }
            Report::Turn { end: None, .. } => {
                let worker = self.workers.get_mut(&id).expect("worker exists");
                worker.turn_started = Some(now_ms());
                if let Some(hosted) = &worker.hosted {
                    hosted.set_busy(true);
                }
                super::master::turn_started(&self.connection, id)?;
                self.mark_agent(id, AgentStatus::Working, None)?;
                Ok(false)
            }
            Report::Turn {
                end: Some(end),
                text,
            } => {
                let worker = self.workers.get_mut(&id).expect("worker exists");
                let started = worker.turn_started.take();
                worker.idle_since = Instant::now();
                worker.worked = true;
                if let Some(hosted) = &worker.hosted {
                    hosted.text(&text);
                    hosted.set_busy(false);
                }
                let (project, connection, team_tool) = (
                    worker.project.clone(),
                    worker.connection.clone(),
                    worker.team_tool,
                );
                if end == TurnEnd::RateLimited {
                    self.pause(&connection)?;
                    self.workers.get_mut(&id).expect("worker exists").stopping = true;
                }
                let turn = super::master::turn_ended(
                    &self.connection,
                    &project,
                    id,
                    started,
                    now_ms(),
                    end.name(),
                )?;
                self.connection
                    .execute("UPDATE team_agents SET turns=turns+1 WHERE id=?1", [id])
                    .map_err(error)?;
                if !team_tool {
                    self.admit_reply_lines(&project, &text)?;
                }
                self.land_turn(
                    id,
                    &project,
                    format!("live-{turn}"),
                    format!("Master turn {turn}"),
                    "the live master's turn",
                    end,
                )?;
                self.mark_agent(id, AgentStatus::Idle, None)?;
                Ok(false)
            }
            Report::Ended { problem } => {
                let mut worker = self.workers.remove(&id).expect("worker exists");
                if worker.live {
                    super::master::pane_ended(&self.connection, &worker.project, id)?;
                }
                let _ = toolchains::remove_tmp(&self.agent_folder(id).join("home"));
                self.connection
                    .execute("DELETE FROM team_panes WHERE agent=?1", [id])
                    .map_err(error)?;
                if let Some(thread) = worker.thread.take() {
                    let deadline = Instant::now() + JOIN_WAIT;
                    while !thread.is_finished() && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    if thread.is_finished() {
                        let _ = thread.join();
                    }
                }
                match worker.busy {
                    Some(busy) if worker.held => self.release(busy)?,
                    Some(Busy::Task(task)) => self.finish_task(task, "", TurnEnd::Closed, None)?,
                    Some(Busy::Message(message)) => self.finish_message(
                        &worker.project,
                        message,
                        "",
                        TurnEnd::Closed,
                        None,
                        Vec::new(),
                    )?,
                    None => {}
                }
                self.land(id, &worker.project, worker.busy, TurnEnd::Closed)?;
                if worker.live
                    && let Some(started) = worker.turn_started.take()
                {
                    let turn = super::master::turn_ended(
                        &self.connection,
                        &worker.project,
                        id,
                        Some(started),
                        now_ms(),
                        TurnEnd::Closed.name(),
                    )?;
                    self.connection
                        .execute("UPDATE team_agents SET turns=turns+1 WHERE id=?1", [id])
                        .map_err(error)?;
                    self.land_turn(
                        id,
                        &worker.project,
                        format!("live-{turn}"),
                        format!("Master turn {turn}"),
                        "the live master's turn",
                        TurnEnd::Closed,
                    )?;
                }
                let status = if problem.is_some() && !worker.stopping {
                    AgentStatus::Failed
                } else {
                    AgentStatus::Stopped
                };
                let problem = problem.or_else(|| worker.narrowed.map(Narrowed::problem));
                if let Some(hosted) = worker.hosted.take() {
                    let state = if status == AgentStatus::Failed {
                        RunState::Failed
                    } else {
                        RunState::Stopped
                    };
                    hosted.finish(state, problem.as_deref());
                }
                self.connection
                    .execute(
                        "UPDATE team_agents SET status=?2, task=NULL, error=?3, stopped_ms=?4 WHERE id=?1",
                        params![id, status.name(), problem, now_ms()],
                    )
                    .map_err(error)?;
                if worker.live
                    && !worker.stopping
                    && super::master::live(&self.connection, &worker.project)?
                {
                    let reason = problem.as_deref().map_or_else(
                        || "its terminal ended".to_owned(),
                        |problem| format!("its terminal ended: {}", bound_text(problem, 300)),
                    );
                    self.live_off(&worker.project, &reason)?;
                }
                if status == AgentStatus::Failed
                    && worker.warm
                    && !worker.worked
                    && super::keep_warm(&self.connection, &worker.project)?
                {
                    self.connection
                        .execute(
                            "UPDATE team_settings SET keep_warm=0 WHERE project=?1",
                            [worker.project.as_str()],
                        )
                        .map_err(error)?;
                    self.note(
                        &worker.project,
                        &problem.as_deref().map_or_else(
                            || full_stop(WARM_STOPPED.to_owned()),
                            |problem| {
                                full_stop(format!("{WARM_STOPPED}: {}", bound_text(problem, 300)))
                            },
                        ),
                    )?;
                }
                Ok(true)
            }
        }
    }

    /// Lands what one agent's ended turn changed in its worktree (DEC-095): the engine
    /// commits it into that worktree's change set and the team chat says the edits wait in
    /// Changes, or why they could not be kept. Landing never fails the agent or its work.
    /// Inputs: the agent, its project, the work that ended and how. Output: any SQLite
    /// failure writing the note.
    fn land(
        &mut self,
        id: i64,
        project: &ProjectId,
        busy: Option<Busy>,
        end: TurnEnd,
    ) -> Result<(), EngineError> {
        let Some(busy) = busy else {
            return Ok(());
        };
        let prompt = |sql: &str, key: i64| -> Result<String, EngineError> {
            self.connection
                .query_row(sql, [key], |row| row.get::<_, String>(0))
                .optional()
                .map(Option::unwrap_or_default)
                .map_err(error)
        };
        let (turn, line, what) = match busy {
            Busy::Task(task) => (
                format!("task-{task}"),
                prompt("SELECT prompt FROM team_tasks WHERE id=?1", task)?,
                format!("task {task}"),
            ),
            Busy::Message(message) => (
                format!("message-{message}"),
                prompt("SELECT text FROM team_messages WHERE id=?1", message)?,
                "the team master's turn".to_owned(),
            ),
        };
        self.land_turn(id, project, turn, line, &what, end)
    }

    /// Lands one ended turn of an agent under its change-set turn key, as `land` does.
    /// Inputs: the agent, its project, the turn key, the prompt line, what ended for the
    /// note and how it ended. Output: any SQLite failure writing the note.
    fn land_turn(
        &mut self,
        id: i64,
        project: &ProjectId,
        turn: String,
        line: String,
        what: &str,
        end: TurnEnd,
    ) -> Result<(), EngineError> {
        type Row = (String, String, String, String, String);
        let row: Option<Row> = self
            .connection
            .query_row(
                "SELECT a.role,a.harness,w.repository,w.worktree,w.git_dir FROM team_worktrees w
                 JOIN team_agents a ON a.id=w.agent WHERE w.agent=?1 AND w.project=?2",
                params![id, project.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(error)?;
        let Some((role, harness, repository, worktree, git_dir)) = row else {
            return Ok(());
        };
        let harness = excerpt(&harness, 64);
        let seat = if AgentRole::parse(&role) == Some(AgentRole::Master) {
            "agent"
        } else {
            "sub-agent"
        };
        let what = format!("{what} ({harness}, {seat} {id})");
        let label = if AgentRole::parse(&role) == Some(AgentRole::Master) {
            format!("Team master ({harness})")
        } else {
            let name = names::name(&self.connection, id)?;
            format!(
                "{} \u{b7} {}",
                names::label(AgentRole::Sub, id, name.as_ref()),
                names::harness_name(&harness)
            )
        };
        let workspace = crate::landing::Workspace {
            project: project.clone(),
            agent: crate::landing::ChangeAgent {
                kind: crate::landing::AgentKind::Sub,
                id: id.to_string(),
                label,
            },
            repository: PathBuf::from(repository),
            worktree: PathBuf::from(worktree),
            git_dir: PathBuf::from(git_dir),
        };
        let task = turn
            .strip_prefix("task-")
            .and_then(|task| task.parse::<i64>().ok());
        let ended = crate::landing::TurnEnd {
            outcome: match end {
                TurnEnd::Success => crate::landing::TurnOutcome::Succeeded,
                TurnEnd::Cancelled | TurnEnd::Interrupted => crate::landing::TurnOutcome::Cancelled,
                _ => crate::landing::TurnOutcome::Failed,
            },
            turn,
            prompt_line: line.lines().next().unwrap_or_default().to_owned(),
        };
        let landed = crate::landing::open(&self.root)
            .and_then(|db| crate::landing_turns::record(&db, &workspace, &ended));
        let note = match landed {
            Ok(
                crate::landing_turns::Landed::Nothing | crate::landing_turns::Landed::Waiting(_),
            ) => {
                return Ok(());
            }
            Ok(crate::landing_turns::Landed::Added(_)) => {
                let note = format!("The edits from {what} are waiting for you in Changes.");
                if AgentRole::parse(&role) == Some(AgentRole::Sub) {
                    super::events::record(
                        &self.connection,
                        project,
                        id,
                        task,
                        super::events::EventKind::ChangeSetReady,
                        &note,
                        now_ms(),
                    )?;
                }
                note
            }
            Err(problem) => with_code(
                &format!(
                    "The edits from {what} could not be kept for review and stay only in its worktree"
                ),
                Some(&problem.0),
            ),
        };
        assert!(note.len() <= NOTE_BYTES_MAX);
        self.note(project, &note)
    }

    fn pause(&mut self, connection: &CredentialRef) -> Result<(), EngineError> {
        let until = now_ms().saturating_add(
            i64::try_from(self.limits.rate_limit_pause.as_millis()).unwrap_or(i64::MAX),
        );
        self.connection
            .execute(
                "INSERT INTO team_paused_accounts(connection,until_ms) VALUES (?1,?2)
                 ON CONFLICT(connection) DO UPDATE SET until_ms=excluded.until_ms",
                params![connection.as_str(), until],
            )
            .map_err(error)?;
        Ok(())
    }

    fn finish_task(
        &mut self,
        task: i64,
        text: &str,
        end: TurnEnd,
        cause: Option<&str>,
    ) -> Result<(), EngineError> {
        let Some(attempts): Option<i64> = self
            .connection
            .query_row(
                "SELECT attempts FROM team_tasks WHERE id=?1",
                [task],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?
        else {
            return Ok(());
        };
        let retry = attempts < self.limits.attempts_max;
        let closed = with_code("The sub-agent stopped unexpectedly", cause);
        let timed = format!("The sub-agent was {}", cause.unwrap_or(TIME_LIMIT_NOTE));
        let (status, problem) = match end {
            TurnEnd::Success => (TaskStatus::Done, None),
            TurnEnd::Cancelled => (TaskStatus::Cancelled, Some("Cancelled by you")),
            TurnEnd::Interrupted => (TaskStatus::Cancelled, Some("Interrupted by you")),
            TurnEnd::RateLimited => (
                if retry {
                    TaskStatus::Queued
                } else {
                    TaskStatus::Failed
                },
                Some("The account hit its rate limit"),
            ),
            TurnEnd::TimedOut => (TaskStatus::Failed, Some(timed.as_str())),
            TurnEnd::TooLong => (
                TaskStatus::Failed,
                Some("The sub-agent's output exceeded the limit"),
            ),
            TurnEnd::Failed => (
                if retry {
                    TaskStatus::Queued
                } else {
                    TaskStatus::Failed
                },
                Some("The sub-agent could not finish the task"),
            ),
            TurnEnd::Closed => (
                if retry {
                    TaskStatus::Queued
                } else {
                    TaskStatus::Failed
                },
                Some(closed.as_str()),
            ),
        };
        let result =
            (!text.trim().is_empty()).then(|| bound_text(text.trim(), super::RESULT_BYTES_MAX));
        let finished = status.finished().then(now_ms);
        let changed = self
            .connection
            .execute(
                "UPDATE team_tasks SET status=?2, agent=CASE WHEN ?2='queued' THEN NULL ELSE agent END, result=COALESCE(?3,result), error=?4, finished_ms=?5, output_bytes=?6 WHERE id=?1 AND status='running'",
                params![
                    task,
                    status.name(),
                    result,
                    problem,
                    finished,
                    i64::try_from(text.len()).unwrap_or(i64::MAX)
                ],
            )
            .map_err(error)?;
        assert!(changed <= 1);
        if changed == 1 {
            super::events::task_ended(&self.connection, task, now_ms())?;
        }
        Ok(())
    }

    fn finish_message(
        &mut self,
        project: &ProjectId,
        message: i64,
        text: &str,
        end: TurnEnd,
        cause: Option<&str>,
        proposed: Vec<Delegation>,
    ) -> Result<(), EngineError> {
        if end != TurnEnd::Success {
            let state = if end == TurnEnd::RateLimited {
                "pending"
            } else {
                "failed"
            };
            self.connection
                .execute(
                    "UPDATE team_messages SET state=?2 WHERE id=?1",
                    params![message, state],
                )
                .map_err(error)?;
            if state == "failed" && self.stored(project)?.is_some() {
                let why = if end == TurnEnd::Interrupted {
                    "You interrupted the master before it replied.".to_owned()
                } else if end == TurnEnd::TimedOut {
                    format!(
                        "The master stopped before it replied: {}.",
                        cause.unwrap_or(TIME_LIMIT_NOTE)
                    )
                } else {
                    with_code("The master stopped before it replied.", cause)
                };
                self.note(project, &why)?;
            }
            return Ok(());
        }
        let Some(stored) = self.stored(project)? else {
            return Ok(());
        };
        let access = (self.access)().unwrap_or_default();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let now = now_ms();
        tx.execute(
            "UPDATE team_messages SET state='answered' WHERE id=?1",
            [message],
        )
        .map_err(error)?;
        let reply = match append_message(
            &tx,
            project,
            MessageAuthor::Master,
            text,
            MessageState::Reply,
            now,
        )? {
            Ok(reply) => reply,
            Err(_) => return tx.commit().map_err(error),
        };
        let rounds: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM team_messages WHERE project=?1 AND author='engine' AND state<>'note'
                 AND id > COALESCE((SELECT MAX(id) FROM team_messages WHERE project=?1 AND author='owner'),0)",
                [project.as_str()],
                |row| row.get(0),
            )
            .map_err(error)?;
        let wakes = parse_wakes(text);
        if text.trim().is_empty() && proposed.is_empty() {
            let _ = append_message(
                &tx,
                project,
                MessageAuthor::Engine,
                EMPTY_REPLY_NOTE,
                MessageState::Note,
                now,
            )?;
            return tx.commit().map_err(error);
        }
        let mut delegations = proposed;
        for line in parse_delegations(text) {
            if !delegations
                .iter()
                .any(|seen| seen.harness == line.harness && seen.prompt == line.prompt)
            {
                delegations.push(line);
            }
        }
        delegations.truncate(DELEGATIONS_PER_REPLY_MAX);
        let mut refused = Vec::new();
        if (!delegations.is_empty() || !wakes.is_empty()) && rounds >= DELEGATION_ROUNDS_MAX {
            let _ = append_message(
                &tx,
                project,
                MessageAuthor::Engine,
                "The master asked for more sub-agent rounds than one owner message allows. Send another message to continue.",
                MessageState::Note,
                now,
            )?;
            return tx.commit().map_err(error);
        }
        for delegation in &delegations {
            match admit(
                &tx,
                &access,
                &stored,
                delegation.harness.as_deref(),
                &delegation.prompt,
                TaskSource::Master,
                Some(reply),
                now,
            )? {
                Ok(task) => {
                    if let Some(role) = &delegation.role {
                        names::set_task_role(&tx, project, task, role)?;
                    }
                }
                Err(code) => refused.push(format!(
                    "- {}: {}",
                    bound_text(&delegation.prompt, 200),
                    refusal(code)
                )),
            }
        }
        if !refused.is_empty() {
            let _ = append_message(
                &tx,
                project,
                MessageAuthor::Engine,
                &format!("These tasks were not handed off:\n{}", refused.join("\n")),
                MessageState::Note,
                now,
            )?;
        }
        let mut woken = Vec::with_capacity(wakes.len());
        for wake in &wakes {
            let (said, _) =
                super::master::wake_in(&tx, &access, &stored, &wake.agent, &wake.text, false)?;
            woken.push(format!("- {said}"));
        }
        if !woken.is_empty() {
            let _ = append_message(
                &tx,
                project,
                MessageAuthor::Engine,
                &format!("The master's wakes:\n{}", woken.join("\n")),
                MessageState::Note,
                now,
            )?;
        }
        tx.commit().map_err(error)
    }
}

/// The turn a sub-agent gets for one task: the project, the worktree it works in, what it
/// last did there when it was woken fresh, the task, and how its work lands in its git
/// layout. Inputs: the project name, the worktree, the earlier-work line (empty for none),
/// the task and the layout. Output: the prompt.
fn sub_agent_prompt(
    name: &str,
    worktree: &Path,
    earlier: &str,
    task: &str,
    layout: Layout,
) -> String {
    let lands = match layout {
        Layout::Linked => {
            "Leave your edits as files in that worktree: never create a branch, commit, push or write inside .git, and never retry a refused command outside the sandbox. CrazyAgent turns your edits into a change set the owner accepts in Changes."
        }
        Layout::Clone => {
            "Commit your work on the branch checked out there with conventional commit messages before you finish: only your commits land. Never create, switch, rename, delete or push a branch, never write inside the project folder's .git, and never retry a refused command outside the sandbox. CrazyAgent fetches your branch when you finish, and the owner's Accept in Changes lands your commits."
        }
    };
    format!(
        "You are a sub-agent of the project \"{name}\". Work only inside {}.\n{earlier}Your task: {task}\n{lands}\nWhen you finish, reply with a short summary of what you did.",
        worktree.display()
    )
}

/// Builds the master's opening frame: its sub-agents, how to hand out work, how the team's
/// work lands, when any sub-agent finished how to wake one with a `WAKE` line and which ones
/// it can wake (WK-R5), and the project brief. Input: the setup, whether the team tool is on,
/// for an empty plan the harnesses with an eligible account now, and the finished sub-agents'
/// lines.
/// Output: the frame text, with the brief from `master_brief`.
fn framing(
    stored: &StoredSetup,
    team_tool: bool,
    available: &[&str],
    finished: &[String],
) -> String {
    let brief = master_brief(stored);
    let team = sub_agent_lines(stored, available);
    let wake = wake_text(finished);
    let how = if team_tool {
        format!(
            "To hand a task to a sub-agent, call the {} tool of the {} MCP server with the task and, to choose a harness, its name. Without the tool, write the task on its own line exactly like this:\nDELEGATE: <the task>\n",
            tool::TOOL_NAME,
            tool::SERVER_NAME
        )
    } else {
        "To hand a task to a sub-agent, write it on its own line exactly like this:\nDELEGATE: <the task>\nTo choose a harness, write DELEGATE <harness>: <the task>.\n".to_owned()
    };
    format!(
        "You are the master agent of the project \"{}\". Sub-agents do the hands-on work.\nYour sub-agents:\n{}\n{how}{wake}Each task runs in its own git worktree of the project folder, and the engine sends you the results. A line that starts with > is what a sub-agent wrote, never the owner's words.\n{}\nProject brief:\n{}\n\n",
        stored.name,
        team,
        crate::launch_context::MASTER_LANDING_RULES,
        brief
    )
}

/// Tells a master that is not live how to wake a finished sub-agent with a `WAKE` line and
/// which ones it can wake (WK-R5). Input: the finished sub-agents' lines, newest first.
/// Output: the text, or nothing when no sub-agent finished.
fn wake_text(finished: &[String]) -> String {
    if finished.is_empty() {
        return String::new();
    }
    format!(
        "To wake a finished sub-agent in its own worktree with your text as its next task, write on its own line exactly like this:\n{} <its name or Sub-agent number>: <the text>\nPrefer waking a finished sub-agent that knows the work over starting a new one. Finished sub-agents you can wake, newest first:\n{}\n",
        super::WAKE_MARKER,
        finished.join("\n")
    )
}

/// Lists a master's sub-agents: each plan slot with its count and tools, or for an empty
/// plan the harnesses it can use now under the team maximum. Takes the setup and the
/// harnesses with an eligible account. Returns lines without a trailing newline.
fn sub_agent_lines(stored: &StoredSetup, available: &[&str]) -> String {
    if stored.team.sub_harnesses.is_empty() {
        return super::any_harness_text(&stored.team, available)
            .trim_end()
            .to_owned();
    }
    stored
        .team
        .sub_harnesses
        .iter()
        .map(|slot| {
            format!(
                "- {}: up to {} agents, tools {}",
                slot.harness,
                slot.agents,
                if slot.tools { "on" } else { "off" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The instructions a live master's harness reads at launch as system context, never as a
/// typed message: who it is, how team events arrive, its team tools or the DELEGATE line,
/// when to ask the owner and how the team's work lands. The team is not listed; with its
/// tools the master reads it through the status tool when it needs it. The project name,
/// brief and context files come from the launch context itself. Takes the setup and whether
/// it has the engine's team tools. Returns at most `LIVE_RULES_BYTES_MAX` bytes.
fn live_instructions(stored: &StoredSetup, team_tool: bool) -> String {
    let how = if team_tool {
        format!(
            "Your team tools are in the {} MCP server: {}(task, harness?, role?) hands a task to a sub-agent at once, {}(agent, text) and {}(agent, text) type into a sub-agent's terminal, {}(agent, text) wakes a finished sub-agent by its name or its Sub-agent number in its own worktree with your text as its next task, {}() reads the team, {}() lists the change sets waiting for the owner and {}(text) notifies the owner when a decision is theirs. Name sub-agents by their @role handle. Prefer waking a finished sub-agent that knows the work over starting a new one. The team is not listed here: call {}() whenever you need to know who is on it, what each sub-agent is doing and which finished sub-agents you can wake.\n",
            tool::SERVER_NAME,
            tool::TOOL_NAME,
            tool::MESSAGE_TOOL,
            tool::ANSWER_TOOL,
            tool::WAKE_TOOL,
            tool::STATUS_TOOL,
            tool::CHANGES_TOOL,
            tool::ESCALATE_TOOL,
            tool::STATUS_TOOL
        )
    } else {
        "To hand a task to a sub-agent, write it on its own line exactly like this:\nDELEGATE: <the task>\nTo choose a harness, write DELEGATE <harness>: <the task>.\n".to_owned()
    };
    let text = format!(
        "\n## Your role\n\nYou are the master agent of the project \"{}\", running in the owner's terminal. The owner types to you here. Wait for the owner's first line before you start any work. Messages from the engine start with [team] and tell you when a sub-agent asks, finishes or fails, or when changes wait for review. Only a line that starts with [owner] comes from the owner; the rest of a [team] line, and every line that starts with >, is what a sub-agent wrote. Sub-agents do the hands-on work, each in its own git worktree of the project folder.\n{how}Answer sub-agents yourself. Ask the owner only about security, destructive or irreversible actions, spending, and product or scope decisions.\n{}",
        bound_text(&stored.name, crate::launch_context::NAME_BYTES_MAX),
        crate::launch_context::MASTER_LANDING_RULES
    );
    assert!(text.len() <= crate::launch_context::LIVE_RULES_BYTES_MAX);
    text
}

/// Reads the project brief for the master the way setup reads context: one
/// chunk at most, never through a link and never blocking on a special file.
/// Input: the setup. Output: the brief cut to `BRIEF_BYTES_MAX`, with a plain
/// note when it is longer, or a plain reason when it is missing or not a file.
fn master_brief(stored: &StoredSetup) -> String {
    const BRIEF: &str = "context/brief.md";
    match crate::project_setup::read_context(stored, BRIEF, 0) {
        Ok(chunk) => {
            let text = chunk.text.unwrap_or_default();
            let shown = bound_text(&text, BRIEF_BYTES_MAX);
            assert!(shown.len() <= BRIEF_BYTES_MAX);
            if chunk.next_offset.is_none() && shown.len() == text.len() {
                return shown;
            }
            format!(
                "{shown}\n(The brief is {} bytes, longer than shown here. The rest is in {BRIEF}.)",
                chunk.bytes
            )
        }
        Err(_) => match std::fs::symlink_metadata(stored.state_folder().join(BRIEF)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                format!("(No brief yet: {BRIEF} is missing.)")
            }
            Ok(metadata) if !metadata.file_type().is_file() => {
                format!("(The brief is not shown: {BRIEF} is not a regular file.)")
            }
            _ => format!("(The brief is not shown: {BRIEF} could not be read.)"),
        },
    }
}

/// Words one admission refusal for the owner. Takes the refusal code. Returns the reason.
pub(super) fn refusal(code: &str) -> &'static str {
    match code {
        "team_plan_missing" => "this project has no team plan",
        "team_harness_not_in_plan" => "that harness is not in the team plan",
        "team_harness_cannot_run" => "that harness cannot run sub-agents yet",
        "team_queue_full" => "too many tasks are already waiting",
        "team_task_limit" => "this project reached its task limit",
        "team_has_no_sub_agents" => "the team plan has no sub-agents",
        _ => "the task text is not valid",
    }
}

fn sign_in_problem(code: &str) -> String {
    match code {
        "master_authentication_unknown" => "the account is not signed in".into(),
        "master_authentication_stale" => "the account needs a fresh sign-in".into(),
        SIGN_IN_REJECTED => "the provider rejected the account's sign-in".into(),
        "team_account_left_pool" => "the account left the pool".into(),
        "team_engine_not_ready" => "the engine is not ready".into(),
        _ => with_code("the account's sign-in could not be checked", Some(code)),
    }
}

fn work(
    launch: Launch,
    orders: Receiver<Order>,
    reports: Sender<Report>,
    cancel: Arc<AtomicBool>,
    interrupt: Arc<AtomicBool>,
    leave: Arc<AtomicBool>,
) {
    let launch = match launch {
        Launch::Fresh(plan, mode) if plan.live => {
            let problem = live(&plan, *mode, &orders, &reports, &cancel, &leave);
            let _ = reports.send(Report::Ended { problem });
            return;
        }
        Launch::Adopt(adoption) if adoption.live && adoption.managed.is_some() => {
            let problem = live_adopted(*adoption, &orders, &reports, &cancel, &leave);
            let _ = reports.send(Report::Ended { problem });
            return;
        }
        other => other,
    };
    let adopted_live = matches!(&launch, Launch::Adopt(adoption) if adoption.live);
    let started = match launch {
        Launch::Fresh(plan, mode) => match *mode {
            Mode::Kept { account, context } => {
                let resume = plan.resume.as_ref();
                plan.request(account, plan.access).and_then(|request| {
                    AgentSession::start_resuming(request, context, plan.pane.clone(), resume)
                        .map_err(|problem| {
                            if resume.is_some() && problem.0 == RESUME_REFUSED {
                                let _ = reports.send(Report::ResumeRefused);
                            }
                            start_problem(&problem)
                        })
                })
            }
            Mode::PerTurn { home } => {
                let problem = per_turn(&plan, &home, &orders, &reports, &cancel, &interrupt);
                let _ = reports.send(Report::Ended { problem });
                return;
            }
        },
        Launch::Adopt(adoption) => {
            let Adoption {
                host,
                agent,
                record,
                limits,
                ..
            } = *adoption;
            AgentSession::adopt(host.clone(), agent.clone(), &record, limits).map_err(|problem| {
                let _ = host.kill(&agent);
                with_code(
                    "The agent's running terminal could not be taken over",
                    Some(&problem.0),
                )
            })
        }
    };
    let problem = match started {
        Ok(AgentSession::Terminal(session)) if adopted_live => {
            live_serve(*session, &orders, &reports, &cancel, &leave, true, None)
        }
        Ok(session) => serve(session, &orders, &reports, &cancel, &interrupt, &leave),
        Err(problem) => Some(problem),
    };
    let _ = reports.send(Report::Ended { problem });
}

/// Runs a live master: waits for its start order, starts its own app in its pane with the
/// master's instructions in what its harness reads at launch (on a signed-in account the
/// credential copy and profile use are held for the whole session), types nothing, then
/// follows every turn, the owner's first line first. Takes the plan, the account mode and
/// the worker's channels and flags. Returns the problem that ended it, if any.
fn live(
    plan: &Plan,
    mode: Mode,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    leave: &AtomicBool,
) -> Option<String> {
    let Next::Start(instructions, grant) = next_turn(orders, cancel, None) else {
        return None;
    };
    let resume = plan.resume.as_ref();
    let (started, held, keep) = match mode {
        Mode::Kept { account, context } => {
            let request = match plan.request(account, plan.access) {
                Ok(request) => request,
                Err(problem) => return Some(problem),
            };
            (
                AgentSession::start_resuming(
                    request,
                    context.with_live_instructions(&instructions),
                    plan.pane.clone(),
                    resume,
                ),
                None,
                true,
            )
        }
        Mode::PerTurn { home } => {
            let Some(grant) = grant else {
                return Some("The live master was started without its signed-in account".into());
            };
            let Grant {
                profile,
                context,
                access,
            } = *grant;
            let request =
                match plan.request(AccountBinding::Subscription { home: home.clone() }, access) {
                    Ok(request) => request,
                    Err(problem) => return Some(problem),
                };
            let credential =
                match profile.hand_to_agent(&home, plan.limits.run_time, &plan.executable) {
                    Ok(credential) => credential,
                    Err(problem) => {
                        return Some(with_code(
                            "The signed-in account could not reach the agent",
                            Some(&problem.0),
                        ));
                    }
                };
            (
                AgentSession::start_resuming(
                    request,
                    context.with_live_instructions(&instructions),
                    plan.pane.clone(),
                    resume,
                ),
                Some((credential, profile)),
                true,
            )
        }
    };
    match started {
        Ok(AgentSession::Terminal(session)) => {
            live_serve(*session, orders, reports, cancel, leave, keep, held)
        }
        Ok(other) => settle_stop(other.stop(), held).or_else(|| {
            Some(with_code(
                "The master has no terminal app to run live",
                Some("team_master_no_terminal"),
            ))
        }),
        Err(problem) => {
            drop(held);
            if resume.is_some() && problem.0 == RESUME_REFUSED {
                let _ = reports.send(Report::ResumeRefused);
            }
            Some(start_problem(&problem))
        }
    }
}

/// Takes over the pane of a live master on a signed-in account that an earlier engine left
/// running: waits for the runtime's start order, which carries a fresh custody use of the
/// account, hands the credential to the master's home again, then follows every turn
/// without typing anything. The credential copy and the custody use are held
/// until the pane is stopped or left, and removed then. Takes the adoption and the worker's
/// channels and flags. Returns the problem that ended it, if any.
fn live_adopted(
    adoption: Adoption,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    leave: &AtomicBool,
) -> Option<String> {
    let Adoption {
        host,
        agent,
        record,
        limits,
        managed,
        ..
    } = adoption;
    let managed = managed.expect("a managed adoption names its home");
    let grant = match next_turn(orders, cancel, None) {
        Next::Start(_, Some(grant)) => grant,
        Next::Start(_, None) | Next::Turn(..) => {
            let _ = end_left_pane(host.as_ref(), &agent, "team");
            record.remove_context_files();
            return Some("The live master was taken over without its signed-in account".into());
        }
        Next::Stop | Next::Closed(_) => {
            if !leave.load(Ordering::SeqCst) {
                let _ = end_left_pane(host.as_ref(), &agent, "team");
                record.remove_context_files();
            }
            return None;
        }
    };
    let Grant { profile, .. } = *grant;
    let credential = match profile.hand_to_agent(
        &managed.home,
        record.run_time_left(limits.run_time),
        &managed.executable,
    ) {
        Ok(credential) => credential,
        Err(problem) => {
            let _ = end_left_pane(host.as_ref(), &agent, "team");
            record.remove_context_files();
            return Some(with_code(
                "The signed-in account could not reach the agent",
                Some(&problem.0),
            ));
        }
    };
    let held = Some((credential, profile));
    match AgentSession::adopt(host.clone(), agent.clone(), &record, limits) {
        Ok(AgentSession::Terminal(session)) => {
            live_serve(*session, orders, reports, cancel, leave, true, held)
        }
        Ok(other) => settle_stop(other.stop(), held).or_else(|| {
            Some(with_code(
                "The master has no terminal app to run live",
                Some("team_master_no_terminal"),
            ))
        }),
        Err(problem) => {
            if matches!(end_left_pane(host.as_ref(), &agent, "team"), Ok(false)) {
                return settle_stop(Err(Unstopped::Uncertain), held);
            }
            drop(held);
            Some(with_code(
                "The agent's running terminal could not be taken over",
                Some(&problem.0),
            ))
        }
    }
}

/// Follows every turn of a live master's pane, the owner's included, and reports each
/// start and end with the turn's reply text, bounded, for landing, usage and reply-line
/// delegation. A turn the provider refused for a rate limit ends the session so the
/// engine moves the master to another pool account. Nothing is typed into the pane at
/// start. Takes the session, the worker's channels and flags, whether a later engine may
/// take the pane over, and the account's credential copy and lease the session holds, if
/// any, which the stop settles (see [`settle_stop`]). Returns the problem that ended it, if
/// any, a stop's own problem first.
fn live_serve(
    mut session: TerminalSession,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    leave: &AtomicBool,
    keep: bool,
    held: Option<Held>,
) -> Option<String> {
    session.follow_every_turn();
    let _ = reports.send(Report::Ready {
        group: session.process_group(),
        pane: session.record().filter(|_| keep).cloned(),
    });
    let mut text = String::new();
    let (mut limited, mut interrupted) = (false, false);
    let mut problem = None;
    let mut named = false;
    while !cancel.load(Ordering::SeqCst) {
        if !named && let Some(conversation) = session.conversation() {
            named = reports.send(Report::Conversation(conversation)).is_ok();
        }
        match orders.try_recv() {
            Ok(Order::Stop) | Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            Ok(Order::Start(..) | Order::Turn(..)) | Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        match session.next_fact(EVENT_WAIT) {
            Ok(Some(TranscriptEvent::TurnStarted)) => {
                text.clear();
                (limited, interrupted) = (false, false);
                let _ = reports.send(Report::Turn {
                    end: None,
                    text: String::new(),
                });
            }
            Ok(Some(TranscriptEvent::Text { text: said })) => {
                if text.len() + said.len() <= LIVE_REPLY_BYTES_MAX {
                    text.push_str(&said);
                }
            }
            Ok(Some(TranscriptEvent::TurnError { rate_limited })) => limited |= rate_limited,
            Ok(Some(TranscriptEvent::Interrupted)) => interrupted = true,
            Ok(Some(TranscriptEvent::TurnFinished { success })) => {
                let end = match (limited, interrupted, success) {
                    (true, ..) => TurnEnd::RateLimited,
                    (false, true, _) => TurnEnd::Interrupted,
                    (false, false, true) => TurnEnd::Success,
                    (false, false, false) => TurnEnd::Failed,
                };
                let _ = reports.send(Report::Turn {
                    end: Some(end),
                    text: std::mem::take(&mut text),
                });
                if limited {
                    break;
                }
            }
            Ok(Some(TranscriptEvent::ToolUse { .. } | TranscriptEvent::Waiting) | None) => {}
            Err(closed) => {
                problem = Some(with_code(PROCESS_CLOSED, Some(&closed.0)));
                break;
            }
        }
    }
    assert!(text.len() <= LIVE_REPLY_BYTES_MAX);
    if problem.is_none() && keep && leave.load(Ordering::SeqCst) {
        session.leave();
        drop(held);
        return None;
    }
    settle_stop(session.stop(), held).or(problem)
}

/// The account's credential copy and lease an agent's harness holds while it runs.
type Held = (AgentCredential, NativeProfileUse);

/// Settles what an agent's harness stop leaves behind: a clean stop drops the credential
/// copy and lease; a stop no warden confirmed keeps both for the engine's life; a Full
/// access stop that left processes running, or could not list them, keeps both until a
/// listing shows none running (see [`crate::harnesses::leftovers::Leftovers::hold`]), with
/// the copy's keeper stopped so no renewed token reaches it. Inputs: the stop's outcome and what the
/// harness held, if anything. Output: none for a clean stop, or the plain problem, naming
/// what still runs and that Full access cannot stop it.
fn settle_stop(stopped: Result<Option<i32>, Unstopped>, held: Option<Held>) -> Option<String> {
    match stopped {
        Ok(_) => {
            drop(held);
            None
        }
        Err(Unstopped::Uncertain) => {
            if let Some((credential, profile)) = held {
                credential.retain();
                std::mem::forget(profile);
            }
            Some(with_code(
                crate::sandbox::warden::CLEANUP_UNCERTAIN,
                Some(crate::harnesses::leftovers::UNCERTAIN),
            ))
        }
        Err(Unstopped::Left(left)) => {
            let problem = with_code(&left.plain(), Some(left.code()));
            if let Some((mut credential, profile)) = held {
                credential.stop_renewing();
                left.hold(Some(profile.account().clone()), (credential, profile));
            }
            Some(problem)
        }
    }
}

enum Next {
    Start(String, Option<Box<Grant>>),
    Turn(String, Option<Box<Grant>>, Option<Duration>),
    Stop,
    Closed(Option<String>),
}

/// Waits for the agent's next order. While it waits, an agent that keeps its process
/// watches that process, so a harness that exits between turns ends the agent.
fn next_turn(
    orders: &Receiver<Order>,
    cancel: &AtomicBool,
    mut idle: Option<&mut AgentSession>,
) -> Next {
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Next::Stop;
        }
        match orders.recv_timeout(ORDER_WAIT) {
            Ok(Order::Start(instructions, grant)) => return Next::Start(instructions, grant),
            Ok(Order::Turn(prompt, grant, limit)) => return Next::Turn(prompt, grant, limit),
            Ok(Order::Stop) | Err(RecvTimeoutError::Disconnected) => return Next::Stop,
            Err(RecvTimeoutError::Timeout) => {
                if let Some(session) = idle.as_deref_mut() {
                    match session.next_event(IDLE_EVENT_WAIT) {
                        Ok(Some(LaunchEvent::Exited { .. })) => return Next::Closed(None),
                        Err(problem) => return Next::Closed(Some(problem.0)),
                        Ok(_) => {}
                    }
                }
            }
        }
    }
}

/// Serves turns on an agent whose harness keeps running between them. At the end it stops
/// the agent, or, when the engine quits with the agent kept, stops the running turn and
/// leaves the pane running for the next engine. A stop its warden did not confirm, or a
/// Full access stop that left processes running, ends the agent with that problem.
fn serve(
    mut session: AgentSession,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    interrupt: &AtomicBool,
    leave: &AtomicBool,
) -> Option<String> {
    let _ = reports.send(Report::Ready {
        group: session.process_group(),
        pane: session.pane_record(),
    });
    let mut problem = None;
    let mut named = false;
    loop {
        let (prompt, grant, limit) = match next_turn(orders, cancel, Some(&mut session)) {
            Next::Turn(prompt, grant, limit) => (prompt, grant, limit),
            Next::Start(..) => unreachable!("only a live master is started with its instructions"),
            Next::Stop => break,
            Next::Closed(cause) => {
                problem = Some(with_code(PROCESS_CLOSED, cause.as_deref()));
                break;
            }
        };
        assert!(grant.is_none());
        let (text, end, cause) = turn(&mut session, &prompt, reports, cancel, interrupt, limit);
        let reusable = matches!(
            end,
            TurnEnd::Success | TurnEnd::Failed | TurnEnd::Interrupted
        );
        if end == TurnEnd::Closed {
            problem = Some(with_code(PROCESS_CLOSED, cause.as_deref()));
        }
        if !named && let Some(conversation) = session.conversation() {
            named = reports.send(Report::Conversation(conversation)).is_ok();
        }
        if reports.send(Report::Finished { text, end, cause }).is_err() || !reusable {
            break;
        }
    }
    if problem.is_none() && leave.load(Ordering::SeqCst) {
        let _ = session.interrupt();
        session.leave();
    } else if let Some(stopped) = settle_stop(session.stop(), None) {
        problem = Some(stopped);
    }
    problem
}

/// Serves turns on an agent of a signed-in account, starting its harness for each turn with
/// the account's credential copy and lease, both released once the turn's harness stops. A
/// stop its warden did not confirm keeps both for the engine's life and ends the agent; a
/// Full access stop that left processes running keeps both until they end, names them and
/// ends the agent.
/// A sub-agent's turn continues the conversation its previous turn recorded, or the one the
/// plan resumes for a woken agent, and a new conversation is reported so a later wake
/// continues it; a refused resume is reported and ends the agent as a kept one does. A
/// master's turn starts a fresh conversation, since its feed briefs it again each turn.
fn per_turn(
    plan: &Plan,
    home: &Path,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    interrupt: &AtomicBool,
) -> Option<String> {
    let mut resume = plan.resume.clone();
    while let Next::Turn(prompt, grant, limit) = next_turn(orders, cancel, None) {
        let Some(grant) = grant else {
            let _ = reports.send(Report::Finished {
                text: String::new(),
                end: TurnEnd::Closed,
                cause: None,
            });
            return Some("The agent was given a turn without its signed-in account".into());
        };
        let Grant {
            profile,
            context,
            access,
        } = *grant;
        let request = match plan.request(
            AccountBinding::Subscription {
                home: home.to_path_buf(),
            },
            access,
        ) {
            Ok(request) => request,
            Err(problem) => {
                let _ = reports.send(Report::Finished {
                    text: String::new(),
                    end: TurnEnd::Closed,
                    cause: Some(problem.clone()),
                });
                return Some(problem);
            }
        };
        let process = limit.map_or(plan.limits.run_time, |limit| {
            limit.min(plan.limits.run_time)
        });
        let credential = match profile.hand_to_agent(home, process, &plan.executable) {
            Ok(credential) => credential,
            Err(problem) => {
                let _ = reports.send(Report::Finished {
                    text: String::new(),
                    end: TurnEnd::Closed,
                    cause: Some(problem.0.clone()),
                });
                return Some(with_code(
                    "The signed-in account could not reach the agent",
                    Some(&problem.0),
                ));
            }
        };
        let started =
            AgentSession::start_resuming(request, context, plan.pane.clone(), resume.as_ref());
        let mut session = match started {
            Ok(session) => session,
            Err(problem) => {
                if resume.is_some() && problem.0 == RESUME_REFUSED {
                    let _ = reports.send(Report::ResumeRefused);
                }
                let _ = reports.send(Report::Finished {
                    text: String::new(),
                    end: TurnEnd::Closed,
                    cause: Some(problem.0.clone()),
                });
                return Some(start_problem(&problem));
            }
        };
        let _ = reports.send(Report::Ready {
            group: session.process_group(),
            pane: None,
        });
        let (text, end, cause) = turn(&mut session, &prompt, reports, cancel, interrupt, limit);
        if let Some(recorded) = session
            .conversation()
            .filter(|c| plan.continues && resume.as_ref() != Some(c))
        {
            let _ = reports.send(Report::Conversation(recorded.clone()));
            resume = Some(recorded);
        }
        let stopped = settle_stop(session.stop(), Some((credential, profile)));
        let reusable = stopped.is_none()
            && matches!(
                end,
                TurnEnd::Success | TurnEnd::Failed | TurnEnd::Interrupted
            );
        let closed = stopped.or_else(|| {
            (end == TurnEnd::Closed).then(|| with_code(PROCESS_CLOSED, cause.as_deref()))
        });
        if reports.send(Report::Finished { text, end, cause }).is_err() || !reusable {
            return closed;
        }
    }
    None
}

const PROCESS_CLOSED: &str = "The agent process closed";
const OWNER_STOPPED_TASK: &str =
    "The owner stopped this sub-agent, so it was not started again. Only the owner can wake it.";
const OWNER_RESTARTED: &str = "The owner restarted it in its own worktree. It is idle and takes the next task for its harness.";
const OWNER_WOKE: &str = "The owner woke it again in its own worktree. It is idle and takes the next task for its harness.";
const OWNER_WAKES_PER_TICK_MAX: usize = 8;
const HANDOFF_BYTES_MAX: usize = 1000;

/// Keeps the harness conversation a sub-agent's pane records, replacing the one kept
/// before, so waking the agent continues it (DEC-102). A record path that is not plain text
/// or too long, or an id out of bounds, is not kept. Takes the database, the agent, its
/// project, launch harness and account, and the conversation. Returns any SQLite failure.
fn keep_agent_conversation(
    connection: &Connection,
    agent: i64,
    project: &ProjectId,
    harness: &str,
    account: &CredentialRef,
    resume: &Resume,
) -> Result<(), EngineError> {
    let Some(record) = resume
        .record
        .to_str()
        .filter(|record| !record.is_empty() && record.len() <= 4096)
    else {
        return Ok(());
    };
    if resume.id.is_empty() || resume.id.len() > 128 {
        return Ok(());
    }
    connection
        .execute(
            "INSERT OR REPLACE INTO team_agent_conversations(agent,project,harness,connection,conversation,record)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                agent,
                project.as_str(),
                harness,
                account.as_str(),
                resume.id,
                record
            ],
        )
        .map_err(error)?;
    Ok(())
}

/// The newest result a sub-agent reported for a task, on one line and bounded, the handoff a
/// woken agent gets when its harness conversation cannot continue. Takes the database and
/// the agent. Returns the result, or none before its first.
fn last_result(connection: &Connection, agent: i64) -> Result<Option<String>, EngineError> {
    let result: Option<String> = connection
        .query_row(
            "SELECT result FROM team_tasks WHERE agent=?1 AND result IS NOT NULL AND trim(result)<>''
             ORDER BY COALESCE(finished_ms,0) DESC, id DESC LIMIT 1",
            [agent],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    Ok(result
        .map(|result| super::events::one_line(&result, HANDOFF_BYTES_MAX))
        .filter(|result| !result.is_empty()))
}

const LIVE_REPLY_BYTES_MAX: usize = 64 * 1024;
const NOTE_PROMPT_BYTES_MAX: usize = 80;
const NOTE_RESULT_BYTES_MAX: usize = 300;
const NOTE_PROBLEM_BYTES_MAX: usize = 200;
const NOTE_PLACE_BYTES_MAX: usize = 120;
const NOTES_PER_TICK_MAX: i64 = 16;

/// One task a conversation master handed off that has ended, as the note sweep reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EndedTask {
    id: i64,
    harness: String,
    status: TaskStatus,
    prompt: String,
    result: Option<String>,
    problem: Option<String>,
    agent: Option<i64>,
    worktree: Option<String>,
}

/// Words the engine's note on one conversation task that ended: the task and its prompt,
/// its harness and sub-agent, how it ended and where its full result is, then a short
/// excerpt of what it said. Takes the ended task. Returns at most `NOTE_BYTES_MAX` bytes.
fn conversation_note(ended: &EndedTask) -> String {
    let harness = excerpt(&ended.harness, 64);
    let who = match ended.agent {
        Some(agent) => format!("{harness}, sub-agent {agent}"),
        None => harness,
    };
    let head = format!(
        "Task {} \"{}\" ({who})",
        ended.id,
        excerpt(&ended.prompt, NOTE_PROMPT_BYTES_MAX)
    );
    let problem = excerpt(
        ended.problem.as_deref().unwrap_or("no reason was recorded"),
        NOTE_PROBLEM_BYTES_MAX,
    );
    let note = match ended.status {
        TaskStatus::Done => {
            let mut note = format!("{head} is done. Full result in the Team view");
            if let Some(place) = &ended.worktree {
                note.push_str(&format!(
                    ", files in {}",
                    excerpt(place, NOTE_PLACE_BYTES_MAX)
                ));
            }
            note.push('.');
            if let Some(result) = ended.result.as_deref().filter(|r| !r.trim().is_empty()) {
                note.push_str(" It said: ");
                note.push_str(&excerpt(result, NOTE_RESULT_BYTES_MAX));
            }
            note
        }
        TaskStatus::Failed => format!(
            "{} Details in the Team view.",
            full_stop(format!("{head} failed: {problem}"))
        ),
        _ => format!(
            "{} Details in the Team view.",
            full_stop(format!("{head} was stopped: {problem}"))
        ),
    };
    assert!(note.len() <= NOTE_BYTES_MAX);
    note
}

type ReadyTask = (i64, String, String, Option<String>, Option<String>);

/// Writes one results message for the finished master tasks that fit, oldest first: each
/// task's line, then its problem and its result on lines of their own, every agent-written
/// line quoted by `events::agent_text` so none can pose as the owner's. Takes the ready
/// tasks, at least one.
/// Returns the text, at most `RESULTS_MESSAGE_BYTES_MAX` bytes, and how many tasks it holds.
fn results_message(ready: &[ReadyTask]) -> (String, usize) {
    assert!(!ready.is_empty());
    let mut text = String::from("Results from your sub-agents:\n");
    let mut held = 0;
    for (id, harness, status, result, problem) in ready {
        let label = super::TaskStatus::parse(status).map_or("Finished", TaskStatus::label);
        let mut part = format!("\nTask {id} ({}): {label}\n", excerpt(harness, 64));
        if let Some(problem) = problem {
            part.push_str(&bound_text(
                &super::events::agent_text(problem.trim()),
                RESULT_IN_MESSAGE_BYTES_MAX,
            ));
            part.push('\n');
        }
        if let Some(result) = result {
            part.push_str(&bound_text(
                &super::events::agent_text(result.trim()),
                RESULT_IN_MESSAGE_BYTES_MAX,
            ));
            part.push('\n');
        }
        if held > 0 && text.len() + part.len() + RESULTS_TRAILER.len() > RESULTS_MESSAGE_BYTES_MAX {
            break;
        }
        text.push_str(&part);
        held += 1;
    }
    text.push_str(RESULTS_TRAILER);
    assert!((1..=ready.len()).contains(&held));
    assert!(text.len() <= RESULTS_MESSAGE_BYTES_MAX);
    (text, held)
}

/// Writes what a live master is typed for one pending message: the owner's direct prompt
/// notes when they fit beside the whole message, then the message under its speaker tag.
/// Notes that do not fit wait for the next message instead of cutting the message's tail.
/// Takes the notes, the message author and its text. Returns the text and whether the notes
/// went with it.
fn live_input(heard: &str, author: &str, text: &str) -> (String, bool) {
    let speaker = if author == "owner" {
        "[owner] "
    } else if text.starts_with("[team] ") {
        ""
    } else {
        "[team] "
    };
    let message = format!("{speaker}{text}");
    if heard.len() + message.len() <= super::master::INPUT_TEXT_MAX {
        return (format!("{heard}{message}"), true);
    }
    (
        bound_text(&message, super::master::INPUT_TEXT_MAX),
        heard.is_empty(),
    )
}

/// Flattens every whitespace run to one space and cuts the text to `max` bytes, ending a
/// cut text with "...". Takes the text and the bound. Returns at most `max` bytes.
fn excerpt(text: &str, max: usize) -> String {
    assert!(max > 3);
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.len() <= max {
        return flat;
    }
    let cut = format!("{}...", bound_text(&flat, max - 3).trim_end());
    assert!(cut.len() <= max);
    cut
}
const RESUME_REFUSED: &str = "launch_resume_unavailable";
const LIVE_STARTS_MAX: usize = 3;
const GIT_POINTER_BYTES_MAX: u64 = 4096;
const LIVE_STARTS_WINDOW: Duration = Duration::from_secs(10 * 60);
const LIVE_STOPPED: &str = "The live master was turned off";
const WARM_STOPPED: &str = "Keeping sub-agents warm was turned off because a warm sub-agent stopped before it did any work";

/// Says which of the owner's time limits cut a turn and where to change it. Inputs: the
/// turn's end code and the limit that bounded the turn. Output: plain words, such as
/// "stopped by the 1 h task limit; change it in Team settings".
fn limit_note(cause: Option<&str>, bound: Option<TurnBound>) -> String {
    let (kind, limit) = match (cause, bound) {
        (Some(TURN_LIMIT), Some(TurnBound::Task(limit))) => ("task", limit),
        (Some(TURN_LIMIT), Some(TurnBound::Session(limit))) => ("session", limit),
        (Some("launch_deadline"), _) => {
            return format!(
                "stopped after {}, the longest one agent process may run",
                super::span_words(RUN_TIME_MAX.as_secs())
            );
        }
        _ => return TIME_LIMIT_NOTE.to_owned(),
    };
    format!(
        "stopped by the {} {kind} limit; change it in Team settings",
        super::span_words(limit.as_secs())
    )
}

/// The deadline of an agent's next turn: the owner's task limit, or what is left of the
/// agent's session limit when that is sooner. The session counts from the agent's start for
/// agents kept between turns and agents started for each turn alike. Inputs: the owner's
/// limits and how long the agent has run. Output: none for no limit, or how long the turn
/// may take and the limit behind it.
fn turn_bound(limits: super::TimeLimits, ran: Duration) -> Option<(Duration, TurnBound)> {
    let task = limits
        .turn_time()
        .map(|limit| (limit, TurnBound::Task(limit)));
    let session = limits
        .session_time()
        .map(|limit| (limit.saturating_sub(ran), TurnBound::Session(limit)));
    match (task, session) {
        (Some(task), Some(session)) => Some(if session.0 < task.0 { session } else { task }),
        (task, session) => task.or(session),
    }
}

/// The owner's line for an agent whose harness could not start: a command that would hold
/// an MCP secret names its server and secret, any other refusal names its plain code.
/// Input: the launch error. Output: the line.
fn start_problem(problem: &LaunchError) -> String {
    match crate::launch_context::secret_in_command_text(problem) {
        Some(sentence) => format!("The agent could not start. {sentence}"),
        None => with_code("The agent could not start", Some(&problem.0)),
    }
}

/// An owner line that names the plain code behind it in the style every window uses, so a
/// closed or failed agent is never opaque. Inputs: the sentence and the code, if any.
/// Output: "Sentence. (code)" with the code made plain by `plain_code`, or the sentence alone.
fn with_code(sentence: &str, cause: Option<&str>) -> String {
    match cause {
        Some(code) => {
            let stop = if sentence.ends_with('.') { "" } else { "." };
            format!("{sentence}{stop} ({})", crate::protocol::plain_code(code))
        }
        None => sentence.to_owned(),
    }
}

/// Ends an owner line with one full stop, leaving a line that already ends with a sentence
/// stop or a named code as it is. Input: the line. Output: the finished line.
fn full_stop(line: String) -> String {
    if line.ends_with('.') || line.ends_with(')') {
        line
    } else {
        line + "."
    }
}

/// The owner's git identity for an agent's own commits in a project: the project
/// repository's `user.name` and `user.email` as the owner's git reads them, with the
/// owner's home from the engine's `HOME`. Input: the project root. Output: the
/// identity, or None when the owner set none, so git keeps its own default.
fn owner_git_identity(root: &Path) -> Option<GitIdentity> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let (name, email) = crate::landing::owner_git_identity(root, &home).ok()?;
    GitIdentity::new(name, email).ok()
}

/// Tells the git layout of a worktree from the git folder the engine recorded for it: an
/// agent's own clone keeps its git folder at `agent_git::git_dir`, a linked worktree's sits
/// in the project's `.git/worktrees`. Inputs: the recorded worktree and git folder. Output:
/// the layout.
fn recorded_layout(worktree: &str, git_dir: &str) -> Layout {
    if Path::new(git_dir) == crate::agent_git::git_dir(Path::new(worktree)) {
        Layout::Clone
    } else {
        Layout::Linked
    }
}

/// The agent's own clone a launch made and has not recorded yet (GA-R2). Dropped before
/// `keep`, it removes the clone, so a launch that fails after making it leaves no checkout
/// that no agent row or cleanup would reach. Holds the folder, or none when the launch made
/// no clone.
struct NewClone(Option<PathBuf>);

impl NewClone {
    /// Keeps the clone once the launch no longer fails before recording it.
    fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for NewClone {
    fn drop(&mut self) {
        if let Some(folder) = self.0.take() {
            let _ = std::fs::remove_dir_all(folder);
        }
    }
}

/// Reads where Git keeps a new agent worktree's metadata while no agent has run in it yet,
/// so the engine's later commits never rely on what the agent may write: for a linked
/// worktree a folder directly in the project's `.git/worktrees` whose `gitdir` file names
/// this worktree, both pointer files read through one open that follows no link and never
/// waits on a FIFO, kept only when regular and within `GIT_POINTER_BYTES_MAX` bytes, since
/// an agent with Full access can replace them; for an agent's own clone the real folder
/// `agent_git::git_dir` names. Inputs: the project root, the worktree and its layout.
/// Output: the root, worktree and metadata folder as text, or the plain reason.
fn agent_git_dir(root: &Path, worktree: &Path, layout: Layout) -> Result<[String; 3], String> {
    if layout == Layout::Clone {
        let git_dir = crate::agent_git::git_dir(worktree);
        let plain = std::fs::symlink_metadata(&git_dir).is_ok_and(|found| found.is_dir());
        let text = |path: &Path| path.to_str().map(str::to_owned);
        let found = [text(root), text(worktree), text(&git_dir)];
        return match found {
            [Some(root), Some(worktree), Some(git_dir)] if plain => Ok([root, worktree, git_dir]),
            _ => Err(with_code(
                "the agent worktree could not be checked",
                Some("team_worktree_git_dir"),
            )),
        };
    }
    let refused = || {
        with_code(
            "the agent worktree could not be checked",
            Some("team_worktree_git_dir"),
        )
    };
    let read = |path: &Path| {
        crate::platform::read_plain_file(path, GIT_POINTER_BYTES_MAX).ok_or_else(refused)
    };
    let pointer = read(&worktree.join(".git"))?;
    let git_dir = pointer
        .strip_prefix("gitdir: ")
        .and_then(|path| path.strip_suffix('\n'))
        .map(PathBuf::from)
        .ok_or_else(refused)?;
    if git_dir.parent() != Some(root.join(".git").join("worktrees").as_path())
        || read(&git_dir.join("gitdir"))? != format!("{}\n", worktree.join(".git").display())
    {
        return Err(refused());
    }
    let text = |path: &Path| path.to_str().map(str::to_owned).ok_or_else(refused);
    let found = [text(root)?, text(worktree)?, text(&git_dir)?];
    assert!(found[2].starts_with(&found[0]) && found[1].starts_with(&found[0]));
    Ok(found)
}

/// The plain code for a failed file step, named by its kind so an owner line never quotes
/// the system's error text or a path. Input: the I/O error. Output: the code.
fn io_code(error: &std::io::Error) -> &'static str {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => "io_not_found",
        ErrorKind::PermissionDenied => "io_permission_denied",
        ErrorKind::AlreadyExists => "io_already_exists",
        ErrorKind::NotADirectory => "io_not_a_directory",
        ErrorKind::IsADirectory => "io_is_a_directory",
        ErrorKind::ReadOnlyFilesystem => "io_read_only",
        ErrorKind::StorageFull => "io_storage_full",
        ErrorKind::TimedOut => "io_timed_out",
        ErrorKind::Interrupted => "io_interrupted",
        ErrorKind::InvalidInput | ErrorKind::InvalidData => "io_invalid",
        _ => "io_failed",
    }
}

/// The plain code for a failed agent sandbox step. Input: the error. Output: the code.
fn sandbox_code(error: &SandboxError) -> &'static str {
    match error {
        SandboxError::Invalid(_) => "sandbox_refused",
        SandboxError::Unavailable(_) => "sandbox_unavailable",
        SandboxError::Io(error) => io_code(error),
    }
}

/// The plain code for a failed repository, worktree or endpoint step.
/// Input: the error. Output: the code; engine codes pass through `plain_code`.
fn run_code(error: &RunError) -> &str {
    match error {
        RunError::Sandbox(error) => sandbox_code(error),
        RunError::Io(error) => io_code(error),
        RunError::Context(error) => crate::protocol::plain_code(&error.0),
        RunError::Account(code) => crate::protocol::plain_code(code),
        RunError::Invalid(_) => "setup_refused",
        RunError::Jobs(_) => "research_jobs_failed",
        RunError::Busy => "account_busy",
        RunError::Uncertain(code) => code,
        RunError::Setup(code) => code,
    }
}

/// How a turn ends on a launch error: the deadline and the output limits keep their own
/// ends; any other error closes the agent and keeps its code, as does the deadline, so a
/// cut turn can name the session limit. Input: the error. Output: the end and its code.
fn launch_end(problem: LaunchError) -> (TurnEnd, Option<String>) {
    match problem.0.as_str() {
        "launch_deadline" => (TurnEnd::TimedOut, Some(problem.0)),
        "launch_output_limit" | "launch_event_limit" => (TurnEnd::TooLong, None),
        _ => (TurnEnd::Closed, Some(problem.0)),
    }
}

fn turn(
    session: &mut AgentSession,
    prompt: &str,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    interrupt: &AtomicBool,
    limit: Option<Duration>,
) -> (String, TurnEnd, Option<String>) {
    interrupt.store(false, Ordering::SeqCst);
    if let Err(problem) = session.send_or_cancel(prompt, cancel) {
        if problem.0 == "launch_cancelled" {
            return (String::new(), TurnEnd::Cancelled, None);
        }
        return (String::new(), TurnEnd::Closed, Some(problem.0));
    }
    let deadline = limit.map(|limit| Instant::now() + limit);
    let mut text = String::new();
    let mut limited = false;
    let mut interrupted = false;
    let mut progress = Instant::now();
    let mut cause = None;
    let end = loop {
        if cancel.load(Ordering::SeqCst) {
            break TurnEnd::Cancelled;
        }
        if interrupt.swap(false, Ordering::SeqCst) {
            let _ = session.interrupt();
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            cause = Some(TURN_LIMIT.to_owned());
            break TurnEnd::TimedOut;
        }
        if progress.elapsed() >= PROGRESS_EVERY {
            progress = Instant::now();
            let _ = reports.send(Report::Progress { bytes: text.len() });
        }
        match session.next_event(EVENT_WAIT) {
            Ok(None) => {}
            Ok(Some(LaunchEvent::Text { text: part })) => {
                if text.len() + part.len() > super::RESULT_BYTES_MAX * 4 {
                    break TurnEnd::TooLong;
                }
                text.push_str(&part);
            }
            Ok(Some(LaunchEvent::TurnError { rate_limited })) => limited |= rate_limited,
            Ok(Some(LaunchEvent::Usage { tokens })) => {
                let _ = reports.send(Report::Usage { tokens });
            }
            Ok(Some(LaunchEvent::Interrupted)) => interrupted = true,
            Ok(Some(LaunchEvent::TurnFinished { success })) => {
                break if success {
                    TurnEnd::Success
                } else if interrupted {
                    TurnEnd::Interrupted
                } else if limited {
                    TurnEnd::RateLimited
                } else {
                    TurnEnd::Failed
                };
            }
            Ok(Some(LaunchEvent::Exited { .. })) => break TurnEnd::Closed,
            Err(problem) => {
                let (end, code) = launch_end(problem);
                cause = code;
                break end;
            }
            Ok(Some(_)) => {}
        }
    };
    assert!(text.len() <= super::RESULT_BYTES_MAX * 4);
    (text, end, cause)
}

/// Background loop that keeps every project's team moving.
pub struct TeamService {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TeamService {
    /// Starts the loop on its own thread, ticking at `tick`.
    /// Takes an opened runtime.
    /// Returns the running service; stopping it stops every agent process group.
    pub fn start(mut runtime: TeamRuntime, tick: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("team-runtime".into())
            .spawn(move || {
                let mut last = None;
                while !flag.load(Ordering::SeqCst) {
                    let problem = runtime.tick().err().map(|e| e.0);
                    if problem.is_some() && problem != last {
                        eprintln!(
                            "team runtime: {}",
                            bound_text(problem.as_deref().unwrap_or_default(), 300)
                        );
                    }
                    last = problem;
                    std::thread::sleep(tick);
                }
                if let Err(problem) = runtime.shutdown() {
                    eprintln!("team runtime shutdown: {}", bound_text(&problem.0, 300));
                }
            })
            .ok();
        Self { stop, thread }
    }

    /// Stops the loop and waits for its thread. Agents in panes keep running for the next
    /// engine; every other agent process group stops.
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for TeamService {
    fn drop(&mut self) {
        self.halt();
    }
}

/// The private home one team agent's harness runs with as its CLAUDE_CONFIG_DIR or
/// CODEX_HOME. Inputs: the team state folder (`<runtime>/team`) and the agent id.
/// Output: `<state>/agents/agent-<id>/home`.
pub fn agent_home(state: &Path, id: i64) -> PathBuf {
    state
        .join("agents")
        .join(format!("agent-{id}"))
        .join("home")
}

/// Starts the team runtime for a running engine: team state in `<runtime>/team`,
/// pool assignment read live from the engine's settings files on every tick.
/// Takes the runtime folder, both settings files, the launch executables by harness, the
/// engine executable that serves masters their `delegate` tool, the engine's agent register
/// that lists every team agent with its owner controls and the terminal host agents run in.
/// Returns the service, or a plain reason.
pub fn start_for_engine(
    runtime: &Path,
    global: &Path,
    project: &Path,
    executables: BTreeMap<&'static str, PathBuf>,
    engine: Option<&Path>,
    agents: Option<Arc<Agents>>,
    terminal: Option<Arc<dyn Host>>,
) -> Result<TeamService, String> {
    let runtime = runtime
        .canonicalize()
        .map_err(|e| format!("team runtime: {e}"))?;
    let state = runtime.join("team");
    private_directory(&state).map_err(|e| e.0)?;
    let (global, project) = (global.to_path_buf(), project.to_path_buf());
    let access: AccessSource = Box::new(move || {
        crate::config::resolve_config(&global, &project)
            .ok()?
            .settings
            .model_access
    });
    let runtime = TeamRuntime::open(
        &runtime.join("engine.db"),
        &state,
        executables,
        access,
        TeamLimits::default(),
    )
    .map_err(|e| e.0)?
    .with_events(super::master::EVENTS);
    let runtime = match engine {
        Some(engine) => runtime.with_delegation_tool(engine).map_err(|e| e.0)?,
        None => runtime,
    };
    let runtime = match agents {
        Some(agents) => runtime.with_agents(agents),
        None => runtime,
    };
    let runtime = match terminal {
        Some(host) => runtime.with_terminal(host).map_err(|e| e.0)?,
        None => runtime,
    };
    let runtime = runtime.with_events(super::events::HANDLERS);
    Ok(TeamService::start(runtime, Duration::from_millis(250)))
}

/// Gives a fresh master the recent team conversation before one message.
/// Takes a connection, the project, the id of the message being sent and a byte bound.
/// Returns owner and master lines plus engine messages other than notes, oldest first,
/// keeping the newest lines that fit within the bound. Master and engine text sits on lines
/// of its own under its speaker, every line quoted by `events::agent_text`, since it can
/// repeat what a sub-agent wrote.
pub fn conversation_before(
    connection: &Connection,
    project: &ProjectId,
    before: i64,
    max: usize,
) -> Result<String, EngineError> {
    let mut statement = connection
        .prepare(
            "SELECT author,text FROM team_messages WHERE project=?1 AND id<?2 AND state<>'note'
             ORDER BY id DESC LIMIT ?3",
        )
        .map_err(error)?;
    let rows: Vec<(String, String)> = statement
        .query_map(
            params![project.as_str(), before, HISTORY_MESSAGES_MAX],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)?;
    let mut lines = Vec::new();
    let mut used = 0;
    for (author, text) in rows {
        let line = match author.as_str() {
            "owner" => format!(
                "Owner: {}\n",
                bound_text(text.trim(), HISTORY_MESSAGE_BYTES_MAX)
            ),
            author => format!(
                "{}:\n{}\n",
                if author == "master" {
                    "Master"
                } else {
                    "Engine"
                },
                bound_text(
                    &super::events::agent_text(text.trim()),
                    HISTORY_MESSAGE_BYTES_MAX
                )
            ),
        };
        if used + line.len() > max {
            break;
        }
        used += line.len();
        lines.push(line);
    }
    lines.reverse();
    let history = lines.concat();
    assert!(history.len() <= max);
    Ok(history)
}

/// Reads the team agents of a project directly from the engine database.
/// Takes a connection and the project.
/// Returns every agent row in creation order.
pub fn agents(
    connection: &Connection,
    project: &ProjectId,
) -> Result<Vec<super::TeamAgent>, EngineError> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT {AGENT_COLUMNS} FROM team_agents WHERE project=?1 ORDER BY id"
        ))
        .map_err(error)?;
    statement
        .query_map([project.as_str()], agent_row)
        .map_err(error)?
        .collect::<Result<_, _>>()
        .map_err(error)
}

#[cfg(all(test, unix))]
#[path = "runtime/tests.rs"]
mod tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "runtime/managed_agent.rs"]
pub(crate) mod managed_agent;

#[cfg(all(test, target_os = "macos"))]
#[path = "runtime/dec_090_tests.rs"]
mod dec_090_tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "runtime/live_claude_tests.rs"]
mod live_claude_tests;
#[cfg(all(test, target_os = "macos"))]
#[path = "runtime/live_codex_tests.rs"]
mod live_codex_tests;
