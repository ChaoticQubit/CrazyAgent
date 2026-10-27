use super::{
    AGENT_COLUMNS, Accounts, AgentRole, AgentStatus, DELEGATIONS_PER_REPLY_MAX, Delegation,
    MessageAuthor, MessageState, NOTE_BYTES_MAX, TaskSource, TaskStatus, TeamAccount, TeamReach,
    admit, agent_row, append_message, bound_text, eligible_accounts, launch_harness, names, now_ms,
    parse_delegations, pick_account, tool,
};
use crate::agents::{
    AgentState as RunState, Agents, Hosted, HostedAgent, HostedRequest, HostedRestart,
};
use crate::config::ModelAccess;
use crate::engine::bootstrap::{NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::owner::HostStamp;
use crate::engine::owner::native_auth::NativeAuthFact;
use crate::engine::{EngineError, NativeProfileCustody, RegisteredNativeAuthHost};
use crate::harnesses::launch::{
    AccountBinding, GitIdentity, LaunchError, LaunchEvent, LaunchLimits, LaunchRequest,
};
use crate::harnesses::terminal_session::{AgentSession, PaneRecord};
use crate::launch_context::AgentContext;
use crate::project_setup::{StoredSetup, load_setup};
use crate::protocol::{CredentialRef, ProjectId, RequestId};
use crate::research::runner::{
    ResearchAccount, RunError, add_worktree, confined_unavailable, ensure_repository, model_remote,
};
use crate::sandbox::toolchains::{self, ToolHomes, Toolchains};
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
const TEAM_HOST: &str = "team";
const SIGN_IN_REJECTED: &str = "master_sign_in_rejected";
const LEFT_RUNNING_NOTE: &str = "Kept running while CrazyAgent was closed";
const EMPTY_REPLY_NOTE: &str = "The master replied with no text, so no work was handed out. Send the message again, or hand a task to a sub-agent yourself.";

/// Bounds every team agent obeys.
/// `turn_time` caps one master or sub-agent turn, `session_time` a whole agent process,
/// `output_bytes` the harness output of one process, `attempts_max` the tries per task,
/// `idle_stop` and `master_idle_stop` how long an idle agent is kept and
/// `rate_limit_pause` how long an account that hit its limit is skipped.
#[derive(Debug, Clone, Copy)]
pub struct TeamLimits {
    pub turn_time: Duration,
    pub session_time: Duration,
    pub output_bytes: usize,
    pub attempts_max: i64,
    pub idle_stop: Duration,
    pub master_idle_stop: Duration,
    pub rate_limit_pause: Duration,
    pub stop_grace: Duration,
}

impl Default for TeamLimits {
    fn default() -> Self {
        Self {
            turn_time: Duration::from_secs(15 * 60),
            session_time: Duration::from_secs(4 * 60 * 60),
            output_bytes: 8 * 1024 * 1024,
            attempts_max: 2,
            idle_stop: Duration::from_secs(120),
            master_idle_stop: Duration::from_secs(15 * 60),
            rate_limit_pause: Duration::from_secs(10 * 60),
            stop_grace: Duration::from_secs(2),
        }
    }
}

impl TeamLimits {
    fn validate(&self) -> Result<(), EngineError> {
        let ok = !self.turn_time.is_zero()
            && self.turn_time <= self.session_time
            && self.session_time <= Duration::from_secs(86_400)
            && (1..=64 * 1024 * 1024).contains(&self.output_bytes)
            && (1..=10).contains(&self.attempts_max)
            && !self.idle_stop.is_zero()
            && !self.master_idle_stop.is_zero()
            && self.stop_grace <= Duration::from_secs(30);
        ok.then_some(())
            .ok_or_else(|| EngineError("team_limits_invalid".into()))
    }
}

enum Order {
    Turn(String, Option<Box<Grant>>),
    Stop,
}

/// What one turn of an agent on a signed-in managed account needs: the profile, held
/// under the engine's custody lock for that turn only, and the project's launch context.
struct Grant {
    profile: NativeProfileUse,
    context: AgentContext,
}

/// Everything about an agent's harness launch except the account, so a managed agent
/// can start a fresh process for each turn. `pane` is the terminal host and the agent's
/// pane name when the agent runs its interactive app in a pane.
struct Plan {
    harness: String,
    executable: PathBuf,
    working_dir: PathBuf,
    model: Option<String>,
    tools: bool,
    sandbox_profile: PathBuf,
    git_identity: Option<GitIdentity>,
    limits: LaunchLimits,
    pane: Option<(Arc<dyn Host>, AgentId)>,
}

impl Plan {
    fn request(&self, account: AccountBinding) -> LaunchRequest {
        LaunchRequest {
            harness: self.harness.clone(),
            executable: self.executable.clone(),
            working_dir: self.working_dir.clone(),
            account,
            model: self.model.clone(),
            tools: self.tools,
            sandbox_profile: Some(self.sandbox_profile.clone()),
            git_identity: self.git_identity.clone(),
            limits: self.limits,
        }
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

enum Report {
    Ready {
        group: u32,
        pane: Option<PaneRecord>,
    },
    Progress {
        bytes: usize,
    },
    Finished {
        text: String,
        end: TurnEnd,
        cause: Option<String>,
    },
    Ended {
        problem: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    Task(i64),
    Message(i64),
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
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
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
        })
    }

    /// Records every team agent in the engine's agent register, so the owner can stop,
    /// interrupt and restart each one with every other agent.
    /// Takes the register of the same engine database. Returns the runtime.
    pub fn with_agents(mut self, agents: Arc<Agents>) -> Self {
        self.agents = Some(agents);
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
                match host.kill(&agent) {
                    Ok(()) => {}
                    Err(problem) if problem.0 == "terminal_agent_unknown" => {}
                    Err(problem) => return Err(problem.into()),
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
    /// room, and its account is still an endpoint in the project's pool. Takes the host, the
    /// agent id, its pane record and the live access. Returns Ok, or why the pane must end.
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
                stored.team.master == harness
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
        let endpoint = matches!(
            eligible_accounts(access, &project, launch),
            Accounts::Usable(accounts) if accounts.iter().any(|account| {
                account.connection == connection
                    && matches!(account.reach, TeamReach::Endpoint { .. })
            })
        );
        if !endpoint {
            return Ok(Err("its account is no longer in the project's pool".into()));
        }
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
            match host.kill(&agent) {
                Ok(()) => self.closed_note(id, "its agent had stopped")?,
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
        self.owner_controls();
        let access = (self.access)().unwrap_or_default();
        self.owner_restarts(&access)?;
        for project in self.active_projects()? {
            self.step(&project, &access)?;
        }
        self.collect()?;
        self.owner_controls();
        self.note_conversations()?;
        self.sweep()
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
    /// restart returns it to its queue for the fresh process.
    fn owner_controls(&mut self) {
        for worker in self.workers.values_mut() {
            let Some(hosted) = &worker.hosted else {
                continue;
            };
            hosted.set_busy(worker.busy.is_some());
            let asked = hosted.requested();
            if asked == Some(HostedRequest::Restart) && !worker.held {
                worker.held = true;
                worker.restarting = true;
            } else if asked != Some(HostedRequest::Stop) || worker.stopping {
                continue;
            }
            worker.stopping = true;
            worker.cancel.store(true, Ordering::SeqCst);
            let _ = worker.orders.send(Order::Stop);
        }
    }

    fn owner_restarts(&mut self, access: &ModelAccess) -> Result<(), EngineError> {
        let Some(agents) = self.agents.clone() else {
            return Ok(());
        };
        for restart in agents.take_restarts(TEAM_HOST) {
            self.restart_agent(&restart, access)?;
        }
        Ok(())
    }

    /// Starts a fresh process for a team agent the owner restarted, under the same register
    /// entry, when the team plan and the pool still allow it; otherwise notes why not.
    fn restart_agent(
        &mut self,
        restart: &HostedRestart,
        access: &ModelAccess,
    ) -> Result<(), EngineError> {
        let Ok(reference) = restart.reference.parse::<i64>() else {
            return Ok(());
        };
        let row: Option<(String, String, String)> = self
            .connection
            .query_row(
                "SELECT project,role,harness FROM team_agents WHERE id=?1",
                [reference],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(error)?;
        let Some((project, role, harness)) = row else {
            return Ok(());
        };
        let (Ok(project), Some(role)) = (ProjectId::new(project), AgentRole::parse(&role)) else {
            return Ok(());
        };
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
            Ok(account) => {
                self.spawn_run(&stored, role, &harness, tools, &account, Some(restart))?
            }
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
                 UNION SELECT project FROM team_tasks WHERE status IN ('queued','needs_pool','running') OR (delivered=0 AND source='master' AND status IN ('done','failed','cancelled'))
                 UNION SELECT project FROM team_settings WHERE keep_warm=1
                 UNION SELECT project FROM team_agents WHERE status IN ('starting','idle','working')",
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
        let paused = self.paused()?;
        self.retire_rejected(project, access);
        self.sync_pool(&stored, access)?;
        self.request_cancels(project)?;
        self.feed_master(&stored, access, &paused)?;
        self.deliver(&stored)?;
        self.dispatch(&stored, access, &paused)?;
        self.keep_warm(&stored, access, &paused)?;
        self.reap_idle(&stored)
    }

    fn sync_pool(&mut self, stored: &StoredSetup, access: &ModelAccess) -> Result<(), EngineError> {
        for slot in &stored.team.sub_harnesses {
            let Some(launch) = launch_harness(&slot.harness) else {
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
                    params![stored.project_id.as_str(), slot.harness, to, from],
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
        let context = self.context(project, id, self.workers[&id].role, self.workers[&id].tools)?;
        self.workers.get_mut(&id).expect("worker exists").grants = generation;
        Ok(Some(Box::new(Grant { profile, context })))
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

    fn feed_master(
        &mut self,
        stored: &StoredSetup,
        access: &ModelAccess,
        paused: &BTreeSet<CredentialRef>,
    ) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let restarting = self
            .workers
            .values()
            .any(|w| &w.project == project && w.role == AgentRole::Master && w.restarting);
        let master = self.master(project);
        if restarting || master.is_some_and(|id| self.workers[&id].busy.is_some()) {
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
                    Ok(account) => self.spawn(
                        stored,
                        AgentRole::Master,
                        &harness,
                        stored.team.master_tools,
                        &account,
                    )?,
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
        if fresh {
            prompt.push_str(&framing(stored, team_tool));
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
        let worker = self.workers.get_mut(&id).expect("master worker exists");
        if worker.orders.send(Order::Turn(prompt, grant)).is_err() {
            return Ok(());
        }
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
        type ReadyTask = (i64, String, String, Option<String>, Option<String>);
        let ready: Vec<ReadyTask> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT t.id,t.harness,t.status,t.result,t.error FROM team_tasks t
                     WHERE t.project=?1 AND t.delivered=0 AND t.source='master' AND t.status IN ('done','failed','cancelled')
                     AND NOT EXISTS (SELECT 1 FROM team_tasks o WHERE o.project=t.project AND o.turn IS t.turn AND t.turn IS NOT NULL
                                     AND o.status IN ('queued','needs_pool','running'))
                     ORDER BY t.id LIMIT 16",
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
        if ready.is_empty() {
            return Ok(());
        }
        let mut text = String::from("Results from your sub-agents:\n");
        for (id, harness, status, result, problem) in &ready {
            let label = super::TaskStatus::parse(status).map_or("Finished", TaskStatus::label);
            text.push_str(&format!("\nTask {id} ({harness}): {label}"));
            if let Some(problem) = problem {
                text.push_str(&format!(" - {problem}"));
            }
            text.push('\n');
            if let Some(result) = result {
                text.push_str(&bound_text(result.trim(), RESULT_IN_MESSAGE_BYTES_MAX));
                text.push('\n');
            }
        }
        text.push_str("\nTell the owner what was done.");
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
            for (id, ..) in &ready {
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
        let queued: Vec<(i64, String, String, Option<i64>)> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT t.id,t.harness,t.prompt,p.agent FROM team_tasks t
                     LEFT JOIN team_task_pins p ON p.task=t.id AND p.project=t.project
                     WHERE t.project=?1 AND t.status='queued' ORDER BY t.id LIMIT 64",
                )
                .map_err(error)?;
            statement
                .query_map([project.as_str()], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        let subs_max = usize::from(stored.team.max_agents.saturating_sub(1));
        for (task, harness, prompt, pin) in queued {
            let Some(slot) = stored
                .team
                .sub_harnesses
                .iter()
                .find(|s| s.harness == harness)
            else {
                self.fail_task(task, "The team plan no longer has this harness")?;
                continue;
            };
            let idle = self
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
            let agent = match (idle, pin) {
                (Some(agent), _) => agent,
                (None, Some(pinned)) if self.workers.contains_key(&pinned) => continue,
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
                    match self.revive(stored, pinned, &harness, slot.tools, &account)? {
                        Ok(agent) => agent,
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
                        Ok(agent) => {
                            assert!(self.live(project, AgentRole::Sub, None) <= subs_max);
                            agent
                        }
                        Err(problem) => {
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
            let prompt = format!(
                "You are a sub-agent of the project \"{}\". Work only inside {}.\nYour task: {}\nWhen you finish, reply with a short summary of what you did.",
                stored.name,
                worktree
                    .map_or(folder.clone(), |worktree| folder.join(worktree))
                    .display(),
                prompt
            );
            let worker = self.workers.get_mut(&agent).expect("worker exists");
            if worker.orders.send(Order::Turn(prompt, grant)).is_err() {
                continue;
            }
            worker.busy = Some(Busy::Task(task));
            self.connection
                .execute(
                    "UPDATE team_tasks SET status='running', agent=?2, attempts=attempts+1, started_ms=?3, error=NULL WHERE id=?1",
                    params![task, agent, now_ms()],
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
                let started =
                    self.spawn(stored, AgentRole::Sub, &slot.harness, slot.tools, &account)?;
                if let Ok(agent) = started {
                    self.workers.get_mut(&agent).expect("worker exists").warm = true;
                }
                if let Err(problem) = started {
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
        Ok(())
    }

    fn reap_idle(&mut self, stored: &StoredSetup) -> Result<(), EngineError> {
        let project = &stored.project_id;
        let warm = super::keep_warm(&self.connection, project)?;
        let floor = usize::from(stored.team.min_agents.saturating_sub(1));
        let mut live_subs = self.live(project, AgentRole::Sub, None);
        let ids: Vec<i64> = self
            .workers
            .iter()
            .filter(|(_, w)| &w.project == project && w.busy.is_none() && !w.stopping)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let worker = self.workers.get_mut(&id).expect("worker exists");
            let limit = match worker.role {
                AgentRole::Master => self.limits.master_idle_stop,
                AgentRole::Sub => self.limits.idle_stop,
            };
            if worker.idle_since.elapsed() < limit {
                continue;
            }
            if worker.role == AgentRole::Sub {
                if warm && live_subs <= floor {
                    continue;
                }
                live_subs -= 1;
            }
            worker.stopping = true;
            let _ = worker.orders.send(Order::Stop);
        }
        Ok(())
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
    ) -> Result<Result<i64, String>, EngineError> {
        self.spawn_run(stored, role, harness, tools, account, None)
    }

    fn spawn_run(
        &mut self,
        stored: &StoredSetup,
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
        let now = now_ms();
        self.connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,connection,status,created_ms,heartbeat_ms) VALUES (?1,?2,?3,?4,?5,'starting',?6,?6)",
                params![stored.project_id.as_str(), role.name(), harness, tools, account.connection.as_str(), now],
            )
            .map_err(error)?;
        let id = self.connection.last_insert_rowid();
        assert!(id > 0);
        if role == AgentRole::Sub {
            names::assign_persona(&self.connection, &stored.project_id, id)?;
        }
        self.launch(stored, id, role, harness, tools, account, again, None)
    }

    /// Starts a stopped sub-agent again under its own id in the worktree it left, for a task
    /// pinned to it, so that task's end lands in the same change set (DEC-095). Takes the
    /// setup, the sub-agent, its harness and tools switch and the account to run on.
    /// Returns its id, or the plain reason it could not start; the worktree must still be
    /// the one recorded when the agent was first started.
    fn revive(
        &mut self,
        stored: &StoredSetup,
        id: i64,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
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
            return Ok(Err("the sub-agent's worktree is no longer known".into()));
        };
        let changed = self
            .connection
            .execute(
                "UPDATE team_agents SET status='starting', connection=?3, task=NULL, error=NULL, stopped_ms=NULL, heartbeat_ms=?4
                 WHERE id=?1 AND project=?2 AND role='sub' AND status IN ('stopped','failed')",
                params![id, stored.project_id.as_str(), account.connection.as_str(), now_ms()],
            )
            .map_err(error)?;
        if changed != 1 {
            return Ok(Err("the sub-agent is not stopped".into()));
        }
        names::assign_persona(&self.connection, &stored.project_id, id)?;
        let kept = [repository, worktree, git_dir];
        self.launch(
            stored,
            id,
            AgentRole::Sub,
            harness,
            tools,
            account,
            None,
            Some(kept),
        )
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
        let prepared = self.prepare(stored, id, role, harness, tools, account, kept.is_some());
        let (root, plan, mode) = match prepared {
            Ok(prepared) => prepared,
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
        let worktree = (plan.working_dir != root)
            .then(|| {
                plan.working_dir
                    .strip_prefix(&root)
                    .ok()
                    .map(|p| p.display().to_string())
            })
            .flatten();
        let landing = match agent_git_dir(&root, &plan.working_dir) {
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
        let turn_time = self.limits.turn_time;
        let managed =
            matches!(&launch, Launch::Fresh(_, mode) if matches!(**mode, Mode::PerTurn { .. }));
        let thread = std::thread::Builder::new()
            .name(format!("team-agent-{id}"))
            .spawn(move || work(launch, inbox, outbox, flag, interrupt, kept, turn_time))
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
            },
        );
        assert!(self.panes(&self.workers[&id].project) <= PANES_PER_PROJECT_MAX);
        self.swept.get_or_insert_with(Instant::now);
        Ok(())
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
        let own_home = agent.join("home");
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
        let context = self.context(&stored.project_id, id, role, tools)?;
        let network = if tools {
            SandboxNetwork::Open
        } else {
            SandboxNetwork::Only(vec![remote])
        };
        let homes = ToolHomes::owner().map_err(sandboxed)?;
        let scratch = toolchains::make_tmp(&home)
            .map_err(|e| with_code("the agent temp folder could not be made", Some(io_code(&e))))?;
        toolchains::prepare_cargo_home(&home, &homes)
            .map_err(|e| with_code("the agent cargo home could not be made", Some(io_code(&e))))?;
        let mut sandbox = ProjectSandbox::new(&root, &home, &tmp, network).map_err(sandboxed)?;
        if let Some(runtime) = self.state.parent() {
            sandbox = sandbox.guard_engine(runtime).map_err(sandboxed)?;
        }
        let extra = crate::sandbox::extra_paths::load(&self.connection, stored.project_id.as_str())
            .map_err(|_| with_code("the project's read-only folders could not be read", None))?;
        (sandbox, _) = sandbox
            .allow_tools(&Toolchains::detect(&root, &homes))
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
        if !adopt {
            add_worktree(&sandbox, &profile, &worktree).map_err(|e| {
                with_code("the agent worktree could not be added", Some(run_code(&e)))
            })?;
        }
        let working_dir = worktree
            .canonicalize()
            .map_err(|e| with_code("the agent worktree could not be opened", Some(io_code(&e))))?;
        if working_dir != worktree {
            return Err("the agent worktree is not a plain folder in the project".into());
        }
        assert!(working_dir.starts_with(&root) && working_dir != root);
        sandbox = sandbox
            .confine_to_worktree(&working_dir)
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
            git_identity: owner_git_identity(&root),
            limits: self.launch_limits(),
            pane: self.terminal.clone().zip(AgentId::team(id).ok()),
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
        Ok((root, plan, mode))
    }

    fn launch_limits(&self) -> LaunchLimits {
        LaunchLimits {
            run_time: self.limits.session_time,
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

    /// Reads what one team agent launch receives: the project context and approved
    /// memories, the approved skills and MCP servers with tools on, the engine's `delegate`
    /// tool for a master with tools on, served by the captured engine while the file at its
    /// path is still that one, and the team's skill and server names for a master with tools
    /// off. Takes the project, the agent id, its role and tools switch.
    fn context(
        &self,
        project: &ProjectId,
        id: i64,
        role: AgentRole,
        tools: bool,
    ) -> Result<AgentContext, String> {
        let unread =
            |e: LaunchError| with_code("the project context could not be read", Some(&e.0));
        let context =
            crate::launch_context::for_project(&self.connection, project).map_err(unread)?;
        let master = role == AgentRole::Master;
        match &self.delegation_tool {
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

    fn apply(&mut self, id: i64, report: Report) -> Result<bool, EngineError> {
        match report {
            Report::Ready { group, pane } => {
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
            Report::Finished { text, end, cause } => {
                let worker = self.workers.get_mut(&id).expect("worker exists");
                let busy = worker.busy.take();
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
            Report::Ended { problem } => {
                let mut worker = self.workers.remove(&id).expect("worker exists");
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
                let status = if problem.is_some() && !worker.stopping {
                    AgentStatus::Failed
                } else {
                    AgentStatus::Stopped
                };
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
                format!("task {task} ({harness}, sub-agent {id})"),
            ),
            Busy::Message(message) => (
                format!("message-{message}"),
                prompt("SELECT text FROM team_messages WHERE id=?1", message)?,
                format!("the team master's turn ({harness}, agent {id})"),
            ),
        };
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
                format!("The edits from {what} are waiting for you in Changes.")
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
            TurnEnd::TimedOut => (TaskStatus::Failed, Some("The sub-agent ran out of time")),
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
        self.connection
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
        if !delegations.is_empty() && rounds >= DELEGATION_ROUNDS_MAX {
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
        tx.commit().map_err(error)
    }
}

/// Builds the master's opening frame: its sub-agents, how to hand out work
/// and the project brief. Input: the setup and whether the team tool is on.
/// Output: the frame text, with the brief from `master_brief`.
fn framing(stored: &StoredSetup, team_tool: bool) -> String {
    let brief = master_brief(stored);
    let team = stored
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
        .join("\n");
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
        "You are the master agent of the project \"{}\". Sub-agents do the hands-on work.\nYour sub-agents:\n{}\n{how}Each task runs in its own git worktree of the project folder, and the engine sends you the results.\n\nProject brief:\n{}\n\n",
        stored.name, team, brief
    )
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
    turn_time: Duration,
) {
    let started = match launch {
        Launch::Fresh(plan, mode) => match *mode {
            Mode::Kept { account, context } => {
                AgentSession::start(plan.request(account), context, plan.pane.clone())
                    .map_err(|problem| with_code("The agent could not start", Some(&problem.0)))
            }
            Mode::PerTurn { home } => {
                let problem = per_turn(
                    &plan, &home, &orders, &reports, &cancel, &interrupt, turn_time,
                );
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
        Ok(session) => serve(
            session, &orders, &reports, &cancel, &interrupt, &leave, turn_time,
        ),
        Err(problem) => Some(problem),
    };
    let _ = reports.send(Report::Ended { problem });
}

enum Next {
    Turn(String, Option<Box<Grant>>),
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
            Ok(Order::Turn(prompt, grant)) => return Next::Turn(prompt, grant),
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
/// leaves the pane running for the next engine. A stop its warden did not confirm ends the
/// agent with that problem.
fn serve(
    mut session: AgentSession,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    interrupt: &AtomicBool,
    leave: &AtomicBool,
    turn_time: Duration,
) -> Option<String> {
    let _ = reports.send(Report::Ready {
        group: session.process_group(),
        pane: session.pane_record(),
    });
    let mut problem = None;
    loop {
        let (prompt, grant) = match next_turn(orders, cancel, Some(&mut session)) {
            Next::Turn(prompt, grant) => (prompt, grant),
            Next::Stop => break,
            Next::Closed(cause) => {
                problem = Some(with_code(PROCESS_CLOSED, cause.as_deref()));
                break;
            }
        };
        assert!(grant.is_none());
        let (text, end, cause) = turn(&mut session, &prompt, reports, cancel, interrupt, turn_time);
        let reusable = matches!(
            end,
            TurnEnd::Success | TurnEnd::Failed | TurnEnd::Interrupted
        );
        if end == TurnEnd::Closed {
            problem = Some(with_code(PROCESS_CLOSED, cause.as_deref()));
        }
        if reports.send(Report::Finished { text, end, cause }).is_err() || !reusable {
            break;
        }
    }
    if problem.is_none() && leave.load(Ordering::SeqCst) {
        let _ = session.interrupt();
        session.leave();
    } else if let Err(uncertain) = session.stop() {
        problem = Some(with_code(
            crate::sandbox::warden::CLEANUP_UNCERTAIN,
            Some(&uncertain.0),
        ));
    }
    problem
}

/// Serves turns on an agent of a signed-in account, starting its harness for each turn with
/// the account's credential copy and lease, both released once the turn's harness stops. A
/// stop its warden did not confirm keeps both for the engine's life and ends the agent.
fn per_turn(
    plan: &Plan,
    home: &Path,
    orders: &Receiver<Order>,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    interrupt: &AtomicBool,
    turn_time: Duration,
) -> Option<String> {
    while let Next::Turn(prompt, grant) = next_turn(orders, cancel, None) {
        let Some(grant) = grant else {
            let _ = reports.send(Report::Finished {
                text: String::new(),
                end: TurnEnd::Closed,
                cause: None,
            });
            return Some("The agent was given a turn without its signed-in account".into());
        };
        let Grant { profile, context } = *grant;
        let credential = match profile.hand_to_agent(home, turn_time, &plan.executable) {
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
        let request = plan.request(AccountBinding::Subscription {
            home: home.to_path_buf(),
        });
        let mut session = match AgentSession::start(request, context, plan.pane.clone()) {
            Ok(session) => session,
            Err(problem) => {
                let _ = reports.send(Report::Finished {
                    text: String::new(),
                    end: TurnEnd::Closed,
                    cause: Some(problem.0.clone()),
                });
                return Some(with_code("The agent could not start", Some(&problem.0)));
            }
        };
        let _ = reports.send(Report::Ready {
            group: session.process_group(),
            pane: None,
        });
        let (text, end, cause) = turn(&mut session, &prompt, reports, cancel, interrupt, turn_time);
        let stopped = session.stop();
        let reusable = stopped.is_ok()
            && matches!(
                end,
                TurnEnd::Success | TurnEnd::Failed | TurnEnd::Interrupted
            );
        let closed = match stopped {
            Ok(_) => {
                drop(credential);
                drop(profile);
                (end == TurnEnd::Closed).then(|| with_code(PROCESS_CLOSED, cause.as_deref()))
            }
            Err(uncertain) => {
                credential.retain();
                std::mem::forget(profile);
                Some(with_code(
                    crate::sandbox::warden::CLEANUP_UNCERTAIN,
                    Some(&uncertain.0),
                ))
            }
        };
        if reports.send(Report::Finished { text, end, cause }).is_err() || !reusable {
            return closed;
        }
    }
    None
}

const PROCESS_CLOSED: &str = "The agent process closed";
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
const WARM_STOPPED: &str = "Keeping sub-agents warm was turned off because a warm sub-agent stopped before it did any work";

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

/// Reads where Git keeps a new agent worktree's metadata while no agent has run in it yet,
/// so the engine's later commits never rely on what the agent may write: a folder directly
/// in the project's `.git/worktrees` whose `gitdir` file names this worktree. Inputs: the
/// project root and the worktree. Output: the root, worktree and metadata folder as text,
/// or the plain reason.
fn agent_git_dir(root: &Path, worktree: &Path) -> Result<[String; 3], String> {
    use std::io::Read;
    let read = |path: &Path| -> Result<String, String> {
        let mut text = String::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(4096).read_to_string(&mut text))
            .map_err(|e| with_code("the agent worktree could not be checked", Some(io_code(&e))))?;
        Ok(text)
    };
    let refused = || {
        with_code(
            "the agent worktree could not be checked",
            Some("team_worktree_git_dir"),
        )
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
    }
}

/// How a turn ends on a launch error: the deadline and the output limits keep their own
/// ends; any other error closes the agent and keeps its code.
/// Input: the error. Output: the end and, for a closed agent, the launch code.
fn launch_end(problem: LaunchError) -> (TurnEnd, Option<String>) {
    match problem.0.as_str() {
        "launch_deadline" => (TurnEnd::TimedOut, None),
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
    turn_time: Duration,
) -> (String, TurnEnd, Option<String>) {
    interrupt.store(false, Ordering::SeqCst);
    if let Err(problem) = session.send_or_cancel(prompt, cancel) {
        if problem.0 == "launch_cancelled" {
            return (String::new(), TurnEnd::Cancelled, None);
        }
        return (String::new(), TurnEnd::Closed, Some(problem.0));
    }
    let deadline = Instant::now() + turn_time;
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
        if Instant::now() >= deadline {
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
    .map_err(|e| e.0)?;
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
    Ok(TeamService::start(runtime, Duration::from_millis(250)))
}

/// Gives a fresh master the recent team conversation before one message.
/// Takes a connection, the project, the id of the message being sent and a byte bound.
/// Returns owner and master lines plus engine messages other than notes, oldest first,
/// keeping the newest lines that fit within the bound.
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
        let speaker = match author.as_str() {
            "owner" => "Owner",
            "master" => "Master",
            _ => "Engine",
        };
        let line = format!(
            "{speaker}: {}\n",
            bound_text(text.trim(), HISTORY_MESSAGE_BYTES_MAX)
        );
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
