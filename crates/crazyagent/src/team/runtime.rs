use super::{
    AGENT_COLUMNS, Accounts, AgentRole, AgentStatus, MessageAuthor, MessageState, TaskSource,
    TaskStatus, TeamAccount, admit, agent_row, append_message, bound_text, eligible_accounts,
    launch_harness, now_ms, parse_delegations, pick_account,
};
use crate::config::ModelAccess;
use crate::engine::EngineError;
use crate::harnesses::launch::{AccountBinding, LaunchEvent, LaunchLimits, LaunchRequest, Session};
use crate::project_setup::{StoredSetup, load_setup};
use crate::protocol::{CredentialRef, ProjectId};
use crate::research::runner::{ResearchAccount, add_worktree, ensure_repository, model_remote};
use crate::sandbox::{ProjectSandbox, SandboxNetwork};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};
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
const JOIN_WAIT: Duration = Duration::from_secs(15);
const SESSION_EVENTS_MAX: usize = 200_000;
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
    Turn(String),
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnEnd {
    Success,
    Failed,
    RateLimited,
    TimedOut,
    TooLong,
    Cancelled,
    Closed,
}

enum Report {
    Ready { group: u32 },
    Progress { bytes: usize },
    Finished { text: String, end: TurnEnd },
    Ended { problem: Option<String> },
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
    busy: Option<Busy>,
    idle_since: Instant,
    stopping: bool,
    thread: Option<JoinHandle<()>>,
}

/// Reads the live model access on every tick; none means no pool is configured.
pub type AccessSource = Box<dyn Fn() -> Option<ModelAccess> + Send>;

/// Runs each project's team: the master on the team plan's master harness and
/// sub-agents on its sub-harnesses, each in the project sandbox, sub-agents in
/// their own git worktrees. The engine admits every task; agents only propose.
pub struct TeamRuntime {
    connection: Connection,
    state: PathBuf,
    executables: BTreeMap<&'static str, PathBuf>,
    access: AccessSource,
    limits: TeamLimits,
    workers: BTreeMap<i64, Worker>,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

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
                 UPDATE team_agents SET status='stopped', task=NULL, error='The engine restarted', stopped_ms={now}
                   WHERE status IN ('starting','idle','working');
                 UPDATE team_tasks SET status='queued', agent=NULL, cancel_requested=0
                   WHERE status='running' AND cancel_requested=0;
                 UPDATE team_tasks SET status='cancelled', agent=NULL, error='Cancelled by you', finished_ms={now}
                   WHERE status='running' AND cancel_requested=1;
                 UPDATE team_messages SET state='pending' WHERE state='sent';
                 COMMIT;"
            ))
            .map_err(error)?;
        Ok(Self {
            connection,
            state: state.to_path_buf(),
            executables,
            access,
            limits,
            workers: BTreeMap::new(),
        })
    }

    /// Runs one step for every project with team work: applies agent reports, sends
    /// pending messages to the master, delivers finished results, starts or reuses
    /// sub-agents within the plan's limits and stops idle or cancelled agents.
    pub fn tick(&mut self) -> Result<(), EngineError> {
        self.collect()?;
        let access = (self.access)().unwrap_or_default();
        for project in self.active_projects()? {
            self.step(&project, &access)?;
        }
        self.collect()
    }

    /// Tells whether any agent process is still running.
    pub fn has_live_agents(&self) -> bool {
        !self.workers.is_empty()
    }

    /// Stops every agent process group and returns running tasks to the queue.
    /// Waits a bounded time for each agent thread.
    pub fn shutdown(&mut self) -> Result<(), EngineError> {
        for worker in self.workers.values_mut() {
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
            tx.execute(
                "UPDATE team_agents SET status='stopped', task=NULL, stopped_ms=?2 WHERE id=?1 AND status IN ('starting','idle','working')",
                params![id, now],
            )
            .map_err(error)?;
        }
        tx.execute(
            "UPDATE team_messages SET state='pending' WHERE state='sent'",
            [],
        )
        .map_err(error)?;
        tx.commit().map_err(error)
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
            return Ok(());
        };
        let paused = self.paused()?;
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
                Accounts::Usable(accounts) if !accounts.is_empty()
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
                    && !w.stopping
                    && harness.is_none_or(|h| w.harness == h)
            })
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
        if !self.executables.contains_key(launch) {
            return Err(format!("{harness} is not installed"));
        }
        match eligible_accounts(access, project, launch) {
            Accounts::NoPool => Err("Needs a pool".into()),
            Accounts::Usable(accounts) if accounts.is_empty() => Err(format!(
                "No account in this project's pool can run {harness}"
            )),
            Accounts::Usable(accounts) => {
                pick_account(&accounts, &|c: &CredentialRef| self.load(c), paused)
                    .cloned()
                    .ok_or_else(|| {
                        "Every account in this project's pool is resting after a rate limit".into()
                    })
            }
        }
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
        let master = self.master(project);
        if master.is_some_and(|id| self.workers[&id].busy.is_some()) {
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
                        return self
                            .note(project, &format!("The master could not start: {reason}."));
                    }
                }
            }
        };
        let speaker = if author == "owner" {
            "Message from the owner"
        } else {
            "Message from the engine"
        };
        let mut prompt = String::new();
        if fresh {
            prompt.push_str(&self.framing(stored));
        }
        prompt.push_str(&format!("{speaker}:\n{text}"));
        let worker = self.workers.get_mut(&id).expect("master worker exists");
        if worker.orders.send(Order::Turn(prompt)).is_err() {
            return Ok(());
        }
        worker.busy = Some(Busy::Message(message));
        self.connection
            .execute(
                "UPDATE team_messages SET state='sent' WHERE id=?1",
                [message],
            )
            .map_err(error)?;
        self.mark_agent(id, AgentStatus::Working, None)
    }

    fn framing(&self, stored: &StoredSetup) -> String {
        let brief = std::fs::read_to_string(Path::new(&stored.folder).join("context/brief.md"))
            .map(|brief| bound_text(&brief, BRIEF_BYTES_MAX))
            .unwrap_or_default();
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
        format!(
            "You are the master agent of the project \"{}\". Sub-agents do the hands-on work.\nYour sub-agents:\n{}\nTo hand a task to a sub-agent, write it on its own line exactly like this:\nDELEGATE: <the task>\nTo choose a harness, write DELEGATE <harness>: <the task>.\nEach task runs in its own git worktree of the project folder, and the engine sends you the results.\n\nProject brief:\n{}\n\n",
            stored.name, team, brief
        )
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
        let ready: Vec<(i64, String, String, Option<String>, Option<String>)> = {
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
        let queued: Vec<(i64, String, String)> = {
            let mut statement = self
                .connection
                .prepare("SELECT id,harness,prompt FROM team_tasks WHERE project=?1 AND status='queued' ORDER BY id LIMIT 64")
                .map_err(error)?;
            statement
                .query_map([project.as_str()], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?
        };
        let subs_max = usize::from(stored.team.max_agents.saturating_sub(1));
        for (task, harness, prompt) in queued {
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
                .find(|(_, w)| {
                    &w.project == project
                        && w.role == AgentRole::Sub
                        && w.harness == harness
                        && w.busy.is_none()
                        && !w.stopping
                        && !paused.contains(&w.connection)
                })
                .map(|(id, _)| *id);
            let agent = match idle {
                Some(agent) => agent,
                None => {
                    if self.live(project, AgentRole::Sub, Some(&harness))
                        >= usize::from(slot.agents)
                        || self.live(project, AgentRole::Sub, None) >= subs_max
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
            let worktree: Option<String> = self
                .connection
                .query_row(
                    "SELECT worktree FROM team_agents WHERE id=?1",
                    [agent],
                    |row| row.get(0),
                )
                .map_err(error)?;
            let prompt = format!(
                "You are a sub-agent of the project \"{}\". Work only inside {}.\nYour task: {}\nWhen you finish, reply with a short summary of what you did.",
                stored.name,
                worktree.as_deref().unwrap_or(&stored.folder),
                prompt
            );
            let worker = self.workers.get_mut(&agent).expect("worker exists");
            if worker.orders.send(Order::Turn(prompt)).is_err() {
                continue;
            }
            worker.busy = Some(Busy::Task(task));
            self.connection
                .execute(
                    "UPDATE team_tasks SET status='running', agent=?2, attempts=attempts+1, started_ms=?3, error=NULL WHERE id=?1",
                    params![task, agent, now_ms()],
                )
                .map_err(error)?;
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
            {
                let Ok(account) = self.account_for(access, project, &slot.harness, paused) else {
                    break;
                };
                if let Err(problem) =
                    self.spawn(stored, AgentRole::Sub, &slot.harness, slot.tools, &account)?
                {
                    self.connection
                        .execute(
                            "UPDATE team_settings SET keep_warm=0 WHERE project=?1",
                            [project.as_str()],
                        )
                        .map_err(error)?;
                    return self.note(
                        project,
                        &format!("Keeping sub-agents warm was turned off: a sub-agent could not start ({}).", bound_text(&problem, 300)),
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
        let now = now_ms();
        self.connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,connection,status,created_ms,heartbeat_ms) VALUES (?1,?2,?3,?4,?5,'starting',?6,?6)",
                params![stored.project_id.as_str(), role.name(), harness, tools, account.connection.as_str(), now],
            )
            .map_err(error)?;
        let id = self.connection.last_insert_rowid();
        assert!(id > 0);
        let prepared = self.prepare(stored, id, role, harness, tools, account);
        let (root, request) = match prepared {
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
        let worktree = (request.working_dir != root)
            .then(|| {
                request
                    .working_dir
                    .strip_prefix(&root)
                    .ok()
                    .map(|p| p.display().to_string())
            })
            .flatten();
        self.connection
            .execute(
                "UPDATE team_agents SET worktree=?2 WHERE id=?1",
                params![id, worktree],
            )
            .map_err(error)?;
        let (orders, inbox) = channel();
        let (outbox, reports) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let turn_time = self.limits.turn_time;
        let thread = std::thread::Builder::new()
            .name(format!("team-agent-{id}"))
            .spawn(move || work(request, inbox, outbox, flag, turn_time))
            .map_err(error)?;
        self.workers.insert(
            id,
            Worker {
                project: stored.project_id.clone(),
                role,
                harness: harness.to_owned(),
                connection: account.connection.clone(),
                orders,
                reports,
                cancel,
                busy: None,
                idle_since: Instant::now(),
                stopping: false,
                thread: Some(thread),
            },
        );
        Ok(Ok(id))
    }

    fn prepare(
        &self,
        stored: &StoredSetup,
        id: i64,
        role: AgentRole,
        harness: &str,
        tools: bool,
        account: &TeamAccount,
    ) -> Result<(PathBuf, LaunchRequest), String> {
        let plain = |e: &dyn std::fmt::Display| e.to_string();
        let launch = launch_harness(harness).ok_or("harness has no launch adapter")?;
        let executable = self
            .executables
            .get(launch)
            .ok_or("harness is not installed")?;
        let root = Path::new(&stored.folder)
            .canonicalize()
            .map_err(|e| plain(&e))?;
        if root.starts_with(&self.state) {
            return Err("project folder overlaps engine state".into());
        }
        let agent = self.state.join("agents").join(format!("agent-{id}"));
        let home = agent.join("home");
        let tmp = agent.join("tmp");
        for directory in [&agent, &home, &tmp] {
            private_directory(directory).map_err(|e| e.0)?;
        }
        let endpoint = ResearchAccount::Endpoint {
            base_url: account.base_url.clone(),
            model: account.model.clone(),
        };
        let network = if tools {
            SandboxNetwork::Open
        } else {
            SandboxNetwork::Only(vec![model_remote(&endpoint).map_err(|e| plain(&e))?])
        };
        let mut sandbox =
            ProjectSandbox::new(&root, &home, &tmp, network).map_err(|e| plain(&e))?;
        let install = executable
            .canonicalize()
            .map_err(|e| plain(&e))?
            .parent()
            .map(Path::to_path_buf)
            .ok_or("harness install folder")?;
        sandbox = sandbox.allow_read(&install).map_err(|e| plain(&e))?;
        let profile = self.state.join("profiles").join(format!("agent-{id}.sb"));
        sandbox.install(&profile).map_err(|e| plain(&e))?;
        ensure_repository(&root, &tmp).map_err(|e| plain(&e))?;
        let working_dir = match role {
            AgentRole::Master => root.clone(),
            AgentRole::Sub => {
                let worktree = root.join("worktrees").join(format!("agent-{id}"));
                add_worktree(&sandbox, &profile, &worktree).map_err(|e| plain(&e))?;
                worktree.canonicalize().map_err(|e| plain(&e))?
            }
        };
        assert!(working_dir.starts_with(&root));
        let request = LaunchRequest {
            harness: launch.into(),
            executable: executable.to_path_buf(),
            working_dir,
            account: AccountBinding::Url {
                home,
                base_url: account.base_url.clone(),
                key: None,
            },
            model: Some(account.model.clone()),
            tools,
            sandbox_profile: Some(profile),
            limits: LaunchLimits {
                run_time: self.limits.session_time,
                stop_grace: self.limits.stop_grace,
                output_bytes: self.limits.output_bytes,
                events: SESSION_EVENTS_MAX,
            },
        };
        Ok((root, request))
    }

    fn collect(&mut self) -> Result<(), EngineError> {
        let ids: Vec<i64> = self.workers.keys().copied().collect();
        for id in ids {
            loop {
                let Some(worker) = self.workers.get(&id) else {
                    break;
                };
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
            Report::Ready { group } => {
                let busy = self.workers[&id].busy;
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
            Report::Finished { text, end } => {
                let worker = self.workers.get_mut(&id).expect("worker exists");
                let busy = worker.busy.take();
                worker.idle_since = Instant::now();
                let connection = worker.connection.clone();
                let project = worker.project.clone();
                if end == TurnEnd::RateLimited {
                    self.pause(&connection)?;
                }
                self.connection
                    .execute("UPDATE team_agents SET turns=turns+1 WHERE id=?1", [id])
                    .map_err(error)?;
                match busy {
                    Some(Busy::Task(task)) => self.finish_task(task, &text, end)?,
                    Some(Busy::Message(message)) => {
                        self.finish_message(&project, message, &text, end)?
                    }
                    None => {}
                }
                if !matches!(end, TurnEnd::Success | TurnEnd::Failed) {
                    self.workers.get_mut(&id).expect("worker exists").stopping = true;
                }
                self.mark_agent(id, AgentStatus::Idle, None)?;
                Ok(false)
            }
            Report::Ended { problem } => {
                let mut worker = self.workers.remove(&id).expect("worker exists");
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
                    Some(Busy::Task(task)) => self.finish_task(task, "", TurnEnd::Closed)?,
                    Some(Busy::Message(message)) => {
                        self.finish_message(&worker.project, message, "", TurnEnd::Closed)?
                    }
                    None => {}
                }
                let status = if problem.is_some() && !worker.stopping {
                    AgentStatus::Failed
                } else {
                    AgentStatus::Stopped
                };
                self.connection
                    .execute(
                        "UPDATE team_agents SET status=?2, task=NULL, error=?3, stopped_ms=?4 WHERE id=?1",
                        params![id, status.name(), problem, now_ms()],
                    )
                    .map_err(error)?;
                Ok(true)
            }
        }
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

    fn finish_task(&mut self, task: i64, text: &str, end: TurnEnd) -> Result<(), EngineError> {
        let attempts: i64 = self
            .connection
            .query_row(
                "SELECT attempts FROM team_tasks WHERE id=?1",
                [task],
                |row| row.get(0),
            )
            .map_err(error)?;
        let retry = attempts < self.limits.attempts_max;
        let (status, problem) = match end {
            TurnEnd::Success => (TaskStatus::Done, None),
            TurnEnd::Cancelled => (TaskStatus::Cancelled, Some("Cancelled by you")),
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
                Some("The sub-agent stopped unexpectedly"),
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
            if state == "failed" {
                self.note(project, "The master stopped before it replied.")?;
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
        if text.trim().is_empty() {
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
        let delegations = parse_delegations(text);
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
            if let Err(code) = admit(
                &tx,
                &access,
                &stored,
                delegation.harness.as_deref(),
                &delegation.prompt,
                TaskSource::Master,
                Some(reply),
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
                &format!("These tasks were not handed off:\n{}", refused.join("\n")),
                MessageState::Note,
                now,
            )?;
        }
        tx.commit().map_err(error)
    }
}

fn refusal(code: &str) -> &'static str {
    match code {
        "team_harness_not_in_plan" => "that harness is not in the team plan",
        "team_harness_cannot_run" => "that harness cannot run sub-agents yet",
        "team_queue_full" => "too many tasks are already waiting",
        "team_task_limit" => "this project reached its task limit",
        "team_has_no_sub_agents" => "the team plan has no sub-agents",
        _ => "the task text is not valid",
    }
}

fn work(
    request: LaunchRequest,
    orders: Receiver<Order>,
    reports: Sender<Report>,
    cancel: Arc<AtomicBool>,
    turn_time: Duration,
) {
    let mut session = match Session::start(request) {
        Ok(session) => session,
        Err(problem) => {
            let _ = reports.send(Report::Ended {
                problem: Some(format!("The agent could not start ({})", problem.0)),
            });
            return;
        }
    };
    let _ = reports.send(Report::Ready {
        group: session.process_group(),
    });
    let mut problem = None;
    loop {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let order = match orders.recv_timeout(ORDER_WAIT) {
            Ok(order) => order,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let Order::Turn(prompt) = order else {
            break;
        };
        let (text, end) = turn(&mut session, &prompt, &reports, &cancel, turn_time);
        let reusable = matches!(end, TurnEnd::Success | TurnEnd::Failed);
        if end == TurnEnd::Closed {
            problem = Some("The agent process closed".to_owned());
        }
        if reports.send(Report::Finished { text, end }).is_err() || !reusable {
            break;
        }
    }
    let _ = session.stop();
    let _ = reports.send(Report::Ended { problem });
}

fn turn(
    session: &mut Session,
    prompt: &str,
    reports: &Sender<Report>,
    cancel: &AtomicBool,
    turn_time: Duration,
) -> (String, TurnEnd) {
    if session.send(prompt).is_err() {
        return (String::new(), TurnEnd::Closed);
    }
    let deadline = Instant::now() + turn_time;
    let mut text = String::new();
    let mut limited = false;
    let mut progress = Instant::now();
    let end = loop {
        if cancel.load(Ordering::SeqCst) {
            break TurnEnd::Cancelled;
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
            Ok(Some(LaunchEvent::TurnFinished { success })) => {
                break if success {
                    TurnEnd::Success
                } else if limited {
                    TurnEnd::RateLimited
                } else {
                    TurnEnd::Failed
                };
            }
            Ok(Some(LaunchEvent::Exited { .. })) => break TurnEnd::Closed,
            Err(problem) => {
                break match problem.0.as_str() {
                    "launch_deadline" => TurnEnd::TimedOut,
                    "launch_output_limit" | "launch_event_limit" => TurnEnd::TooLong,
                    _ => TurnEnd::Closed,
                };
            }
            Ok(Some(_)) => {}
        }
    };
    assert!(text.len() <= super::RESULT_BYTES_MAX * 4);
    (text, end)
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
                while !flag.load(Ordering::SeqCst) {
                    let _ = runtime.tick();
                    std::thread::sleep(tick);
                }
                let _ = runtime.shutdown();
            })
            .ok();
        Self { stop, thread }
    }

    /// Stops the loop, every agent process group and waits for the loop thread.
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
/// Takes the runtime folder, both settings files and the launch executables by harness.
/// Returns the service, or a plain reason it could not start.
pub fn start_for_engine(
    runtime: &Path,
    global: &Path,
    project: &Path,
    executables: BTreeMap<&'static str, PathBuf>,
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
    Ok(TeamService::start(runtime, Duration::from_millis(250)))
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
