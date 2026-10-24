use super::accounts::PROFILE_BUSY;
use super::jobs::{JobError, ResearchJob, ResearchJobs, ResearchStatus, valid_kind};
use crate::config::ModelConnection;
use crate::engine::bootstrap::{NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::engine::{EngineError, NativeProfileCustody};
use crate::harnesses::launch::LaunchError;
use crate::launch_context::AgentContext;
use crate::protocol::{ProjectId, RequestId};
use crate::sandbox::{ProjectSandbox, SandboxError, SandboxNetwork, SandboxRemote};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const GIT: &str = "/usr/bin/git";
static REPOSITORY_SETUP: Mutex<()> = Mutex::new(());
const BRIEF_BYTES_MAX: u64 = 32 * 1024;
const DOCUMENTS_LISTED_MAX: usize = 50;
const LINKS_MAX: usize = 100;
const POLL: Duration = Duration::from_millis(100);
const HEARTBEAT: Duration = Duration::from_millis(250);
const MODEL_BYTES_MAX: usize = 256;
const URL_BYTES_MAX: usize = 2048;
const SETUP_TIMEOUT: Duration = Duration::from_secs(60);
const GROUP_EXIT_WAIT: Duration = Duration::from_secs(10);
const RUNS_PER_ATTEMPT_MAX: u32 = 100;
const LINKS_HEADING: &str = "## Links and references";
const LOCAL_PLACEHOLDER_KEY: &str = "crazyagent-local";
const CONTEXT_BUSY: &str = "launch_home_busy";
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
    codex: PathBuf,
    claude: Option<PathBuf>,
    limits: ResearchLimits,
    claimer: String,
    stopping: AtomicBool,
    setup: Mutex<()>,
    custody: Mutex<Result<NativeProfileCustody, EngineError>>,
}

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
        if !state.is_absolute() || !state.is_dir() || state.canonicalize()? != state {
            return Err(RunError::Invalid(
                "state directory must be canonical".into(),
            ));
        }
        if !codex.is_absolute() || !codex.is_file() {
            return Err(RunError::Invalid(
                "codex executable must be absolute".into(),
            ));
        }
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
            codex: codex.to_path_buf(),
            claude: None,
            limits,
            claimer: format!("runner-{}-{nanos}", std::process::id()),
            stopping: AtomicBool::new(false),
            setup: Mutex::new(()),
            custody: Mutex::new(custody),
        })
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

    /// The job store this runner claims from.
    pub fn database(&self) -> &Path {
        &self.database
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
            jobs.fail(
                job.id,
                &self.claimer,
                "This harness cannot run research yet",
                1,
            )?;
            return Ok(run(ResearchStatus::Failed, None, None));
        };
        let Some(account) = project
            .account
            .as_ref()
            .filter(|account| account.harness() == harness)
        else {
            jobs.needs_pool(job.id, &self.claimer)?;
            return Ok(run(ResearchStatus::NeedsPool, None, None));
        };
        if harness == ResearchHarness::ClaudeCode && self.claude.is_none() {
            jobs.fail(
                job.id,
                &self.claimer,
                "This harness is not installed on this computer",
                1,
            )?;
            return Ok(run(ResearchStatus::Failed, None, None));
        }
        match self.launch(jobs, &job, project, account) {
            Ok((Ending::Exited(true), group, output)) => {
                let report = match self.publish(&job, harness, project.root, &output) {
                    Ok(report) => report,
                    Err(reason) => {
                        let status =
                            jobs.fail(job.id, &self.claimer, reason, self.limits.attempts_max)?;
                        return Ok(run(status, None, Some(group)));
                    }
                };
                let relative = format!("context/research/{}.md", job.kind);
                jobs.finish(job.id, &self.claimer, &relative)?;
                Ok(run(ResearchStatus::Done, Some(report), Some(group)))
            }
            Ok((ending, group, _)) => {
                if let Ending::Cancelled = ending {
                    jobs.cancelled(job.id, &self.claimer)?;
                    return Ok(run(ResearchStatus::Cancelled, None, Some(group)));
                }
                if let Ending::Interrupted = ending {
                    let _ = jobs.release(job.id, &self.claimer);
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
                let status = jobs.fail(job.id, &self.claimer, reason, self.limits.attempts_max)?;
                Ok(run(status, None, Some(group)))
            }
            Err(RunError::Busy) => {
                jobs.release(job.id, &self.claimer)?;
                let status = jobs
                    .get(job.id)?
                    .map_or(ResearchStatus::Queued, |job| job.status);
                Ok(run(status, None, None))
            }
            Err(RunError::Account(_)) => {
                jobs.needs_pool(job.id, &self.claimer)?;
                Ok(run(ResearchStatus::NeedsPool, None, None))
            }
            Err(error) => {
                let reason = match error {
                    RunError::Context(_) => "Could not give the agent this project's context",
                    _ => "Could not prepare the research sandbox",
                };
                jobs.fail(job.id, &self.claimer, reason, self.limits.attempts_max)?;
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
        if !valid_kind(&job.kind) {
            return Err(RunError::Invalid("unsafe research kind".into()));
        }
        let root = project.root.canonicalize()?;
        if root != project.root || !root.is_dir() || root.starts_with(&self.state) {
            return Err(RunError::Invalid("project folder must be canonical".into()));
        }
        let id = ProjectId::new(project.id).map_err(|_| RunError::Invalid("project id".into()))?;
        let context = jobs.project_context(&id).map_err(RunError::Context)?;
        let (name, worktree) = self.free_run(&root, job)?;
        let agent = self.state.join("agents").join(&name);
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
                Some(self.lease(&host, &fact.binding, &name, u64::from(job.attempts))?)
            }
            ResearchAccount::Endpoint { .. } | ResearchAccount::AnthropicEndpoint { .. } => None,
        };
        let home = match &lease {
            Some(lease) => lease_home(lease)?,
            None => {
                private_directory(&agent)?;
                let home = agent.join("home");
                private_directory(&home)?;
                home
            }
        };
        let harness = account.harness();
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
        let executable = match harness {
            ResearchHarness::Codex => self.codex.as_path(),
            ResearchHarness::ClaudeCode => self
                .claude
                .as_deref()
                .ok_or_else(|| RunError::Invalid("claude executable missing".into()))?,
        };
        let network = if job.tools {
            SandboxNetwork::Open
        } else {
            SandboxNetwork::Only(vec![model_remote(account)?])
        };
        let mut sandbox = ProjectSandbox::new(&root, &home, &tmp, network)?;
        for folder in read_folders(executable, &context, job.tools)? {
            sandbox = sandbox.allow_read(&folder)?;
        }
        let profile = self.state.join("profiles").join(format!("{name}.sb"));
        sandbox.install(&profile)?;
        let setup = self.setup.lock().unwrap_or_else(PoisonError::into_inner);
        ensure_repository(&root, &tmp)?;
        add_worktree(&sandbox, &profile, &worktree)?;
        drop(setup);
        let prompt = research_prompt(&root, &job.kind)?;
        write_private(&agent.join("prompt.md"), prompt.as_bytes())?;
        let mut command = sandbox.command(&profile, executable);
        let (output, stdout_log) = match harness {
            ResearchHarness::Codex => {
                let output = tmp.join("report.md");
                codex_arguments(&mut command, &worktree, &output, &home, job.tools, account)?;
                (output, "events.jsonl")
            }
            ResearchHarness::ClaudeCode => {
                claude_arguments(&mut command, &home, &tmp, job.tools, account)?;
                command.args(installed.arguments());
                (agent.join("output.md"), "output.md")
            }
        };
        command
            .current_dir(&worktree)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        assert!(!carries_secret(&command, &context));
        let mut running = AgentProcess::new(command.spawn()?);
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
            let log = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(agent.join(name))?;
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
                match jobs.heartbeat(job.id, &self.claimer, counted.load(Ordering::Relaxed)) {
                    Ok(true) => break Ending::Cancelled,
                    Err(JobError::NotHeld) => break Ending::Interrupted,
                    Ok(false) | Err(_) => {}
                }
            }
            std::thread::sleep(POLL);
        };
        agent.stop();
        let _ = jobs.heartbeat(job.id, &self.claimer, counted.load(Ordering::Relaxed));
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

/// The agent home a managed launch runs in: the held lease's profile path,
/// which is a configured profile folder when the owner set one and the
/// engine-owned `native-profiles/<key>/home` otherwise. Takes the lease.
/// Returns the canonical home, or an error once the lease no longer verifies.
fn lease_home(lease: &NativeProfileUse) -> Result<PathBuf, RunError> {
    let home = lease
        .profile_path()
        .map_err(|problem| RunError::Account(problem.0))?;
    if !home.is_absolute() || home.canonicalize()? != home {
        return Err(RunError::Invalid("managed home must be canonical".into()));
    }
    Ok(home)
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
            command.args(["-c", "cli_auth_credentials_store=\"file\""]);
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

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Makes the project folder a git repository with one empty start commit and
/// worktrees/ and context/ excluded, once per process at a time.
/// Takes the project folder and a private temporary home for git.
pub(crate) fn ensure_repository(root: &Path, tmp: &Path) -> Result<(), RunError> {
    let _setup = REPOSITORY_SETUP
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if root.join(".git").is_dir() {
        return Ok(());
    }
    let git = |arguments: &[&str]| -> Result<bool, RunError> {
        let mut command = Command::new(GIT);
        command
            .args(["-c", "core.hooksPath=/dev/null"])
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
        wait_bounded(command.spawn()?, SETUP_TIMEOUT)
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
    let mut exclude = OpenOptions::new()
        .append(true)
        .create(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(info.join("exclude"))?;
    exclude.write_all(b"/worktrees/\n/context/\n")?;
    Ok(())
}

/// Adds a detached git worktree at `worktree` inside the sandbox's project
/// folder, running git under the installed profile with hooks disabled.
/// Takes the sandbox, its installed profile and the new worktree path.
pub(crate) fn add_worktree(
    sandbox: &ProjectSandbox,
    profile: &Path,
    worktree: &Path,
) -> Result<(), RunError> {
    let _setup = REPOSITORY_SETUP
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let worktrees = crate::projects_home::state_folder(sandbox.project_root()).join("worktrees");
    if worktree.parent() != Some(worktrees.as_path()) {
        return Err(RunError::Invalid("worktree must sit in worktrees/".into()));
    }
    fs::create_dir_all(&worktrees)?;
    let mut git = sandbox.command(profile, Path::new(GIT));
    git.args([
        "-c",
        "core.hooksPath=/dev/null",
        "worktree",
        "add",
        "-q",
        "--detach",
    ])
    .arg(worktree)
    .arg("HEAD")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    if !wait_bounded(git.spawn()?, SETUP_TIMEOUT)? {
        return Err(RunError::Invalid("git worktree add failed".into()));
    }
    assert!(worktree.starts_with(sandbox.project_root()));
    Ok(())
}

fn wait_bounded(mut child: Child, limit: Duration) -> Result<bool, RunError> {
    let group = child.id();
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if started.elapsed() >= limit {
            stop_group(group);
            let _ = child.wait();
            await_group_exit(group);
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A running agent and the process group it leads. Stopping it, which also
/// happens when it is dropped while a panic unwinds or a launch step fails,
/// kills the whole group, reaps the agent and waits until the group is empty,
/// so no agent process outlives the run that started it.
struct AgentProcess {
    child: Child,
    group: u32,
    stopped: bool,
}

impl AgentProcess {
    /// Takes a just spawned agent that leads its own process group.
    fn new(child: Child) -> Self {
        let group = child.id();
        assert!(group > 1);
        Self {
            child,
            group,
            stopped: false,
        }
    }

    /// Kills the agent's group once, reaps the agent and waits for the group
    /// to empty. Later calls do nothing, so a reused group id is never killed.
    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        stop_group(self.group);
        let _ = self.child.wait();
        await_group_exit(self.group);
        self.stopped = true;
    }
}

impl Drop for AgentProcess {
    /// Stops the agent's process group if the run did not already stop it.
    fn drop(&mut self) {
        self.stop();
    }
}

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

fn drain(mut stream: Box<dyn Read + Send>, counted: &AtomicU64, mut log: File, log_max: u64) {
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

fn private_directory(path: &Path) -> io::Result<()> {
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

fn read_bounded(path: &Path, max: u64) -> io::Result<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelAuthentication, ModelEndpoint, ModelReference};
    use crate::protocol::{ConfigId, CredentialRef, Cursor};
    use std::os::unix::fs::PermissionsExt;

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
            .tempdir_in("/private/tmp")
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
    fn two_research_launches_never_hold_one_managed_profile_at_once() {
        let root = engine_root();
        let runner = ResearchRunner::new(
            &root.database,
            &root.state,
            Path::new("/usr/bin/true"),
            ResearchLimits::default(),
        )
        .unwrap();
        let host = host();
        let connection = ModelConnection {
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
        };
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
        let first = runner.lease(&host, &binding, "job-1-1", 1).unwrap();
        assert_eq!(lease_home(&first).unwrap(), default_home);
        assert!(matches!(
            runner.lease(&host, &binding, "job-2-1", 1),
            Err(RunError::Busy)
        ));
        drop(first);
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
        assert_eq!(most.load(Ordering::SeqCst), 1);
        assert!(granted.load(Ordering::SeqCst) > 0);
        assert_eq!(
            granted.load(Ordering::SeqCst) + refused.load(Ordering::SeqCst),
            120
        );
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
        assert_eq!(lease_home(&lease).unwrap(), home);
        assert_ne!(lease_home(&lease).unwrap(), custody.profile_home(&profile));
        drop(lease);
    }

    fn group_members(group: u32) -> usize {
        let output = Command::new("/usr/bin/pgrep")
            .env_clear()
            .arg("-g")
            .arg(group.to_string())
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).lines().count()
    }

    #[test]
    fn an_agent_dropped_while_a_panic_unwinds_leaves_no_process_in_its_group() {
        use std::os::unix::process::CommandExt;
        const MID_RUN: &str = "the run fails while its agent and the agent's child are running";
        let child = Command::new("/bin/sh")
            .env_clear()
            .args(["-c", "/bin/sleep 120 & /bin/sleep 120"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let group = child.id();
        let pid = rustix::process::Pid::from_raw(i32::try_from(group).unwrap()).unwrap();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let agent = AgentProcess::new(child);
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
}
