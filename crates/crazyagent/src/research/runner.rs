use super::accounts::PROFILE_BUSY;
use super::jobs::{JobError, ResearchJob, ResearchJobs, ResearchStatus, valid_kind};
use crate::agents::{Agents, Hold, Holder};
use crate::config::ModelConnection;
use crate::engine::bootstrap::{NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::engine::{EngineError, NativeProfileCustody};
use crate::harnesses::launch::{
    AccountBinding, LaunchError, LaunchEvent, LaunchLimits, LaunchRequest,
};
use crate::harnesses::terminal_session::TerminalSession;
use crate::launch_context::AgentContext;
use crate::protocol::{ProjectId, RequestId};
use crate::sandbox::{ProjectSandbox, SandboxError, SandboxNetwork, SandboxRemote};
use crate::terminal::{AgentId, Host};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const GIT: &str = "/usr/bin/git";
const GIT_SAFE: [&str; 10] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.pager=cat",
    "-c",
    "submodule.recurse=false",
    "-c",
    "protocol.allow=never",
];
#[cfg(unix)]
const GIT_CONFIG_LISTING_BYTES_MAX: usize = 64 * 1024;
#[cfg(unix)]
const FILTER_DRIVERS_MAX: usize = 64;
static REPOSITORIES_SETTING_UP: Mutex<std::collections::BTreeSet<PathBuf>> =
    Mutex::new(std::collections::BTreeSet::new());
static REPOSITORY_FREED: Condvar = Condvar::new();
const BRIEF_BYTES_MAX: u64 = 32 * 1024;
const DOCUMENTS_LISTED_MAX: usize = 50;
const LINKS_MAX: usize = 100;
const POLL: Duration = Duration::from_millis(100);
const HEARTBEAT: Duration = Duration::from_millis(250);
const MODEL_BYTES_MAX: usize = 256;
const URL_BYTES_MAX: usize = 2048;
const SETUP_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(unix)]
const GROUP_EXIT_WAIT: Duration = Duration::from_secs(10);
#[cfg(unix)]
const LIFELINE: &str = "IFS= read -r word; [ \"$word\" = done ] || kill -s KILL -- \"-$1\"";
const RUNS_PER_ATTEMPT_MAX: u32 = 100;
const LINKS_HEADING: &str = "## Links and references";
const LOCAL_PLACEHOLDER_KEY: &str = "crazyagent-local";
const CONTEXT_BUSY: &str = "launch_home_busy";
const PANE_PREFIX: &str = "research-";
const PANE_EVENTS_MAX: usize = 200_000;
const CLAUDE_ARGUMENTS: [&str; 9] = [
    "--print",
    "--output-format",
    "text",
    "--strict-mcp-config",
    "--setting-sources",
    "",
    "--settings",
    "{\"disableAllHooks\":true}",
    "--no-session-persistence",
];

/// Bounds every research run obeys.
#[derive(Debug, Clone)]
pub struct ResearchLimits {
    pub wall_time: Duration,
    pub output_bytes_max: u64,
    pub report_bytes_max: u64,
    pub running_max: u32,
    pub attempts_max: u32,
}

impl Default for ResearchLimits {
    /// Twenty minutes, 4 MiB of agent output, a 512 KiB report, three agents
    /// per project at once and two attempts per job.
    fn default() -> Self {
        Self {
            wall_time: Duration::from_secs(20 * 60),
            output_bytes_max: 4 * 1024 * 1024,
            report_bytes_max: 512 * 1024,
            running_max: 3,
            attempts_max: 2,
        }
    }
}

/// The coding agents research can run on, named by a job's harness id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResearchHarness {
    Codex,
    ClaudeCode,
}

impl ResearchHarness {
    /// Maps a team plan or connection harness id to a research harness.
    /// Takes the id, such as `codex`, `claude` or `claude-code`.
    /// Returns `None` for a harness research cannot run on.
    pub fn of(id: &str) -> Option<Self> {
        match id {
            "codex" => Some(Self::Codex),
            "claude" | "claude-code" => Some(Self::ClaudeCode),
            _ => None,
        }
    }

    /// The harness id the launch-context seam installs project context for.
    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
        }
    }

    /// The plain product name written into reports.
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
        }
    }
}

/// The pool account a research agent runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResearchAccount {
    /// An OpenAI-compatible URL or a local model server, with no key, for Codex.
    Endpoint { base_url: String, model: String },
    /// A managed Codex connection of the project's pool. Each launch proves
    /// it is signed in for this engine run and holds its profile under the
    /// engine's custody lease while the agent runs.
    ManagedCodex {
        connection: ModelConnection,
        model: Option<String>,
    },
    /// An Anthropic Messages URL or local model server, with no key, for
    /// Claude Code.
    AnthropicEndpoint { base_url: String, model: String },
    /// A managed Claude connection of the project's pool, held like
    /// [`ResearchAccount::ManagedCodex`].
    ManagedClaude {
        connection: ModelConnection,
        model: Option<String>,
    },
}

impl ResearchAccount {
    /// The only harness that can drive this account.
    pub fn harness(&self) -> ResearchHarness {
        match self {
            Self::Endpoint { .. } | Self::ManagedCodex { .. } => ResearchHarness::Codex,
            Self::AnthropicEndpoint { .. } | Self::ManagedClaude { .. } => {
                ResearchHarness::ClaudeCode
            }
        }
    }
}

/// The project a runner works for: its id, its folder and the account picked
/// from its pool, if it has one.
#[derive(Debug, Clone)]
pub struct ResearchProject<'a> {
    pub id: &'a str,
    pub root: &'a Path,
    pub account: Option<ResearchAccount>,
}

/// What one claimed job ended as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResearchRun {
    pub job: i64,
    pub status: ResearchStatus,
    pub report: Option<PathBuf>,
    pub process_group: Option<u32>,
}

/// A runner failure outside the agent's own outcome.
#[derive(Debug)]
pub enum RunError {
    Invalid(String),
    Jobs(JobError),
    Sandbox(SandboxError),
    Context(LaunchError),
    Account(String),
    Busy,
    Io(io::Error),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "research refused: {reason}"),
            Self::Jobs(error) => write!(f, "{error}"),
            Self::Sandbox(error) => write!(f, "{error}"),
            Self::Context(error) => write!(f, "research context: {}", error.0),
            Self::Account(code) => write!(f, "research account: {code}"),
            Self::Busy => write!(f, "research account busy"),
            Self::Io(error) => write!(f, "research I/O: {error}"),
        }
    }
}

impl std::error::Error for RunError {}

impl From<JobError> for RunError {
    fn from(value: JobError) -> Self {
        Self::Jobs(value)
    }
}

/// Refreshes a claim while its run prepares, so slow preparation is not taken back as
/// stale, and stops the run once the claim was taken back. Inputs: the job store and
/// the claimed job. Output: `RunError::Jobs(JobError::NotHeld)` when it is no longer held.
fn still_held(jobs: &ResearchJobs, job: &ResearchJob) -> Result<(), RunError> {
    jobs.heartbeat(job, 0)?;
    Ok(())
}

impl From<SandboxError> for RunError {
    fn from(value: SandboxError) -> Self {
        Self::Sandbox(value)
    }
}

impl From<io::Error> for RunError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Claims queued research jobs and runs each one as a sandboxed Codex or
/// Claude Code agent in its own git worktree under the project folder, then
/// writes the agent's report to `context/research/<kind>.md`.
pub struct ResearchRunner {
    database: PathBuf,
    state: PathBuf,
    codex: Option<PathBuf>,
    claude: Option<PathBuf>,
    limits: ResearchLimits,
    claimer: String,
    stopping: AtomicBool,
    setup: Mutex<()>,
    custody: Mutex<Result<NativeProfileCustody, EngineError>>,
    terminal: Option<Arc<dyn Host>>,
    agents: Option<Arc<Agents>>,
}

/// Where one launch of a claimed research job runs: the canonical project folder, its
/// id and launch context, the run's name, worktree and agent folder.
struct JobPlace {
    root: PathBuf,
    project: ProjectId,
    context: AgentContext,
    name: String,
    worktree: PathBuf,
    agent: PathBuf,
}

#[derive(Debug)]
enum Ending {
    Exited(bool),
    TimedOut,
    Overflowed,
    Cancelled,
    Interrupted,
}

impl ResearchRunner {
    /// `database` is the job store, `state` an engine-owned directory outside
    /// every project for sandbox profiles and agent homes, `codex` the
    /// absolute Codex executable.
    pub fn new(
        database: &Path,
        state: &Path,
        codex: &Path,
        limits: ResearchLimits,
    ) -> Result<Self, RunError> {
        if !codex.is_absolute() || !codex.is_file() {
            return Err(RunError::Invalid(
                "codex executable must be absolute".into(),
            ));
        }
        Self::build(database, state, Some(codex), limits)
    }

    /// A runner with no Codex executable, for an engine where Codex is not
    /// installed: it runs Claude Code once [`Self::with_claude`] adds it, and
    /// refuses every Codex run with a plain error. Takes the same database,
    /// state directory and limits as [`Self::new`]. Returns the runner.
    pub fn without_codex(
        database: &Path,
        state: &Path,
        limits: ResearchLimits,
    ) -> Result<Self, RunError> {
        Self::build(database, state, None, limits)
    }

    fn build(
        database: &Path,
        state: &Path,
        codex: Option<&Path>,
        limits: ResearchLimits,
    ) -> Result<Self, RunError> {
        if !state.is_absolute() || !state.is_dir() || state.canonicalize()? != state {
            return Err(RunError::Invalid(
                "state directory must be canonical".into(),
            ));
        }
        assert!(codex.is_none_or(Path::is_absolute));
        if limits.wall_time.is_zero()
            || limits.output_bytes_max == 0
            || limits.report_bytes_max == 0
            || !(1..=64).contains(&limits.running_max)
            || !(1..=10).contains(&limits.attempts_max)
        {
            return Err(RunError::Invalid("research limits out of range".into()));
        }
        for directory in ["profiles", "agents"] {
            private_directory(&state.join(directory))?;
        }
        ResearchJobs::open(database)?;
        let custody = database
            .canonicalize()
            .and_then(|canonical| fs::symlink_metadata(&canonical).map(|meta| (canonical, meta)))
            .map_err(|error| EngineError(error.to_string()))
            .and_then(|(canonical, metadata)| NativeProfileCustody::capture(&canonical, &metadata));
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Ok(Self {
            database: database.to_path_buf(),
            state: state.to_path_buf(),
            codex: codex.map(Path::to_path_buf),
            claude: None,
            limits,
            claimer: format!("runner-{}-{nanos}", std::process::id()),
            stopping: AtomicBool::new(false),
            setup: Mutex::new(()),
            custody: Mutex::new(custody),
            terminal: None,
            agents: None,
        })
    }

    /// Runs every research agent in its harness's interactive app in a pane of `host` named
    /// `research-<run>`, where the owner can watch it work. Panes an earlier engine left end
    /// here, since that engine's jobs go back to the queue. Takes the host.
    /// Returns the runner, or the host's plain code.
    pub fn with_terminal(mut self, host: Arc<dyn Host>) -> Result<Self, RunError> {
        let host_error = |problem: crate::terminal::HostError| RunError::Invalid(problem.0);
        for agent in host.alive().map_err(host_error)? {
            if !agent.as_str().starts_with(PANE_PREFIX) {
                continue;
            }
            match host.kill(&agent) {
                Ok(()) => {}
                Err(problem) if problem.0 == "terminal_agent_unknown" => {}
                Err(problem) => return Err(host_error(problem)),
            }
        }
        self.terminal = Some(host);
        Ok(self)
    }

    /// Names each running research job in the engine's agent register as a holder of the
    /// account it signs in with, so a refused sign-out can name and cancel it. Takes the
    /// register. Returns the runner.
    pub fn with_agents(mut self, agents: Arc<Agents>) -> Self {
        self.agents = Some(agents);
        self
    }

    /// Lets the runner run Claude Code jobs with the absolute `claude`
    /// executable. Returns the runner, or an error for a relative or missing
    /// path.
    pub fn with_claude(mut self, claude: &Path) -> Result<Self, RunError> {
        if !claude.is_absolute() || !claude.is_file() {
            return Err(RunError::Invalid(
                "claude executable must be absolute".into(),
            ));
        }
        self.claude = Some(claude.to_path_buf());
        Ok(self)
    }

    /// Whether this runner has the executable `harness` needs.
    pub fn can_run(&self, harness: ResearchHarness) -> bool {
        match harness {
            ResearchHarness::Codex => self.codex.is_some(),
            ResearchHarness::ClaudeCode => self.claude.is_some(),
        }
    }

    /// The job store this runner claims from.
    pub fn database(&self) -> &Path {
        &self.database
    }

    /// The engine-owned folder holding this runner's sandbox profiles and
    /// agent state.
    pub(crate) fn state(&self) -> &Path {
        &self.state
    }

    /// Stops claiming, stops every running agent's process group and returns
    /// its job to the queue so a later runner picks it up again.
    pub fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }

    /// Whether [`ResearchRunner::shutdown`] was called.
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Claims and runs one queued job of `project` to its end on this thread.
    pub fn run_once(&self, project: &ResearchProject<'_>) -> Result<Option<ResearchRun>, RunError> {
        if self.is_stopping() {
            return Ok(None);
        }
        let jobs = ResearchJobs::open(&self.database)?;
        let Some(job) = jobs.claim(project.id, &self.claimer, self.limits.running_max)? else {
            return Ok(None);
        };
        self.run_claimed(&jobs, job, project).map(Some)
    }

    /// Claims every job of `project` for the account's harness that the
    /// per-project limit allows right now, at most `running_max` in one call
    /// even while runs hand busy jobs back, and runs each on its own thread.
    pub fn spawn_ready(
        self: &Arc<Self>,
        project: String,
        harness: &str,
        root: PathBuf,
        account: Option<ResearchAccount>,
    ) -> Vec<JoinHandle<Result<ResearchRun, RunError>>> {
        let mut handles = Vec::new();
        let Ok(jobs) = ResearchJobs::open(&self.database) else {
            return handles;
        };
        for _ in 0..self.limits.running_max {
            if self.is_stopping() {
                break;
            }
            let Ok(Some(job)) =
                jobs.claim_harness(&project, harness, &self.claimer, self.limits.running_max)
            else {
                break;
            };
            let runner = self.clone();
            let project = project.clone();
            let root = root.clone();
            let account = account.clone();
            handles.push(std::thread::spawn(move || {
                let jobs = ResearchJobs::open(&runner.database)?;
                runner.run_claimed(
                    &jobs,
                    job,
                    &ResearchProject {
                        id: &project,
                        root: &root,
                        account,
                    },
                )
            }));
        }
        assert!(handles.len() <= self.limits.running_max as usize);
        handles
    }

    fn run_claimed(
        &self,
        jobs: &ResearchJobs,
        job: ResearchJob,
        project: &ResearchProject<'_>,
    ) -> Result<ResearchRun, RunError> {
        assert_eq!(job.status, ResearchStatus::Running);
        assert_eq!(job.project, project.id);
        let run = |status, report, process_group| ResearchRun {
            job: job.id,
            status,
            report,
            process_group,
        };
        let Some(harness) = ResearchHarness::of(&job.harness) else {
            let status = jobs.fail(&job, "This harness cannot run research yet", 1)?;
            return Ok(run(status, None, None));
        };
        let Some(account) = project
            .account
            .as_ref()
            .filter(|account| account.harness() == harness)
        else {
            let status = jobs.needs_pool(&job)?;
            return Ok(run(status, None, None));
        };
        if harness == ResearchHarness::ClaudeCode && self.claude.is_none() {
            let status = jobs.fail(&job, "This harness is not installed on this computer", 1)?;
            return Ok(run(status, None, None));
        }
        match self.launch(jobs, &job, project, account) {
            Ok((Ending::Exited(true), group, output)) => {
                let report = match self.publish(&job, harness, project.root, &output) {
                    Ok(report) => report,
                    Err(reason) => {
                        let status = jobs.fail(&job, reason, self.limits.attempts_max)?;
                        return Ok(run(status, None, Some(group)));
                    }
                };
                let relative = format!("context/research/{}.md", job.kind);
                jobs.finish(&job, &relative)?;
                Ok(run(ResearchStatus::Done, Some(report), Some(group)))
            }
            Ok((ending, group, _)) => {
                if let Ending::Cancelled = ending {
                    jobs.cancelled(&job)?;
                    return Ok(run(ResearchStatus::Cancelled, None, Some(group)));
                }
                if let Ending::Interrupted = ending {
                    let _ = jobs.release(&job);
                    let status = jobs
                        .get(job.id)?
                        .map_or(ResearchStatus::Queued, |job| job.status);
                    return Ok(run(status, None, Some(group)));
                }
                let reason = match ending {
                    Ending::TimedOut => "Agent ran out of time",
                    Ending::Overflowed => "Agent output exceeded the limit",
                    _ => "Agent stopped with an error",
                };
                let status = jobs.fail(&job, reason, self.limits.attempts_max)?;
                Ok(run(status, None, Some(group)))
            }
            Err(RunError::Jobs(JobError::NotHeld)) => {
                let status = jobs
                    .get(job.id)?
                    .map_or(ResearchStatus::Queued, |job| job.status);
                Ok(run(status, None, None))
            }
            Err(RunError::Busy) => {
                jobs.release(&job)?;
                let status = jobs
                    .get(job.id)?
                    .map_or(ResearchStatus::Queued, |job| job.status);
                Ok(run(status, None, None))
            }
            Err(RunError::Account(_)) => {
                let status = jobs.needs_pool(&job)?;
                Ok(run(status, None, None))
            }
            Err(error) => {
                let reason = match error {
                    RunError::Context(_) => "Could not give the agent this project's context",
                    _ => "Could not prepare the research sandbox",
                };
                jobs.fail(&job, reason, self.limits.attempts_max)?;
                Err(error)
            }
        }
    }

    fn launch(
        &self,
        jobs: &ResearchJobs,
        job: &ResearchJob,
        project: &ResearchProject<'_>,
        account: &ResearchAccount,
    ) -> Result<(Ending, u32, PathBuf), RunError> {
        let place = self.job_place(jobs, job, project)?;
        let lease = match account {
            ResearchAccount::ManagedCodex { connection, .. }
            | ResearchAccount::ManagedClaude { connection, .. } => {
                let (host, fact) = jobs.managed_sign_in(connection).map_err(|code| {
                    if code == PROFILE_BUSY {
                        RunError::Busy
                    } else {
                        RunError::Account(code)
                    }
                })?;
                Some(self.held_lease(&host, &fact.binding, &place.name, &place.project, job)?)
            }
            ResearchAccount::Endpoint { .. } | ResearchAccount::AnthropicEndpoint { .. } => None,
        };
        self.launch_leased(jobs, job, account, place, lease)
    }

    /// Checks that a claimed job can launch and names its run: the canonical project
    /// folder, the project's launch context and a free run name and worktree.
    /// Inputs: the job store, the claimed job and its project. Output: where it runs.
    fn job_place(
        &self,
        jobs: &ResearchJobs,
        job: &ResearchJob,
        project: &ResearchProject<'_>,
    ) -> Result<JobPlace, RunError> {
        if !valid_kind(&job.kind) {
            return Err(RunError::Invalid("unsafe research kind".into()));
        }
        still_held(jobs, job)?;
        let root = project.root.canonicalize()?;
        if root != project.root || !root.is_dir() || root.starts_with(&self.state) {
            return Err(RunError::Invalid("project folder must be canonical".into()));
        }
        let id = ProjectId::new(project.id).map_err(|_| RunError::Invalid("project id".into()))?;
        let context = jobs.project_context(&id).map_err(RunError::Context)?;
        let (name, worktree) = self.free_run(&root, job)?;
        let agent = self.state.join("agents").join(&name);
        Ok(JobPlace {
            root,
            project: id,
            context,
            name,
            worktree,
            agent,
        })
    }

    /// Runs a claimed job's agent once its account is taken: the managed credential copy
    /// in the agent's home, the project context, the sandboxed worktree and the harness.
    /// Inputs: the job store, the job, its account, where it runs and the held lease, none
    /// for an endpoint account. Output: how the agent ended, its process group and output.
    fn launch_leased(
        &self,
        jobs: &ResearchJobs,
        job: &ResearchJob,
        account: &ResearchAccount,
        place: JobPlace,
        lease: Option<(NativeProfileUse, Option<Hold>)>,
    ) -> Result<(Ending, u32, PathBuf), RunError> {
        let JobPlace {
            root,
            context,
            name,
            worktree,
            agent,
            ..
        } = place;
        private_directory(&agent)?;
        let home = agent.join("home");
        private_directory(&home)?;
        let harness = account.harness();
        let executable = match harness {
            ResearchHarness::Codex => self
                .codex
                .as_deref()
                .ok_or_else(|| RunError::Invalid("codex executable missing".into()))?,
            ResearchHarness::ClaudeCode => self
                .claude
                .as_deref()
                .ok_or_else(|| RunError::Invalid("claude executable missing".into()))?,
        };
        let credential = match &lease {
            Some((lease, _)) => Some(
                lease
                    .hand_to_agent(&home, self.limits.wall_time, executable)
                    .map_err(|problem| RunError::Account(problem.0))?,
            ),
            None => None,
        };
        let installed = context
            .install(harness.id(), &home, job.tools)
            .map_err(|problem| {
                if problem.0 == CONTEXT_BUSY {
                    RunError::Busy
                } else {
                    RunError::Context(problem)
                }
            })?;
        private_directory(&agent)?;
        let tmp = agent.join("tmp");
        private_directory(&tmp)?;
        let network = if job.tools {
            SandboxNetwork::Open
        } else {
            SandboxNetwork::Only(vec![model_remote(account)?])
        };
        let mut sandbox = ProjectSandbox::new(&root, &home, &tmp, network)?;
        if let Some(runtime) = self.state.parent() {
            sandbox = sandbox.guard_engine(runtime)?;
        }
        for folder in read_folders(executable, &context, job.tools)? {
            sandbox = sandbox.allow_read(&folder)?;
        }
        let profile = self.state.join("profiles").join(format!("{name}.sb"));
        sandbox.install(&profile)?;
        let setup = self.setup.lock().unwrap_or_else(PoisonError::into_inner);
        ensure_repository(&root, &tmp)?;
        add_worktree(&sandbox, &profile, &worktree)?;
        let sandbox = sandbox.confine_to_worktree(&worktree)?;
        sandbox.install(&profile)?;
        drop(setup);
        still_held(jobs, job)?;
        let prompt = research_prompt(&root, &job.kind)?;
        write_private(&agent.join("prompt.md"), prompt.as_bytes())?;
        if let Some(host) = &self.terminal {
            let output = match harness {
                ResearchHarness::Codex => tmp.join("report.md"),
                ResearchHarness::ClaudeCode => agent.join("output.md"),
            };
            let (binding, model) = pane_account(account, &home);
            let request = LaunchRequest {
                harness: harness.id().to_owned(),
                executable: executable.to_path_buf(),
                working_dir: worktree.clone(),
                account: binding,
                model,
                tools: job.tools,
                sandbox_profile: Some(profile.clone()),
                limits: LaunchLimits {
                    run_time: self.limits.wall_time,
                    output_bytes: usize::try_from(self.limits.output_bytes_max)
                        .unwrap_or(usize::MAX)
                        .min(64 * 1024 * 1024),
                    events: PANE_EVENTS_MAX,
                    ..LaunchLimits::default()
                },
            };
            let pane = AgentId::new(&format!("{PANE_PREFIX}{name}"))
                .map_err(|problem| RunError::Invalid(problem.0))?;
            let mut session =
                TerminalSession::start_installed(host.clone(), pane, request, installed, &context)
                    .map_err(|problem| {
                        RunError::Invalid(format!("agent terminal: {}", problem.0))
                    })?;
            let group = session.process_group();
            #[cfg(unix)]
            let mut lifeline = Lifeline::watch(group)?;
            let ending = self.supervise_pane(jobs, job, &mut session, &prompt, &output);
            let _ = session.stop();
            #[cfg(unix)]
            lifeline.release();
            drop(credential);
            drop(lease);
            return Ok((ending?, group, output));
        }
        let mut launch = Command::new(executable);
        let (output, stdout_log) = match harness {
            ResearchHarness::Codex => {
                let output = tmp.join("report.md");
                codex_arguments(&mut launch, &worktree, &output, &home, job.tools, account)?;
                (output, "events.jsonl")
            }
            ResearchHarness::ClaudeCode => {
                claude_arguments(&mut launch, &home, &tmp, job.tools, account)?;
                launch.args(installed.arguments());
                (agent.join("output.md"), "output.md")
            }
        };
        launch.current_dir(&worktree);
        let mut command = sandbox.wrap(&profile, &launch)?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        assert!(!carries_secret(&command, &context));
        let mut running = AgentProcess::new(crate::spawn_gate::spawn(&mut command)?)?;
        let group = running.group;
        let counted = Arc::new(AtomicU64::new(0));
        for (stream, name) in [
            (
                running
                    .child
                    .stdout
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Read + Send>),
                stdout_log,
            ),
            (
                running
                    .child
                    .stderr
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Read + Send>),
                "stderr.log",
            ),
        ] {
            let Some(stream) = stream else { continue };
            let log = create_private(&agent.join(name))?;
            let counted = counted.clone();
            let log_max = self.limits.output_bytes_max;
            std::thread::spawn(move || drain(stream, &counted, log, log_max));
        }
        if let Some(mut stdin) = running.child.stdin.take() {
            let _ = stdin.write_all(prompt.as_bytes());
        }
        let ending = self.supervise(jobs, job, &mut running, &counted)?;
        drop(running);
        drop(installed);
        drop(credential);
        drop(lease);
        Ok((ending, group, output))
    }

    /// Takes a managed profile for one research launch under the engine's
    /// custody lease, so no other research, team or master launch uses it
    /// until the lease is dropped. Takes the active engine host, the sign-in
    /// binding, the launch name and the job attempt as the lease generation.
    /// Returns the lease, [`RunError::Busy`] while another launch holds the
    /// profile, or the custody refusal code.
    fn lease(
        &self,
        host: &HostStamp,
        binding: &NativeProfileBinding,
        name: &str,
        generation: u64,
    ) -> Result<NativeProfileUse, RunError> {
        let operation = RequestId::new(format!("research-{name}"))
            .map_err(|_| RunError::Invalid("research launch name".into()))?;
        let mut custody = self.custody.lock().unwrap_or_else(PoisonError::into_inner);
        let custody = custody
            .as_mut()
            .map_err(|problem| RunError::Account(problem.0.clone()))?;
        custody
            .acquire_use(
                host,
                binding,
                &operation,
                generation,
                NativeProfileUsePurpose::Execution,
            )
            .map_err(|problem| {
                if problem.0 == PROFILE_BUSY {
                    RunError::Busy
                } else {
                    RunError::Account(problem.0)
                }
            })
    }

    /// Takes a research job's profile lease and names the job in the agent register as a
    /// holder of the lease's account until both are dropped together. Takes the engine
    /// host, the sign-in binding, the launch name, the project and the claimed job.
    /// Returns the lease and its hold, none without a register, or the lease's refusal.
    fn held_lease(
        &self,
        host: &HostStamp,
        binding: &NativeProfileBinding,
        name: &str,
        project: &ProjectId,
        job: &ResearchJob,
    ) -> Result<(NativeProfileUse, Option<Hold>), RunError> {
        let lease = self.lease(host, binding, name, u64::from(job.attempts))?;
        let hold = self.agents.as_ref().map(|agents| {
            agents.hold(
                binding.connection.id.clone(),
                Holder::Research {
                    project: project.clone(),
                    job: job.id,
                    title: kind_label(&job.kind),
                },
            )
        });
        Ok((lease, hold))
    }

    fn supervise(
        &self,
        jobs: &ResearchJobs,
        job: &ResearchJob,
        agent: &mut AgentProcess,
        counted: &AtomicU64,
    ) -> Result<Ending, RunError> {
        let started = Instant::now();
        let mut beat = Instant::now();
        let ending = loop {
            if let Some(status) = agent.child.try_wait()? {
                break Ending::Exited(status.success());
            }
            if started.elapsed() >= self.limits.wall_time {
                break Ending::TimedOut;
            }
            if self.is_stopping() {
                break Ending::Interrupted;
            }
            if counted.load(Ordering::Relaxed) > self.limits.output_bytes_max {
                break Ending::Overflowed;
            }
            if beat.elapsed() >= HEARTBEAT {
                beat = Instant::now();
                match jobs.heartbeat(job, counted.load(Ordering::Relaxed)) {
                    Ok(true) => break Ending::Cancelled,
                    Err(JobError::NotHeld) => break Ending::Interrupted,
                    Ok(false) | Err(_) => {}
                }
            }
            std::thread::sleep(POLL);
        };
        agent.stop();
        let _ = jobs.heartbeat(job, counted.load(Ordering::Relaxed));
        Ok(ending)
    }

    /// Types the research prompt into the agent's pane and follows its session record until
    /// the turn ends, the job is cancelled, the runner stops or a limit passes. The agent's
    /// last reply becomes the report at `output`, as `codex exec -o` and `claude -p` give.
    /// Returns how the run ended.
    fn supervise_pane(
        &self,
        jobs: &ResearchJobs,
        job: &ResearchJob,
        session: &mut TerminalSession,
        prompt: &str,
        output: &Path,
    ) -> Result<Ending, RunError> {
        let mut last = String::new();
        let mut counted: u64 = 0;
        if let Err(problem) = session.send_or_cancel(prompt, &self.stopping) {
            if problem.0 == "launch_cancelled" {
                return Ok(Ending::Interrupted);
            }
            return Ok(Ending::Exited(false));
        }
        let mut beat = Instant::now();
        let ending = loop {
            if self.is_stopping() {
                break Ending::Interrupted;
            }
            if counted > self.limits.output_bytes_max {
                break Ending::Overflowed;
            }
            if beat.elapsed() >= HEARTBEAT {
                beat = Instant::now();
                match jobs.heartbeat(job, counted) {
                    Ok(true) => break Ending::Cancelled,
                    Err(JobError::NotHeld) => break Ending::Interrupted,
                    Ok(false) | Err(_) => {}
                }
            }
            match session.next_event(POLL) {
                Ok(Some(LaunchEvent::Text { text })) => {
                    counted += text.len() as u64;
                    last = text;
                }
                Ok(Some(LaunchEvent::TurnFinished { success })) => break Ending::Exited(success),
                Ok(Some(LaunchEvent::Exited { .. })) => break Ending::Exited(false),
                Ok(_) => {}
                Err(problem) => {
                    break match problem.0.as_str() {
                        "launch_deadline" => Ending::TimedOut,
                        "launch_output_limit" | "launch_event_limit" => Ending::Overflowed,
                        _ => Ending::Exited(false),
                    };
                }
            }
        };
        let _ = jobs.heartbeat(job, counted);
        if matches!(ending, Ending::Exited(true)) {
            assert!(last.len() as u64 <= counted);
            write_private(output, last.as_bytes())?;
        }
        Ok(ending)
    }

    /// Names this launch of a job so it never reuses the agent state or the
    /// worktree of an earlier launch, such as one an engine stop interrupted
    /// without spending its attempt. Takes the project folder and the job.
    /// Returns the agent state name and the worktree path, or an error after
    /// `RUNS_PER_ATTEMPT_MAX` launches of the same attempt.
    fn free_run(&self, root: &Path, job: &ResearchJob) -> Result<(String, PathBuf), RunError> {
        for launch in 0..RUNS_PER_ATTEMPT_MAX {
            let suffix = if launch == 0 {
                String::new()
            } else {
                format!("-{launch}")
            };
            let name = format!("job-{}-{}{suffix}", job.id, job.attempts);
            let worktree = crate::projects_home::state_folder(root)
                .join("worktrees")
                .join(format!(
                    "research-{}-{}-{}{suffix}",
                    job.kind, job.id, job.attempts
                ));
            let taken = |path: &Path| fs::symlink_metadata(path).is_ok();
            if !taken(&self.state.join("agents").join(&name)) && !taken(&worktree) {
                return Ok((name, worktree));
            }
        }
        Err(RunError::Invalid(
            "too many launches of one research attempt".into(),
        ))
    }

    fn publish(
        &self,
        job: &ResearchJob,
        harness: ResearchHarness,
        root: &Path,
        output: &Path,
    ) -> Result<PathBuf, &'static str> {
        let body = read_bounded(output, self.limits.report_bytes_max)
            .map_err(|_| "Agent returned no report")?;
        let body = body.trim();
        if body.is_empty() {
            return Err("Agent returned no report");
        }
        let report = render_report(&kind_label(&job.kind), harness.label(), body);
        write_report(root, &job.kind, &report).map_err(|_| "Could not save the research report")
    }
}

/// One agent run outside the research queue that may only read `folder`,
/// such as onboarding's mapping agent: `name` names its agent state and
/// sandbox profile, `prompt` goes to it on stdin, `wall_time` bounds it.
pub(crate) struct ReadOnlyRun<'a> {
    pub name: &'a str,
    #[cfg(unix)]
    pub folder: &'a Path,
    #[cfg(unix)]
    pub prompt: &'a str,
    pub account: &'a ResearchAccount,
    pub wall_time: Duration,
}

/// How a read-only agent run ended: its last message, a plain failure, out
/// of time, or stopped by its caller or the runner's shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadOnlyEnding {
    #[cfg(unix)]
    Answered(String),
    #[cfg(unix)]
    Failed(String),
    #[cfg(unix)]
    TimedOut,
    #[cfg(unix)]
    Stopped,
}

impl ResearchRunner {
    /// Runs one agent with tools off that can read `run.folder` but write only
    /// its own engine-owned work, home and temp folders, reaching nothing but
    /// its model endpoint. A managed account is held under the engine custody
    /// lease for the whole run. Takes the run and `stop`, polled while the
    /// agent runs. Returns how it ended; the agent's process group is stopped
    /// on every path, including a panic or an early error.
    #[cfg(unix)]
    pub(crate) fn run_read_only(
        &self,
        run: &ReadOnlyRun<'_>,
        stop: &dyn Fn() -> bool,
    ) -> Result<ReadOnlyEnding, RunError> {
        if !valid_run_name(run.name) || run.wall_time.is_zero() {
            return Err(RunError::Invalid("read-only run name or limit".into()));
        }
        let folder = run.folder.canonicalize()?;
        if folder != run.folder || !folder.is_dir() || folder.starts_with(&self.state) {
            return Err(RunError::Invalid(
                "read-only folder must be canonical".into(),
            ));
        }
        let agent = self.state.join("agents").join(run.name);
        if fs::symlink_metadata(&agent).is_ok() {
            return Err(RunError::Invalid("read-only run name already used".into()));
        }
        let lease = match run.account {
            ResearchAccount::ManagedCodex { connection, .. }
            | ResearchAccount::ManagedClaude { connection, .. } => {
                let jobs = ResearchJobs::open(&self.database)?;
                let (host, fact) = jobs.managed_sign_in(connection).map_err(|code| {
                    if code == PROFILE_BUSY {
                        RunError::Busy
                    } else {
                        RunError::Account(code)
                    }
                })?;
                Some(self.lease(&host, &fact.binding, run.name, 1)?)
            }
            ResearchAccount::Endpoint { .. } | ResearchAccount::AnthropicEndpoint { .. } => None,
        };
        private_directory(&agent)?;
        let home = agent.join("home");
        private_directory(&home)?;
        let harness = run.account.harness();
        let executable = match harness {
            ResearchHarness::Codex => self
                .codex
                .as_deref()
                .ok_or_else(|| RunError::Invalid("codex executable missing".into()))?,
            ResearchHarness::ClaudeCode => self
                .claude
                .as_deref()
                .ok_or_else(|| RunError::Invalid("claude executable missing".into()))?,
        };
        let credential = match &lease {
            Some(lease) => Some(
                lease
                    .hand_to_agent(&home, run.wall_time, executable)
                    .map_err(|problem| RunError::Account(problem.0))?,
            ),
            None => None,
        };
        let work = agent.join("work");
        private_directory(&work)?;
        let tmp = agent.join("tmp");
        private_directory(&tmp)?;
        let mut sandbox = ProjectSandbox::new(
            &work,
            &home,
            &tmp,
            SandboxNetwork::Only(vec![model_remote(run.account)?]),
        )?
        .allow_read(&folder)?;
        if let Some(parent) = executable.canonicalize()?.parent() {
            sandbox = sandbox.allow_read(parent)?;
        }
        let profile = self.state.join("profiles").join(format!("{}.sb", run.name));
        sandbox.install(&profile)?;
        write_private(&agent.join("prompt.md"), run.prompt.as_bytes())?;
        let mut command = sandbox.command(&profile, executable);
        let (output, stdout_log) = match harness {
            ResearchHarness::Codex => {
                let output = tmp.join("answer.md");
                codex_arguments(&mut command, &folder, &output, &home, false, run.account)?;
                (output, "events.jsonl")
            }
            ResearchHarness::ClaudeCode => {
                claude_arguments(&mut command, &home, &tmp, false, run.account)?;
                (agent.join("output.md"), "output.md")
            }
        };
        command
            .current_dir(&folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut running = AgentProcess::new(crate::spawn_gate::spawn(&mut command)?)?;
        let counted = Arc::new(AtomicU64::new(0));
        let mut drains = Vec::with_capacity(2);
        let streams = [
            (
                running
                    .child
                    .stdout
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Read + Send>),
                stdout_log,
            ),
            (
                running
                    .child
                    .stderr
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Read + Send>),
                "stderr.log",
            ),
        ];
        for (stream, name) in streams {
            let Some(stream) = stream else { continue };
            let log = create_private(&agent.join(name))?;
            let counted = counted.clone();
            let log_max = self.limits.output_bytes_max;
            drains.push(std::thread::spawn(move || {
                drain(stream, &counted, log, log_max)
            }));
        }
        if let Some(mut stdin) = running.child.stdin.take() {
            let _ = stdin.write_all(run.prompt.as_bytes());
        }
        let started = Instant::now();
        let mut exited_failing = false;
        let ending = loop {
            if let Some(status) = running.child.try_wait()? {
                exited_failing = !status.success();
                break None;
            }
            if started.elapsed() >= run.wall_time {
                break Some(ReadOnlyEnding::TimedOut);
            }
            if stop() || self.is_stopping() {
                break Some(ReadOnlyEnding::Stopped);
            }
            if counted.load(Ordering::Relaxed) > self.limits.output_bytes_max {
                break Some(ReadOnlyEnding::Failed("Agent wrote too much output".into()));
            }
            std::thread::sleep(POLL);
        };
        running.stop();
        drop(running);
        for handle in drains {
            let _ = handle.join();
        }
        drop(credential);
        drop(lease);
        if exited_failing {
            return Ok(ReadOnlyEnding::Failed(agent_failure(
                &agent.join(stdout_log),
                harness,
                self.limits.output_bytes_max,
            )));
        }
        if let Some(ending) = ending {
            return Ok(ending);
        }
        let answer = read_bounded(&output, self.limits.report_bytes_max)
            .map(|text| text.trim().to_owned())
            .unwrap_or_default();
        Ok(if answer.is_empty() {
            ReadOnlyEnding::Failed("Agent returned no answer".into())
        } else {
            ReadOnlyEnding::Answered(answer)
        })
    }

    /// Asks the Windows boundary for a read-only run's network reach before any agent
    /// state is made. A read-only run always limits the agent to its model endpoint, which
    /// the AppContainer boundary refuses until its firewall rule exists rather than widen it.
    /// Takes the run and `stop`, unused. Returns `Invalid` for a bad run, or the boundary's
    /// `Unavailable` refusal.
    #[cfg(windows)]
    pub(crate) fn run_read_only(
        &self,
        run: &ReadOnlyRun<'_>,
        _stop: &dyn Fn() -> bool,
    ) -> Result<ReadOnlyEnding, RunError> {
        if !valid_run_name(run.name) || run.wall_time.is_zero() {
            return Err(RunError::Invalid("read-only run name or limit".into()));
        }
        let network = SandboxNetwork::Only(vec![model_remote(run.account)?]);
        let refusal = crate::sandbox::windows::capabilities(&network)
            .expect_err("an endpoint limit needs the AppContainer firewall rule");
        Err(RunError::Sandbox(refusal))
    }
}

/// Whether `name` can name a read-only run's agent state, sandbox profile and
/// lease operation: 1 to 96 ASCII letters, digits, `-` or `_`.
fn valid_run_name(name: &str) -> bool {
    (1..=96).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Lists the folders an agent's sandbox must let it read beyond its home:
/// the harness install folder and, with tools on, the folder of every
/// approved MCP command server. Takes the harness executable, the project
/// context and the tools switch. Returns the canonical folders, sorted and
/// without repeats, or an error for a server executable that does not exist.
fn read_folders(
    executable: &Path,
    context: &AgentContext,
    tools: bool,
) -> Result<Vec<PathBuf>, RunError> {
    let mut executables = vec![executable.to_path_buf()];
    if tools {
        executables.extend(context.read_paths());
    }
    let mut folders = Vec::with_capacity(executables.len());
    for executable in executables {
        if let Some(folder) = executable.canonicalize()?.parent() {
            folders.push(folder.to_path_buf());
        }
    }
    folders.sort();
    folders.dedup();
    Ok(folders)
}

/// Reports whether any MCP secret of the project context appears in the
/// command's program, arguments or environment.
fn carries_secret(command: &Command, context: &AgentContext) -> bool {
    context.exposes_secret(command.get_program().as_encoded_bytes())
        || command
            .get_args()
            .any(|argument| context.exposes_secret(argument.as_encoded_bytes()))
        || command.get_envs().any(|(name, value)| {
            context.exposes_secret(name.as_encoded_bytes())
                || value.is_some_and(|value| context.exposes_secret(value.as_encoded_bytes()))
        })
}

pub(crate) fn model_remote(account: &ResearchAccount) -> Result<SandboxRemote, RunError> {
    match account {
        ResearchAccount::ManagedCodex { .. } | ResearchAccount::ManagedClaude { .. } => {
            Ok(SandboxRemote::AnyHost(443))
        }
        ResearchAccount::Endpoint { base_url, .. }
        | ResearchAccount::AnthropicEndpoint { base_url, .. } => {
            let url = endpoint_url(base_url)?;
            let port = url
                .port_or_known_default()
                .ok_or_else(|| RunError::Invalid("endpoint port".into()))?;
            let loopback = match url.host() {
                Some(url::Host::Ipv4(address)) => address.is_loopback(),
                Some(url::Host::Ipv6(address)) => address.is_loopback(),
                Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
                None => false,
            };
            Ok(if loopback {
                SandboxRemote::Loopback(port)
            } else {
                SandboxRemote::AnyHost(port)
            })
        }
    }
}

fn endpoint_url(base_url: &str) -> Result<url::Url, RunError> {
    let invalid =
        || RunError::Invalid("endpoint URL must be plain http(s) without credentials".into());
    if base_url.len() > URL_BYTES_MAX || base_url.contains(['"', '\\', '\'']) {
        return Err(invalid());
    }
    let url = url::Url::parse(base_url).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host().is_none()
    {
        return Err(invalid());
    }
    Ok(url)
}

fn valid_model(model: &str) -> bool {
    model.len() <= MODEL_BYTES_MAX
        && model
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/@+".contains(&byte))
}

fn codex_arguments(
    command: &mut Command,
    worktree: &Path,
    output: &Path,
    home: &Path,
    tools: bool,
    account: &ResearchAccount,
) -> Result<(), RunError> {
    command
        .arg("exec")
        .args(["--skip-git-repo-check", "--color", "never", "--json"])
        .arg("-C")
        .arg(worktree)
        .arg("-o")
        .arg(output)
        .arg("-s")
        .arg(if tools {
            "danger-full-access"
        } else {
            "read-only"
        })
        .env("CODEX_HOME", home);
    match account {
        ResearchAccount::Endpoint { base_url, model } => {
            let url = endpoint_url(base_url)?;
            if !valid_model(model) {
                return Err(RunError::Invalid("endpoint model name".into()));
            }
            command
                .args(["-c", "model_provider=\"crazyagent_endpoint\""])
                .args([
                    "-c",
                    "model_providers.crazyagent_endpoint.name=\"Research endpoint\"",
                ])
                .arg("-c")
                .arg(format!(
                    "model_providers.crazyagent_endpoint.base_url=\"{}\"",
                    url.as_str().trim_end_matches('/')
                ))
                .arg("-m")
                .arg(model);
        }
        ResearchAccount::ManagedCodex { model, .. } => {
            command.args([
                "-c",
                "cli_auth_credentials_store=\"file\"",
                "-c",
                "features.apps=false",
                "-c",
                "features.plugins=false",
            ]);
            if let Some(model) = model {
                if !valid_model(model) {
                    return Err(RunError::Invalid("managed model name".into()));
                }
                command.arg("-m").arg(model);
            }
        }
        ResearchAccount::AnthropicEndpoint { .. } | ResearchAccount::ManagedClaude { .. } => {
            return Err(RunError::Invalid("account does not fit Codex".into()));
        }
    }
    command.arg("-");
    Ok(())
}

/// Adds Claude Code's print-mode arguments and environment for one research
/// run: no MCP servers, settings sources, hooks or saved sessions, tools on or
/// off, and the account as either a managed config directory or a keyless
/// Anthropic endpoint in bare mode that never reads the keychain.
/// Takes the command, the agent home and temp dir, the tools switch and the
/// account. Returns an error for an account that does not fit Claude Code.
fn claude_arguments(
    command: &mut Command,
    home: &Path,
    tmp: &Path,
    tools: bool,
    account: &ResearchAccount,
) -> Result<(), RunError> {
    command
        .args(CLAUDE_ARGUMENTS)
        .env("CLAUDE_CONFIG_DIR", home)
        .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", home)
        .env("CLAUDE_CODE_TMPDIR", tmp)
        .env("TERM", "dumb")
        .env("DISABLE_TELEMETRY", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
    if tools {
        command
            .args([
                "--tools",
                "default",
                "--permission-mode",
                "bypassPermissions",
            ])
            .env("SHELL", "/bin/zsh");
    } else {
        command.args(["--tools", "", "--disable-slash-commands"]);
    }
    match account {
        ResearchAccount::AnthropicEndpoint { base_url, model } => {
            let url = endpoint_url(base_url)?;
            if !valid_model(model) {
                return Err(RunError::Invalid("endpoint model name".into()));
            }
            command
                .arg("--bare")
                .arg("--model")
                .arg(model)
                .env("ANTHROPIC_API_KEY", LOCAL_PLACEHOLDER_KEY)
                .env("ANTHROPIC_BASE_URL", url.as_str().trim_end_matches('/'))
                .env("ANTHROPIC_SMALL_FAST_MODEL", model);
        }
        ResearchAccount::ManagedClaude { model, .. } => {
            if let Some(model) = model {
                if !valid_model(model) {
                    return Err(RunError::Invalid("managed model name".into()));
                }
                command.arg("--model").arg(model);
            }
        }
        ResearchAccount::Endpoint { .. } | ResearchAccount::ManagedCodex { .. } => {
            return Err(RunError::Invalid("account does not fit Claude Code".into()));
        }
    }
    Ok(())
}

/// Names the account and model a research pane launches on, in the agent's home.
/// Takes the research account and the home. Returns the binding and the model; research
/// accounts carry no key.
fn pane_account(account: &ResearchAccount, home: &Path) -> (AccountBinding, Option<String>) {
    let home = home.to_path_buf();
    match account {
        ResearchAccount::Endpoint { base_url, model }
        | ResearchAccount::AnthropicEndpoint { base_url, model } => (
            AccountBinding::Url {
                home,
                base_url: base_url.clone(),
                key: None,
            },
            Some(model.clone()),
        ),
        ResearchAccount::ManagedCodex { model, .. }
        | ResearchAccount::ManagedClaude { model, .. } => {
            (AccountBinding::Subscription { home }, model.clone())
        }
    }
}

/// The error every owner-only research file or folder reports where none can be made.
/// Takes nothing. Returns an `Unsupported` I/O error reading `research_runner_unavailable`.
#[cfg(not(unix))]
fn unavailable() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "research_runner_unavailable")
}

/// Creates a fresh 0600 file without following a link at `path`.
/// Takes the path. Returns the new file, or the I/O error, also when it exists.
#[cfg(unix)]
fn create_private(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

/// Refuses an owner-only research file where none can be made.
/// Takes the path, unused. Returns `research_runner_unavailable`.
#[cfg(not(unix))]
fn create_private(_path: &Path) -> io::Result<File> {
    Err(unavailable())
}

/// Opens a file to append, creating it, without following a link at `path`.
/// Takes the path. Returns the open file or the I/O error.
#[cfg(unix)]
fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

/// Opens a file to append, creating it, as the reparse point itself and never a link's
/// target. Takes the path. Returns the open file or the I/O error.
#[cfg(windows)]
fn open_append(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .append(true)
        .create(true)
        .custom_flags(crate::platform::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Opens a file to read without following a link at `path`.
/// Takes the path. Returns the open file or the I/O error.
#[cfg(unix)]
fn open_read(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

/// Opens a file to read as the reparse point itself, never a link's target.
/// Takes the path. Returns the open file or the I/O error.
#[cfg(windows)]
fn open_read(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(crate::platform::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = create_private(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// The setup lock of one project repository; dropping it lets the next setup of that
/// repository run.
struct RepositorySetup(PathBuf);

impl Drop for RepositorySetup {
    fn drop(&mut self) {
        REPOSITORIES_SETTING_UP
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.0);
        REPOSITORY_FREED.notify_all();
    }
}

/// Waits for and takes the setup lock of one project repository, so git init, the start
/// commit and worktree adds on that repository never race, while other projects set up
/// at the same time. Takes the project folder, compared after resolving links.
/// Output: the lock, held until dropped.
fn repository_setup(root: &Path) -> RepositorySetup {
    let key = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut busy = REPOSITORIES_SETTING_UP
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    while busy.contains(&key) {
        busy = REPOSITORY_FREED
            .wait(busy)
            .unwrap_or_else(PoisonError::into_inner);
    }
    assert!(busy.insert(key.clone()));
    RepositorySetup(key)
}

/// Makes a project folder the engine created a git repository with one empty start commit,
/// one setup per repository at a time. A folder with any `.git` entry is the owner's repository and
/// stays as it is; an owner's folder holding `.crazyagent/`, or that folder itself, never
/// gets a repository from the engine. Takes the project folder and a private home for git.
/// Returns Ok once the folder is a repository, or a plain refusal for an owner's folder.
pub(crate) fn ensure_repository(root: &Path, tmp: &Path) -> Result<(), RunError> {
    let _setup = repository_setup(root);
    if fs::symlink_metadata(root.join(".git")).is_ok() {
        return Ok(());
    }
    let owners = root.ends_with(crate::projects_home::CRAZYAGENT_DIRECTORY)
        || fs::symlink_metadata(root.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).is_ok();
    if owners {
        return Err(RunError::Invalid(
            "the project folder is not a git repository, so agents cannot get their own copy of it; run git init there and make a first commit".into(),
        ));
    }
    let git = |arguments: &[&str]| -> Result<bool, RunError> {
        let mut command = Command::new(GIT);
        command
            .args(GIT_SAFE)
            .args(arguments)
            .current_dir(root)
            .env_clear()
            .env("HOME", tmp)
            .env("TMPDIR", tmp)
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        wait_bounded(crate::spawn_gate::spawn(&mut command)?, SETUP_TIMEOUT)
    };
    let created = git(&["init", "-q", "--template="])?
        && git(&[
            "-c",
            "user.name=CrazyAgent",
            "-c",
            "user.email=engine@crazyagent.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "Start project",
        ])?;
    if !created {
        return Err(RunError::Invalid(
            "could not create the project repository".into(),
        ));
    }
    let info = root.join(".git").join("info");
    fs::create_dir_all(&info)?;
    let mut exclude = open_append(&info.join("exclude"))?;
    exclude.write_all(b"/worktrees/\n/context/\n")?;
    Ok(())
}

/// Refuses to add a worktree with git under a Seatbelt profile, which Windows cannot load;
/// no git runs unconfined in its place. Takes the sandbox, profile and worktree, all unused.
/// Returns the sandbox's `Unavailable` refusal with the Seatbelt reason.
#[cfg(not(unix))]
pub(crate) fn add_worktree(
    _sandbox: &ProjectSandbox,
    _profile: &Path,
    _worktree: &Path,
) -> Result<(), RunError> {
    Err(RunError::Sandbox(SandboxError::Unavailable(
        crate::sandbox::windows::SEATBELT_MISSING.into(),
    )))
}

/// Adds a detached git worktree at `worktree` inside the sandbox's project folder, running
/// git under the installed profile so that no command the repository configures runs: the
/// worktree is added without a checkout, its effective config is listed in the worktree's
/// own context, and the checkout blanks every filter driver that listing names.
/// Takes the sandbox, its installed profile and the new worktree path.
#[cfg(unix)]
pub(crate) fn add_worktree(
    sandbox: &ProjectSandbox,
    profile: &Path,
    worktree: &Path,
) -> Result<(), RunError> {
    let _setup = repository_setup(sandbox.project_root());
    let worktrees = crate::projects_home::state_folder(sandbox.project_root()).join("worktrees");
    if worktree.parent() != Some(worktrees.as_path()) {
        return Err(RunError::Invalid("worktree must sit in worktrees/".into()));
    }
    fs::create_dir_all(&worktrees)?;
    let mut add = worktree_git(sandbox, profile);
    add.args(["worktree", "add", "-q", "--no-checkout", "--detach"])
        .arg(worktree)
        .arg("HEAD");
    if !wait_bounded(crate::spawn_gate::spawn(&mut add)?, SETUP_TIMEOUT)? {
        return Err(RunError::Invalid("git worktree add failed".into()));
    }
    let mut listing = worktree_git(sandbox, profile);
    listing
        .arg("-C")
        .arg(worktree)
        .args(["config", "--list", "--name-only", "-z"]);
    let overrides = filter_overrides(&bounded_stdout(&mut listing)?)?;
    let mut checkout = worktree_git(sandbox, profile);
    checkout
        .args(&overrides)
        .arg("-C")
        .arg(worktree)
        .args(["reset", "-q", "--hard", "HEAD"]);
    if !wait_bounded(crate::spawn_gate::spawn(&mut checkout)?, SETUP_TIMEOUT)? {
        return Err(RunError::Invalid(
            "git could not check out the worktree".into(),
        ));
    }
    assert!(worktree.starts_with(sandbox.project_root()));
    Ok(())
}

/// A git command for the engine's own worktree steps under the agent's installed profile:
/// no global config, no lazy fetch and the [`GIT_SAFE`] settings, with no input or output.
/// Takes the sandbox and its installed profile. Returns the command, ready for arguments.
#[cfg(unix)]
fn worktree_git(sandbox: &ProjectSandbox, profile: &Path) -> Command {
    let mut git = sandbox.command(profile, Path::new(GIT));
    git.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(GIT_SAFE)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    git
}

/// Runs a setup git command whose output fits in [`GIT_CONFIG_LISTING_BYTES_MAX`] within
/// the setup time limit. Takes the command. Returns what it printed, or `Invalid` when it
/// fails, runs past the limit or prints more.
#[cfg(unix)]
fn bounded_stdout(command: &mut Command) -> Result<Vec<u8>, RunError> {
    let refused = || RunError::Invalid("git could not list the worktree's config".into());
    command.stdout(Stdio::piped());
    let mut child = crate::spawn_gate::spawn(command)?;
    let stdout = child.stdout.take().ok_or_else(refused)?;
    if !wait_bounded(child, SETUP_TIMEOUT)? {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    stdout
        .take(GIT_CONFIG_LISTING_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > GIT_CONFIG_LISTING_BYTES_MAX {
        return Err(refused());
    }
    Ok(bytes)
}

/// The `-c` arguments that switch off every filter driver a config listing names: empty
/// smudge, clean and process commands and `required=false`, so a checkout runs none of them
/// and still succeeds. Takes the NUL-separated keys of `git config --list --name-only -z`.
/// Returns the arguments, or `Invalid` for a driver name a `-c` cannot carry or too many.
#[cfg(unix)]
fn filter_overrides(listing: &[u8]) -> Result<Vec<String>, RunError> {
    let refused = || {
        RunError::Invalid(
            "the repository configures a git filter the engine cannot switch off; name its filters with letters, digits, dots, dashes or underscores".into(),
        )
    };
    let mut drivers: Vec<&str> = Vec::new();
    for key in listing
        .split(|byte| *byte == 0)
        .filter(|key| !key.is_empty())
    {
        let key = std::str::from_utf8(key).map_err(|_| refused())?;
        let Some((driver, _)) = key
            .strip_prefix("filter.")
            .and_then(|rest| rest.rsplit_once('.'))
        else {
            continue;
        };
        if driver.is_empty()
            || !driver
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            return Err(refused());
        }
        if !drivers.contains(&driver) {
            drivers.push(driver);
        }
        if drivers.len() > FILTER_DRIVERS_MAX {
            return Err(refused());
        }
    }
    let mut arguments = Vec::with_capacity(drivers.len() * 8);
    for driver in &drivers {
        for setting in ["smudge=", "clean=", "process=", "required=false"] {
            arguments.push("-c".to_owned());
            arguments.push(format!("filter.{driver}.{setting}"));
        }
    }
    assert_eq!(arguments.len(), drivers.len() * 8);
    assert!(
        arguments
            .iter()
            .skip(1)
            .step_by(2)
            .all(|argument| argument.starts_with("filter."))
    );
    Ok(arguments)
}

fn wait_bounded(mut child: Child, limit: Duration) -> Result<bool, RunError> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if started.elapsed() >= limit {
            stop_timed_out(&mut child);
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Stops a setup command that ran past its limit: kills the process group it leads,
/// reaps it and waits for the group to empty. Takes the command's process.
#[cfg(unix)]
fn stop_timed_out(child: &mut Child) {
    let group = child.id();
    stop_group(group);
    let _ = child.wait();
    await_group_exit(group);
}

/// Stops a setup command that ran past its limit where no process group exists: kills the
/// command and reaps it. Takes the command's process.
#[cfg(not(unix))]
fn stop_timed_out(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// A running agent and the process group it leads. Stopping it, which also
/// happens when it is dropped while a panic unwinds or a launch step fails,
/// kills the whole group, reaps the agent and waits until the group is empty,
/// so no agent process outlives the run that started it. Its [`Lifeline`]
/// kills the group if the engine itself dies first.
pub(crate) struct AgentProcess {
    pub(crate) child: Child,
    pub(crate) group: u32,
    #[cfg(unix)]
    lifeline: Lifeline,
    stopped: bool,
}

impl AgentProcess {
    /// Takes a just spawned agent that leads its own process group and starts
    /// its lifeline. Returns the guard, or the error that kept the lifeline from
    /// starting, after stopping the agent's group.
    pub(crate) fn new(child: Child) -> io::Result<Self> {
        let group = child.id();
        assert!(group > 1);
        #[cfg(unix)]
        let lifeline = match Lifeline::watch(group) {
            Ok(lifeline) => lifeline,
            Err(error) => {
                let mut child = child;
                stop_group(group);
                let _ = child.wait();
                await_group_exit(group);
                return Err(error);
            }
        };
        Ok(Self {
            child,
            group,
            #[cfg(unix)]
            lifeline,
            stopped: false,
        })
    }

    /// Kills the agent's group once, reaps the agent and waits for the group
    /// to empty. Later calls do nothing, so a reused group id is never killed.
    #[cfg(unix)]
    pub(crate) fn stop(&mut self) {
        if self.stopped {
            return;
        }
        stop_group(self.group);
        let _ = self.child.wait();
        await_group_exit(self.group);
        self.lifeline.release();
        self.stopped = true;
    }

    /// Kills the agent and reaps it where no process group exists, once; later calls do
    /// nothing. Takes the process. Returns nothing.
    #[cfg(not(unix))]
    pub(crate) fn stop(&mut self) {
        if self.stopped {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stopped = true;
    }
}

impl Drop for AgentProcess {
    /// Stops the agent's process group if the run did not already stop it.
    fn drop(&mut self) {
        self.stop();
    }
}

/// Kills an agent's process group if the engine dies while the agent runs. A
/// `/bin/sh` child in its own process group reads a pipe only the engine holds
/// and kills the group once that pipe closes without the word `done`, as it
/// does when the engine dies. Releasing it sends `done` and reaps it.
#[cfg(unix)]
pub(crate) struct Lifeline {
    child: Child,
    released: bool,
}

#[cfg(unix)]
impl Lifeline {
    /// Starts the lifeline of a running agent. Takes the group id the agent
    /// leads, above 1. Returns the lifeline, or the error from starting `/bin/sh`.
    pub(crate) fn watch(group: u32) -> io::Result<Self> {
        use std::os::unix::process::CommandExt;
        assert!(group > 1);
        let child = crate::spawn_gate::spawn(
            Command::new("/bin/sh")
                .env_clear()
                .args(["-c", LIFELINE, "lifeline", &group.to_string()])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0),
        )?;
        Ok(Self {
            child,
            released: false,
        })
    }

    /// Tells the lifeline the engine ended the agent itself, so it exits
    /// without killing, and reaps it; later calls do nothing. Called once the
    /// agent's group is stopped, so the group id is never killed after reuse.
    pub(crate) fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        if let Some(mut pipe) = self.child.stdin.take() {
            let _ = pipe.write_all(b"done\n");
        }
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
impl Drop for Lifeline {
    /// Releases the lifeline if the run did not already.
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(unix)]
fn stop_group(group: u32) {
    let Ok(raw) = i32::try_from(group) else {
        return;
    };
    let Some(pid) = rustix::process::Pid::from_raw(raw) else {
        return;
    };
    assert!(pid.as_raw_nonzero().get() > 1);
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
}

/// Waits until no process of a killed agent's group is left, killing again
/// any member that is still there. Takes the group id; returns once the group
/// is empty or after `GROUP_EXIT_WAIT`, so a run never reports an agent as
/// stopped while one of its processes still lingers unnoticed.
#[cfg(unix)]
fn await_group_exit(group: u32) {
    let Some(pid) = i32::try_from(group)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return;
    };
    let started = Instant::now();
    while rustix::process::test_kill_process_group(pid).is_ok()
        && started.elapsed() < GROUP_EXIT_WAIT
    {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Copies an agent stream into its log, counting every byte read and logging
/// at most `log_max` bytes, until the stream ends.
pub(crate) fn drain(
    mut stream: Box<dyn Read + Send>,
    counted: &AtomicU64,
    mut log: File,
    log_max: u64,
) {
    let mut buffer = [0_u8; 8192];
    let mut logged = 0_u64;
    while let Ok(read) = stream.read(&mut buffer) {
        if read == 0 {
            break;
        }
        counted.fetch_add(read as u64, Ordering::Relaxed);
        let room = log_max.saturating_sub(logged).min(read as u64) as usize;
        if room > 0 && log.write_all(&buffer[..room]).is_ok() {
            logged += room as u64;
        }
        assert!(logged <= log_max);
    }
}

/// Refuses an owner-only research folder where no 0700 directory can be made.
/// Takes the path, unused. Returns `research_runner_unavailable`.
#[cfg(not(unix))]
pub(crate) fn private_directory(_path: &Path) -> io::Result<()> {
    Err(unavailable())
}

/// Creates a 0700 directory, or accepts an existing real directory there.
#[cfg(unix)]
pub(crate) fn private_directory(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_dir() {
                Ok(())
            } else {
                Err(io::Error::other("research state path is not a directory"))
            }
        }
        Err(error) => Err(error),
    }
}

/// The plain reason a read-only agent stopped with an error: Codex's own
/// message from its last `turn.failed` event, else its last `error` event,
/// else the generic reason; a message that is a provider's JSON error body
/// gives its inner message. Takes the agent's stdout log, its harness and
/// the log bound; the log is read bounded and never through a link.
#[cfg(unix)]
fn agent_failure(log: &Path, harness: ResearchHarness, max: u64) -> String {
    const GENERIC: &str = "Agent stopped with an error";
    if harness != ResearchHarness::Codex {
        return GENERIC.to_owned();
    }
    let Ok(text) = read_bounded(log, max) else {
        return GENERIC.to_owned();
    };
    let (mut turn, mut error) = (None, None);
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let (slot, message) = match event["type"].as_str() {
            Some("turn.failed") => (&mut turn, &event["error"]["message"]),
            Some("error") => (&mut error, &event["message"]),
            _ => continue,
        };
        if let Some(message) = message.as_str().map(str::trim).filter(|m| !m.is_empty()) {
            let inner = serde_json::from_str::<serde_json::Value>(message)
                .ok()
                .and_then(|body| body["error"]["message"].as_str().map(str::to_owned));
            *slot = Some(inner.unwrap_or_else(|| message.to_owned()));
        }
    }
    turn.or(error).map_or_else(
        || GENERIC.to_owned(),
        |message| format!("Agent stopped: {message}"),
    )
}

fn read_bounded(path: &Path, max: u64) -> io::Result<String> {
    let file = open_read(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > max {
        return Err(io::Error::other("report is not a bounded regular file"));
    }
    let mut text = String::new();
    file.take(max).read_to_string(&mut text)?;
    Ok(text)
}

fn research_prompt(root: &Path, kind: &str) -> Result<String, RunError> {
    let context = crate::projects_home::state_folder(root).join("context");
    let brief = match read_bounded(&context.join("brief.md"), BRIEF_BYTES_MAX) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    let documents_dir = context.join("documents");
    let mut documents = Vec::new();
    if let Ok(entries) = fs::read_dir(&documents_dir) {
        for entry in entries.flatten().take(DOCUMENTS_LISTED_MAX) {
            if entry.file_type().is_ok_and(|kind| kind.is_file())
                && let Some(name) = entry.file_name().to_str()
                && !name.chars().any(char::is_control)
            {
                documents.push(name.to_string());
            }
        }
    }
    documents.sort();
    let label = kind_label(kind);
    let mut prompt = format!(
        "You are a research agent for the project described below. Write a {} report.\n\n",
        label.to_lowercase()
    );
    prompt.push_str("Project brief:\n");
    if brief.trim().is_empty() {
        prompt.push_str("(The owner has not written a brief yet.)\n");
    } else {
        prompt.push_str(brief.trim());
        prompt.push('\n');
    }
    if !documents.is_empty() {
        prompt.push_str(&format!(
            "\nThe owner attached these documents in {}: {}. Read them if they help.\n",
            documents_dir.display(),
            documents.join(", ")
        ));
    }
    prompt.push_str(&format!("\nTask: {}\n", kind_task(kind, &label)));
    prompt.push_str(
        "\nRules:\n- Work only inside your current folder; do not change files anywhere else.\n- Reply with the full report in Markdown, with short headings.\n- Cite sources inline as Markdown links.\n- End with a section \"## Links and references\" that lists every source as a Markdown link.\n",
    );
    Ok(prompt)
}

/// The plain heading for a research kind, such as "Market research".
pub fn kind_label(kind: &str) -> String {
    match kind {
        "market_research" => "Market research".into(),
        "competitor_research" => "Competitor research".into(),
        "design_research" => "Design research".into(),
        "front_end_design" => "Front-end design".into(),
        "tech_stack_research" => "Tech stack research".into(),
        other => {
            let words = other.replace(['_', '-'], " ");
            let mut chars = words.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => "Research".into(),
            }
        }
    }
}

fn kind_task(kind: &str, label: &str) -> String {
    match kind {
        "market_research" => "Describe the market: target users and their needs, market size signals, pricing norms and trends that matter for this project.".into(),
        "competitor_research" => "Find the main competitors and alternatives, compare their features, pricing and positioning, and point out gaps this project can use.".into(),
        "design_research" => "Collect design patterns, reference products and user experience conventions that fit this project and its users.".into(),
        "front_end_design" => "Propose the front-end: key screens, navigation, main components and a visual direction, with reasons.".into(),
        "tech_stack_research" => "Compare technologies that suit this project and recommend a stack with clear reasons and tradeoffs.".into(),
        _ => format!("Research this topic for the project: {label}."),
    }
}

fn render_report(label: &str, harness: &str, body: &str) -> String {
    let mut has_links = false;
    let body: Vec<&str> = body
        .lines()
        .map(|line| {
            if line.trim().eq_ignore_ascii_case(LINKS_HEADING) {
                has_links = true;
                LINKS_HEADING
            } else {
                line
            }
        })
        .collect();
    let body = body.join("\n");
    let mut report = format!(
        "# {label}\n\nResearched by {harness} on {}.\n\n{body}\n",
        utc_date(SystemTime::now())
    );
    if !has_links {
        report.push_str(&format!("\n{LINKS_HEADING}\n\n"));
        let links = links_in(&body);
        if links.is_empty() {
            report.push_str("No links were returned by the agent.\n");
        }
        for link in links {
            report.push_str(&format!("- <{link}>\n"));
        }
    }
    assert!(report.starts_with("# "));
    report
}

fn links_in(text: &str) -> Vec<String> {
    let mut links: Vec<String> = Vec::new();
    for (start, _) in text.match_indices("http") {
        let rest = &text[start..];
        if !(rest.starts_with("http://") || rest.starts_with("https://")) {
            continue;
        }
        let end = rest
            .find(|c: char| c.is_whitespace() || "()<>[]\"'`".contains(c))
            .unwrap_or(rest.len());
        let link = rest[..end].trim_end_matches(['.', ',', ';', ':']);
        if link.len() > "https://".len() && !links.iter().any(|known| known == link) {
            links.push(link.to_string());
        }
        if links.len() >= LINKS_MAX {
            break;
        }
    }
    links
}

/// Refuses to publish a research report where its no-follow staged write is not made.
/// Takes the project root, kind and report, all unused. Returns `research_runner_unavailable`.
#[cfg(not(unix))]
fn write_report(_root: &Path, _kind: &str, _report: &str) -> io::Result<PathBuf> {
    Err(unavailable())
}

#[cfg(unix)]
fn write_report(root: &Path, kind: &str, report: &str) -> io::Result<PathBuf> {
    assert!(valid_kind(kind));
    let context = crate::projects_home::state_folder(root).join("context");
    let research = context.join("research");
    for directory in [&context, &research] {
        match fs::DirBuilder::new().mode(0o755).create(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        if !fs::symlink_metadata(directory)?.file_type().is_dir() {
            return Err(io::Error::other("research folder is not a directory"));
        }
    }
    let target = research.join(format!("{kind}.md"));
    let staging = research.join(format!(".{kind}.md.{}.tmp", std::process::id()));
    let mut file: File = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&staging)?;
    let written = file
        .write_all(report.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&staging, &target));
    if let Err(error) = written {
        let _ = fs::remove_file(&staging);
        return Err(error);
    }
    Ok(target)
}

fn utc_date(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (seconds / 86_400) as i64;
    let minutes = (seconds % 86_400) / 60;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    assert!((1..=12).contains(&month) && (1..=31).contains(&day));
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        minutes / 60,
        minutes % 60
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::{ModelAuthentication, ModelEndpoint, ModelReference};
    use crate::protocol::{ConfigId, CredentialRef, Cursor};
    use std::os::unix::fs::PermissionsExt;

    /// Setting up one project's repository must not wait for another project's setup:
    /// the lock covers one repository, so unrelated projects set up at the same time.
    #[test]
    fn a_project_repository_sets_up_while_another_projects_setup_holds_its_lock() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let (busy, free, tmp) = (base.join("busy"), base.join("free"), base.join("tmp"));
        for folder in [&busy, &free, &tmp] {
            fs::create_dir(folder).unwrap();
        }
        let _held = repository_setup(&busy);
        let (done, finished) = std::sync::mpsc::channel();
        let root = free.clone();
        std::thread::spawn(move || {
            let _ = done.send(ensure_repository(&root, &tmp).map_err(|e| e.to_string()));
        });
        finished
            .recv_timeout(Duration::from_secs(30))
            .expect("the free project waited for the busy project's setup lock")
            .unwrap();
        assert!(free.join(".git").is_dir());
        assert!(!busy.join(".git").exists());
    }

    struct EngineRoot {
        _temp: tempfile::TempDir,
        base: PathBuf,
        database: PathBuf,
        state: PathBuf,
    }

    fn private_directory_at(path: &Path) {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn engine_root() -> EngineRoot {
        let temp = tempfile::Builder::new()
            .prefix("ca-research-lease-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let base = temp.path().canonicalize().unwrap();
        let engine = base.join("engine");
        private_directory_at(&engine);
        let database = engine.join("engine.db");
        let sqlite = rusqlite::Connection::open(&database).unwrap();
        sqlite.execute_batch("PRAGMA user_version=1").unwrap();
        sqlite.close().map_err(|(_, error)| error).unwrap();
        fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
        let state = engine.join("research");
        private_directory_at(&state);
        EngineRoot {
            _temp: temp,
            base,
            database,
            state,
        }
    }

    fn reference(value: &str) -> CredentialRef {
        CredentialRef::new(value).unwrap()
    }

    fn host() -> HostStamp {
        HostStamp {
            db_instance: "a".repeat(64),
            engine_epoch: "b".repeat(64),
        }
    }

    /// The managed Codex connection these lease tests sign in with.
    fn codex_connection() -> ModelConnection {
        ModelConnection {
            id: reference("codex-connection"),
            provider: reference("openai"),
            harness: reference("codex"),
            protocol: reference("native"),
            account_ref: reference("codex-account"),
            endpoint: ModelEndpoint::Native {},
            model: ModelReference {
                id: "native-default".into(),
                author: reference("openai"),
            },
            auth: ModelAuthentication::NativeManaged {
                profile_ref: reference("codex-profile"),
            },
        }
    }

    /// A research agent on a managed ChatGPT account gets neither the account's ChatGPT
    /// apps nor its remote plugins, with tools on or off, as team agents and the master.
    #[test]
    fn a_managed_codex_research_agent_gets_no_chatgpt_apps_or_plugins() {
        let account = ResearchAccount::ManagedCodex {
            connection: codex_connection(),
            model: None,
        };
        for tools in [false, true] {
            let mut command = Command::new("/usr/bin/true");
            codex_arguments(
                &mut command,
                Path::new("/work"),
                Path::new("/work/report.md"),
                Path::new("/home"),
                tools,
                &account,
            )
            .unwrap();
            let args: Vec<&str> = command
                .get_args()
                .map(|argument| argument.to_str().unwrap())
                .collect();
            for setting in ["features.apps=false", "features.plugins=false"] {
                assert!(
                    args.windows(2).any(|pair| pair == ["-c", setting]),
                    "{tools} {setting}: {args:?}"
                );
            }
        }
    }

    /// A research job's lease names the job as a holder of its account in the agent
    /// register while the lease lives, and no longer once it ends.
    #[test]
    fn a_research_lease_names_its_job_as_a_holder_of_the_account_until_it_ends() {
        let root = engine_root();
        let agents = Arc::new(Agents::open(root.database.parent().unwrap()).unwrap());
        let runner = ResearchRunner::new(
            &root.database,
            &root.state,
            Path::new("/usr/bin/true"),
            ResearchLimits::default(),
        )
        .unwrap()
        .with_agents(agents.clone());
        let host = host();
        let connection = codex_connection();
        let custody_id = {
            let mut custody = runner.custody.lock().unwrap();
            custody
                .as_mut()
                .unwrap()
                .prepare(&host, &connection)
                .unwrap()
                .0
        };
        let binding = binding(&host, &connection, custody_id);
        let job = ResearchJob {
            id: 7,
            project: "alpha".into(),
            kind: "market_research".into(),
            harness: "codex".into(),
            tools: false,
            status: ResearchStatus::Running,
            attempts: 1,
            claimer: Some("runner-test".into()),
            report_path: None,
            error: None,
            cancel_requested: false,
            output_bytes: 0,
            created_ms: 1,
            started_ms: Some(1),
            heartbeat_ms: None,
            finished_ms: None,
        };
        let project = ProjectId::new("alpha").unwrap();
        let held = runner
            .held_lease(&host, &binding, "job-7-1", &project, &job)
            .unwrap();
        assert_eq!(
            agents.holders(&connection.id),
            vec![Holder::Research {
                project,
                job: 7,
                title: kind_label("market_research"),
            }]
        );
        assert!(agents.holders(&reference("other-connection")).is_empty());
        drop(held);
        assert!(agents.holders(&connection.id).is_empty());
    }

    fn binding(
        host: &HostStamp,
        connection: &ModelConnection,
        custody_id: String,
    ) -> NativeProfileBinding {
        NativeProfileBinding {
            database_instance: host.db_instance.clone(),
            engine_epoch: host.engine_epoch.clone(),
            config_id: ConfigId::new("c".repeat(64)).unwrap(),
            source_hash: "d".repeat(64),
            at_revision: Cursor::new(1),
            connection: connection.clone(),
            custody_id,
        }
    }

    #[test]
    fn three_research_launches_share_one_managed_profile_while_a_sign_in_waits() {
        let root = engine_root();
        let runner = ResearchRunner::new(
            &root.database,
            &root.state,
            Path::new("/usr/bin/true"),
            ResearchLimits::default(),
        )
        .unwrap();
        let host = host();
        let connection = codex_connection();
        let (custody_id, default_home) = {
            let mut custody = runner.custody.lock().unwrap();
            let custody = custody.as_mut().unwrap();
            let (custody_id, _) = custody.prepare(&host, &connection).unwrap();
            (
                custody_id,
                custody.profile_home(&reference("codex-profile")),
            )
        };
        let binding = binding(&host, &connection, custody_id);
        let leases = (1..=3)
            .map(|job| {
                runner
                    .lease(&host, &binding, &format!("job-{job}-1"), 1)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for lease in &leases {
            assert_eq!(lease.profile_path().unwrap(), default_home);
        }
        let mut sign_in =
            NativeProfileCustody::capture(&root.database, &fs::metadata(&root.database).unwrap())
                .unwrap();
        let operation = RequestId::new("sign-in").unwrap();
        let authentication = NativeProfileUsePurpose::Authentication;
        assert_eq!(
            sign_in
                .acquire_use(&host, &binding, &operation, 1, authentication)
                .err()
                .expect("a sign-in waits for running agents")
                .0,
            PROFILE_BUSY
        );
        drop(leases);
        let signing_in = sign_in
            .acquire_use(&host, &binding, &operation, 1, authentication)
            .unwrap();
        assert!(matches!(
            runner.lease(&host, &binding, "job-4-1", 1),
            Err(RunError::Busy)
        ));
        drop(signing_in);
        let holding = AtomicU64::new(0);
        let most = AtomicU64::new(0);
        let granted = AtomicU64::new(0);
        let refused = AtomicU64::new(0);
        std::thread::scope(|scope| {
            for worker in 0..6_u64 {
                let (runner, binding, host) = (&runner, &binding, &host);
                let (holding, most, granted, refused) = (&holding, &most, &granted, &refused);
                scope.spawn(move || {
                    for round in 0..20_u64 {
                        let name = format!("job-{worker}-{round}");
                        match runner.lease(host, binding, &name, round + 1) {
                            Ok(lease) => {
                                let now = holding.fetch_add(1, Ordering::SeqCst) + 1;
                                most.fetch_max(now, Ordering::SeqCst);
                                granted.fetch_add(1, Ordering::SeqCst);
                                std::thread::sleep(Duration::from_millis(5));
                                holding.fetch_sub(1, Ordering::SeqCst);
                                drop(lease);
                            }
                            Err(RunError::Busy) => {
                                refused.fetch_add(1, Ordering::SeqCst);
                            }
                            Err(other) => panic!("research lease failed: {other}"),
                        }
                    }
                });
            }
        });
        assert!(most.load(Ordering::SeqCst) >= 1);
        assert_eq!(refused.load(Ordering::SeqCst), 0);
        assert_eq!(granted.load(Ordering::SeqCst), 120);
        assert!(runner.lease(&host, &binding, "job-3-1", 1).is_ok());
    }

    #[test]
    fn a_configured_profile_folder_overrides_the_default_managed_home() {
        let root = engine_root();
        let mut custody =
            NativeProfileCustody::capture(&root.database, &fs::metadata(&root.database).unwrap())
                .unwrap();
        let host = host();
        let home = root.base.join("configured-home");
        private_directory_at(&home);
        private_directory_at(&home.join(".backboard"));
        let config = serde_json::json!({
            "model": { "provider": "local-provider", "model": "local-model" },
            "providers": [{
                "id": "local-provider", "name": "Local provider",
                "protocol": "openai-chat", "baseUrl": "http://127.0.0.1:11434/v1",
                "auth": { "type": "none" }
            }]
        });
        let file = home.join(".backboard").join("config.json");
        fs::write(&file, serde_json::to_vec(&config).unwrap()).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let connection = ModelConnection {
            id: reference("configured-connection"),
            provider: reference("local-provider"),
            harness: reference("rcli"),
            protocol: reference("openai-chat"),
            account_ref: reference("declared-account"),
            endpoint: ModelEndpoint::Http {
                url: "http://127.0.0.1:11434/v1".into(),
            },
            model: ModelReference {
                id: "local-model".into(),
                author: reference("local-author"),
            },
            auth: ModelAuthentication::None {},
        };
        let profile = reference("declared-profile");
        let metadata = custody
            .register_configured_rcli_profile(&host, &connection, &profile, &home)
            .unwrap();
        let binding = binding(&host, &connection, metadata.custody_id.clone());
        let operation = RequestId::new("research-job-1-1").unwrap();
        let lease = custody
            .acquire_configured_rcli_use(&host, &binding, &profile, &operation, 1)
            .unwrap();
        assert_eq!(lease.profile_path().unwrap(), home);
        assert_ne!(
            lease.profile_path().unwrap(),
            custody.profile_home(&profile)
        );
        drop(lease);
    }

    fn group_members(group: u32) -> usize {
        let output = crate::spawn_gate::output(
            Command::new("/usr/bin/pgrep")
                .env_clear()
                .arg("-g")
                .arg(group.to_string())
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .stdout(std::process::Stdio::piped()),
        )
        .unwrap();
        String::from_utf8_lossy(&output.stdout).lines().count()
    }

    #[test]
    fn an_agent_dropped_while_a_panic_unwinds_leaves_no_process_in_its_group() {
        use std::os::unix::process::CommandExt;
        const MID_RUN: &str = "the run fails while its agent and the agent's child are running";
        let child = crate::spawn_gate::spawn(
            Command::new("/bin/sh")
                .env_clear()
                .args(["-c", "/bin/sleep 120 & /bin/sleep 120"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0),
        )
        .unwrap();
        let group = child.id();
        let pid = rustix::process::Pid::from_raw(i32::try_from(group).unwrap()).unwrap();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let agent = AgentProcess::new(child).unwrap();
            let until = Instant::now() + Duration::from_secs(10);
            while group_members(agent.group) < 2 {
                assert!(Instant::now() < until, "the agent never started its child");
                std::thread::sleep(Duration::from_millis(20));
            }
            std::panic::panic_any(MID_RUN);
        }));
        let payload = unwound.expect_err("the run must panic mid-run");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&MID_RUN));
        assert_eq!(group_members(group), 0);
        assert!(rustix::process::test_kill_process_group(pid).is_err());
    }

    #[test]
    fn a_lifeline_kills_its_agent_group_only_when_its_pipe_closes_without_done() {
        use std::os::unix::process::CommandExt;
        let spawn = || {
            let agent = crate::spawn_gate::spawn(
                Command::new("/bin/sh")
                    .env_clear()
                    .args(["-c", "/bin/sleep 120 & /bin/sleep 120"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0),
            )
            .unwrap();
            let until = Instant::now() + Duration::from_secs(10);
            while group_members(agent.id()) < 2 {
                assert!(Instant::now() < until, "the agent never started its child");
                std::thread::sleep(Duration::from_millis(20));
            }
            agent
        };
        let mut kept = spawn();
        let kept_group = kept.id();
        Lifeline::watch(kept_group).unwrap().release();
        assert!(
            group_members(kept_group) >= 2,
            "a released lifeline killed the agent's group"
        );
        stop_group(kept_group);
        kept.wait().unwrap();
        await_group_exit(kept_group);
        let mut orphan = spawn();
        let group = orphan.id();
        let mut lifeline = Lifeline::watch(group).unwrap();
        drop(lifeline.child.stdin.take());
        lifeline.child.wait().unwrap();
        orphan.wait().unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while group_members(group) > 0 {
            assert!(
                Instant::now() < until,
                "the lifeline left the agent's group running"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let pid = rustix::process::Pid::from_raw(i32::try_from(group).unwrap()).unwrap();
        assert!(rustix::process::test_kill_process_group(pid).is_err());
    }

    #[test]
    fn utc_date_formats_known_instants() {
        assert_eq!(utc_date(UNIX_EPOCH), "1970-01-01 00:00 UTC");
        let instant = UNIX_EPOCH + Duration::from_secs(1_792_729_920);
        assert_eq!(utc_date(instant), "2026-10-23 04:32 UTC");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400);
        assert_eq!(utc_date(leap), "2000-02-29 00:00 UTC");
    }

    #[test]
    fn links_are_collected_once_without_trailing_punctuation() {
        let links = links_in(
            "See [Vet](https://vet.example/a), https://vet.example/a. and <http://b.example/x>; https://",
        );
        assert_eq!(links, vec!["https://vet.example/a", "http://b.example/x"]);
    }

    #[test]
    fn report_appends_links_only_when_the_agent_left_them_out() {
        let added = render_report("Market research", "Codex", "Body with https://a.example/x");
        assert!(added.contains("## Links and references\n\n- <https://a.example/x>\n"));
        let kept = render_report(
            "Market research",
            "Codex",
            "Body\n\n## Links and references\n- [A](https://a.example)",
        );
        assert_eq!(kept.matches("## Links and references").count(), 1);
        let lower = render_report(
            "Market research",
            "Codex",
            "Body\n\n### Links and References\n- [A](https://a.example/y)",
        );
        assert!(lower.contains("\n## Links and references\n\n- <https://a.example/y>\n"));
        let capital = render_report(
            "Market research",
            "Codex",
            "Body\n\n  ## Links and References  \n- [A](https://a.example/z)",
        );
        assert_eq!(capital.matches("## Links and references\n").count(), 1);
        assert!(!capital.contains("References"));
        assert!(capital.contains("- [A](https://a.example/z)"));
        let empty = render_report("Market research", "Codex", "Body only");
        assert!(empty.contains("No links were returned by the agent."));
    }

    #[test]
    fn kind_labels_are_plain() {
        assert_eq!(kind_label("market_research"), "Market research");
        assert_eq!(kind_label("front_end_design"), "Front-end design");
        assert_eq!(kind_label("pricing-study"), "Pricing study");
    }

    #[test]
    fn endpoint_urls_with_credentials_or_quotes_are_refused() {
        assert!(endpoint_url("http://127.0.0.1:11434/v1").is_ok());
        assert!(endpoint_url("http://user:secret@127.0.0.1:11434/v1").is_err());
        assert!(endpoint_url("http://127.0.0.1:11434/v1\"").is_err());
        assert!(endpoint_url("file:///etc/passwd").is_err());
        let loopback = ResearchAccount::Endpoint {
            base_url: "http://localhost:11434/v1".into(),
            model: "m".into(),
        };
        assert_eq!(
            model_remote(&loopback).unwrap(),
            SandboxRemote::Loopback(11434)
        );
        let remote = ResearchAccount::Endpoint {
            base_url: "https://openrouter.ai/api/v1".into(),
            model: "m".into(),
        };
        assert_eq!(model_remote(&remote).unwrap(), SandboxRemote::AnyHost(443));
    }

    #[test]
    fn adding_an_agent_worktree_never_runs_the_project_fsmonitor() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let project = base.join("project");
        let home = base.join("agent").join("home");
        let tmp = base.join("agent").join("tmp");
        let profiles = base.join("profiles");
        for folder in [&project, &home, &tmp, &profiles] {
            fs::create_dir_all(folder).unwrap();
        }
        ensure_repository(&project, &tmp).unwrap();
        let marker = tmp.join("fsmonitor-ran");
        let monitor = tmp.join("monitor.sh");
        fs::write(
            &monitor,
            format!("#!/bin/sh\necho ran >> '{}'\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&monitor, fs::Permissions::from_mode(0o755)).unwrap();
        let configured = crate::spawn_gate::status(
            Command::new(GIT)
                .env_clear()
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .arg("-C")
                .arg(&project)
                .args(["config", "core.fsmonitor"])
                .arg(&monitor),
        )
        .unwrap();
        assert!(configured.success());
        let sandbox =
            ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new())).unwrap();
        let profile = profiles.join("agent.sb");
        sandbox.install(&profile).unwrap();
        let worktree = project.join("worktrees").join("agent-one");
        add_worktree(&sandbox, &profile, &worktree).unwrap();
        assert!(worktree.join(".git").is_file());
        assert!(
            !marker.exists(),
            "the engine's git ran the project fsmonitor"
        );
    }

    fn repository_git(project: &Path, arguments: &[&str]) {
        let status = crate::spawn_gate::status(
            Command::new(GIT)
                .env_clear()
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(["-c", "core.hooksPath=/dev/null"])
                .args(["-c", "user.name=Test", "-c", "user.email=test@invalid"])
                .arg("-C")
                .arg(project)
                .args(arguments)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        assert!(status.success(), "git {arguments:?}");
    }

    #[test]
    fn filter_overrides_blank_each_named_driver_and_refuse_names_a_flag_cannot_carry() {
        let listing = b"core.bare\0filter.lfs.smudge\0filter.lfs.clean\0filter.a.b.process\0filter.required\0";
        let overrides = filter_overrides(listing).unwrap();
        assert_eq!(overrides.len(), 16);
        for expected in [
            "filter.lfs.smudge=",
            "filter.lfs.clean=",
            "filter.lfs.process=",
            "filter.lfs.required=false",
            "filter.a.b.smudge=",
            "filter.a.b.required=false",
        ] {
            assert!(
                overrides.iter().any(|argument| argument == expected),
                "{expected}"
            );
        }
        assert!(filter_overrides(b"core.bare\0").unwrap().is_empty());
        for refused in [
            &b"filter.x=y.smudge\0"[..],
            b"filter..smudge\0",
            b"filter.a b.smudge\0",
            b"filter.\xff.smudge\0",
        ] {
            assert!(filter_overrides(refused).is_err(), "{refused:?}");
        }
        let many: Vec<u8> = (0..=FILTER_DRIVERS_MAX)
            .flat_map(|index| format!("filter.d{index}.smudge\0").into_bytes())
            .collect();
        assert!(filter_overrides(&many).is_err());
    }

    #[test]
    fn adding_an_agent_worktree_runs_no_command_the_repository_configures() {
        let mut problems = Vec::new();
        for hostile in [
            "smudge",
            "process",
            "required",
            "dotted include",
            "worktree include",
            "info attributes",
            "agent global config",
            "post-checkout hook",
            "unsafe driver name",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let base = temp.path().canonicalize().unwrap();
            let project = base.join("project");
            let home = base.join("agent").join("home");
            let tmp = base.join("agent").join("tmp");
            let profiles = base.join("profiles");
            for folder in [&project, &home, &tmp, &profiles] {
                fs::create_dir_all(folder).unwrap();
            }
            ensure_repository(&project, &tmp).unwrap();
            let marker = tmp.join("ran");
            let command = format!("/usr/bin/touch '{}'; /bin/cat", marker.display());
            fs::write(project.join("a.txt"), "plain\n").unwrap();
            let attributes = match hostile {
                "dotted include" => Some("* filter=a.b\n"),
                "worktree include" => Some("* filter=late\n"),
                "unsafe driver name" => Some("* filter=x=y\n"),
                "info attributes" | "agent global config" | "post-checkout hook" => None,
                _ => Some("* filter=hostile\n"),
            };
            if let Some(attributes) = attributes {
                fs::write(project.join(".gitattributes"), attributes).unwrap();
            }
            repository_git(&project, &["add", "-A"]);
            repository_git(&project, &["commit", "-q", "-m", "files"]);
            let git_dir = project.join(".git");
            match hostile {
                "smudge" => {
                    repository_git(&project, &["config", "filter.hostile.smudge", &command])
                }
                "process" => {
                    repository_git(&project, &["config", "filter.hostile.process", &command])
                }
                "required" => {
                    repository_git(&project, &["config", "filter.hostile.smudge", &command]);
                    repository_git(&project, &["config", "filter.hostile.required", "true"]);
                }
                "dotted include" => {
                    fs::write(
                        git_dir.join("hostile.config"),
                        format!("[filter \"a.b\"]\n\tsmudge = {command}\n"),
                    )
                    .unwrap();
                    repository_git(&project, &["config", "include.path", "hostile.config"]);
                }
                "worktree include" => {
                    fs::write(
                        git_dir.join("late.config"),
                        format!("[filter \"late\"]\n\tsmudge = {command}\n"),
                    )
                    .unwrap();
                    repository_git(
                        &project,
                        &[
                            "config",
                            "includeIf.gitdir:**/worktrees/**.path",
                            &git_dir.join("late.config").display().to_string(),
                        ],
                    );
                }
                "info attributes" => {
                    fs::write(
                        git_dir.join("info").join("attributes"),
                        "* filter=hostile\n",
                    )
                    .unwrap();
                    repository_git(&project, &["config", "filter.hostile.smudge", &command]);
                }
                "agent global config" => {
                    fs::write(home.join("attributes"), "* filter=h\n").unwrap();
                    fs::write(
                        home.join(".gitconfig"),
                        format!(
                            "[core]\n\tattributesFile = {}\n[filter \"h\"]\n\tsmudge = {command}\n",
                            home.join("attributes").display()
                        ),
                    )
                    .unwrap();
                }
                "post-checkout hook" => {
                    let hooks = git_dir.join("hooks");
                    fs::create_dir_all(&hooks).unwrap();
                    let hook = hooks.join("post-checkout");
                    fs::write(
                        &hook,
                        format!("#!/bin/sh\n/usr/bin/touch '{}'\n", marker.display()),
                    )
                    .unwrap();
                    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
                }
                "unsafe driver name" => {
                    repository_git(&project, &["config", "filter.x=y.smudge", &command])
                }
                other => panic!("no setup for {other}"),
            }
            let sandbox =
                ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new()))
                    .unwrap();
            let profile = profiles.join("agent.sb");
            sandbox.install(&profile).unwrap();
            let worktree = project.join("worktrees").join("agent-one");
            let added = add_worktree(&sandbox, &profile, &worktree);
            if marker.exists() {
                problems.push(format!("{hostile}: the repository's command ran"));
            }
            let checked_out = fs::read_to_string(worktree.join("a.txt")).ok();
            match (hostile, &added) {
                ("unsafe driver name", Ok(())) => {
                    problems.push(format!("{hostile}: the worktree was added"))
                }
                ("unsafe driver name", Err(_)) => {}
                (_, Err(error)) => problems.push(format!("{hostile}: {error}")),
                (_, Ok(())) if checked_out.as_deref() != Some("plain\n") => {
                    problems.push(format!("{hostile}: checked out {checked_out:?}"))
                }
                _ => {}
            }
        }
        assert_eq!(problems, Vec::<String>::new());
    }

    #[test]
    fn the_engine_makes_a_repository_only_in_a_folder_it_created() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let home = base.join("git-home");
        fs::create_dir(&home).unwrap();
        let refused = |folder: &Path| match ensure_repository(folder, &home) {
            Err(RunError::Invalid(reason)) => reason.contains("not a git repository"),
            _ => false,
        };

        let owners = base.join("owners");
        fs::create_dir_all(owners.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
        fs::write(owners.join("notes.txt"), "the owner's file\n").unwrap();
        assert!(refused(&owners), "an owner's folder gets no repository");
        assert!(!owners.join(".git").exists());
        let state = owners.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
        assert!(refused(&state), "nor does its .crazyagent folder");
        assert!(!state.join(".git").exists());

        let linked = base.join("linked");
        fs::create_dir(&linked).unwrap();
        fs::write(
            linked.join(".git"),
            "gitdir: /elsewhere/.git/worktrees/linked\n",
        )
        .unwrap();
        ensure_repository(&linked, &home).unwrap();
        assert!(
            fs::symlink_metadata(linked.join(".git")).unwrap().is_file(),
            "a linked worktree stays as the owner made it"
        );

        let created = base.join("created");
        fs::create_dir(&created).unwrap();
        ensure_repository(&created, &home).unwrap();
        assert!(
            created.join(".git").is_dir(),
            "a folder the engine made gets its repository"
        );
        ensure_repository(&created, &home).unwrap();
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "runner/live_claude_tests.rs"]
mod live_claude_tests;
#[cfg(all(test, target_os = "macos"))]
#[path = "runner/live_codex_tests.rs"]
mod live_codex_tests;
