use super::accounts::PROFILE_BUSY;
use super::jobs::{JobError, ResearchJob, ResearchJobs, ResearchStatus, valid_kind};
use crate::agents::{Agents, Hold, Holder};
use crate::config::ModelConnection;
use crate::engine::bootstrap::{NativeProfileUse, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::engine::{EngineError, NativeProfileCustody};
use crate::harnesses::launch::{
    AccountBinding, CLAUDE_TOOLS_OFF, LaunchError, LaunchEvent, LaunchLimits, LaunchRequest,
    rate_limited,
};
#[cfg(unix)]
use crate::harnesses::launch::{await_group_exit, kill_group};
use crate::harnesses::leftovers::{self, Unstopped};
use crate::harnesses::terminal_session::{TerminalSession, end_left_pane};
use crate::launch_context::AgentContext;
use crate::project_setup::ResearchKind;
use crate::project_setup::links::LinkedText;
use crate::protocol::{CredentialRef, ProjectId, RequestId};
use crate::sandbox::model_proxy::{ModelHost, ModelProxy};
use crate::sandbox::policy::{AgentAccess, Narrowing, narrowing};
use crate::sandbox::toolchains::{self, ToolHomes, Toolchain, Toolchains};
use crate::sandbox::warden::Warden;
use crate::sandbox::{ProjectSandbox, SandboxError, SandboxNetwork, SandboxRemote};
#[cfg(unix)]
use crate::setup::AGENT_GIT_NEEDS_TOOLS;
use crate::terminal::{AgentId, Host};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::ExitStatus;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The `/usr/bin/git` xcrun shim, the git every resolver here falls back to.
pub(crate) const GIT: &str = "/usr/bin/git";
pub(crate) const GIT_SAFE: [&str; 10] = [
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
const SETUP_TIMED_OUT: &str = "setup_timed_out";
#[cfg(unix)]
const LIFELINE: &str = "IFS= read -r word; [ \"$word\" = done ] || kill -s KILL -- \"-$1\"";
const RUNS_PER_ATTEMPT_MAX: u32 = 100;
const LOG_LINE_BYTES_MAX: usize = 16 * 1024;
const DRAIN_WAIT: Duration = Duration::from_secs(5);
const CODEX_LOG: &str = "error,codex_core::responses_retry=warn";
/// How long [`unreached_provider`] waits for a loopback model provider to take a connection.
const PROVIDER_REACH_TIME: Duration = Duration::from_secs(2);
const LINKS_HEADING: &str = "## Links and references";
const LOCAL_PLACEHOLDER_KEY: &str = "crazyagent-local";
/// The `RunError::Invalid` text of a run whose agent state folder already exists,
/// so a caller can tell this refusal from the others without reading free text.
pub(crate) const RUN_NAME_USED: &str = "run name already used";
const CONTEXT_BUSY: &str = "launch_home_busy";
const PANE_PREFIX: &str = "research-";
/// Gives a tools-off Claude Code run one model turn, as it has no tools to take a second.
/// It does not stop Claude Code 2.1.309 from continuing an answer cut at the output limit:
/// it sends up to three more requests for that answer outside the turn count, so the
/// output token cap and the caller's wall time bound such a run.
const CLAUDE_ONE_TURN: [&str; 2] = ["--max-turns", "1"];
/// The variable that caps the output tokens of each Claude Code request.
const CLAUDE_OUTPUT_TOKENS: &str = "CLAUDE_CODE_MAX_OUTPUT_TOKENS";
const CLAUDE_ARGUMENTS: [&str; 4] = ["--print", "--strict-mcp-config", "--setting-sources", ""];
/// What every Claude Code research run prints: one JSON event per line, from which the
/// engine joins the whole answer the run ends with, where `--print` text keeps only the
/// last part of an answer Claude Code continued past the output cap (DEC-160).
const CLAUDE_EVENTS: [&str; 3] = ["--output-format", "stream-json", "--verbose"];
/// The stdout log of a run that prints one JSON event per line.
const EVENTS_LOG: &str = "events.jsonl";
/// The longest Claude Code event line the engine parses. A longer one, such as a tool
/// result that holds a large file, is told apart by its raw bytes ([`LongLine`]).
const CLAUDE_LINE_BYTES_MAX: usize = 8 * 1024 * 1024;
/// How a Claude Code user event line begins: Claude Code writes its type first.
const CLAUDE_USER_LINE: &[u8] = b"{\"type\":\"user\"";
/// The key that names the sub-agent a Claude Code event belongs to, as raw JSON: inside a
/// string its quotes would be escaped, so in a line it can only be a key.
const CLAUDE_PARENT_KEY: &[u8] = b"\"parent_tool_use_id\":";
/// The model name Claude Code gives the messages it writes itself, such as its API errors.
const CLAUDE_OWN_MODEL: &str = "<synthetic>";
/// The `--settings` every research Claude Code gets; a keyed run adds its `apiKeyHelper`.
const CLAUDE_SETTINGS: &str = "{\"disableAllHooks\":true}";

/// The most bytes of one research report: the runner takes no longer report from an agent,
/// and a reader of a stored report reads up to this many.
pub const REPORT_BYTES_MAX: u64 = 512 * 1024;

/// Bounds every research run obeys. `output_bytes_max` is the most bytes kept in each agent
/// log and read back from it; output past it is counted, never a reason to end the run
/// (owner 11-11).
#[derive(Debug, Clone)]
pub struct ResearchLimits {
    pub wall_time: Duration,
    pub output_bytes_max: u64,
    pub report_bytes_max: u64,
    pub running_max: u32,
    pub attempts_max: u32,
}

impl Default for ResearchLimits {
    /// Twenty minutes, 4 MiB kept of each agent log, a report of [`REPORT_BYTES_MAX`], three
    /// agents per project at once and two attempts per job.
    fn default() -> Self {
        Self {
            wall_time: Duration::from_secs(20 * 60),
            output_bytes_max: 4 * 1024 * 1024,
            report_bytes_max: REPORT_BYTES_MAX,
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
    /// An OpenAI-compatible URL or a local model server for Codex, with the reference of
    /// its saved key when it has one; a keyed run reaches it only through a key relay.
    Endpoint {
        base_url: String,
        model: String,
        key: Option<CredentialRef>,
    },
    /// A managed Codex connection of the project's pool. Each launch proves
    /// it is signed in for this engine run and holds its profile under the
    /// engine's custody lease while the agent runs.
    ManagedCodex {
        connection: ModelConnection,
        model: Option<String>,
    },
    /// An Anthropic Messages URL or local model server for Claude Code, keyed or not
    /// like [`ResearchAccount::Endpoint`].
    AnthropicEndpoint {
        base_url: String,
        model: String,
        key: Option<CredentialRef>,
    },
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

/// A runner failure outside the agent's own outcome. `Uncertain` carries the code of a
/// warden that did not confirm the agent's sandbox empty after the agent stopped.
#[derive(Debug)]
pub enum RunError {
    Invalid(String),
    Jobs(JobError),
    Sandbox(SandboxError),
    Context(LaunchError),
    Account(String),
    Busy,
    Uncertain(&'static str),
    Setup(&'static str),
    Io(io::Error),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "research refused: {reason}"),
            Self::Jobs(error) => write!(f, "{error}"),
            Self::Setup(code) => write!(f, "worktree setup failed: {code}"),
            Self::Sandbox(error) => write!(f, "{error}"),
            Self::Context(error) => write!(f, "research context: {}", error.0),
            Self::Account(code) => write!(f, "research account: {code}"),
            Self::Busy => write!(f, "research account busy"),
            Self::Uncertain(code) => write!(f, "research agent cleanup uncertain: {code}"),
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
/// writes the agent's report to `context/research/<kind>.md`, or a roadmap
/// planning reply to `context/roadmap.md` and `context/release-process.md`.
pub struct ResearchRunner {
    database: PathBuf,
    state: PathBuf,
    codex: Option<PathBuf>,
    claude: Option<PathBuf>,
    lookup: Option<crate::paths::HarnessLookup>,
    limits: ResearchLimits,
    claimer: String,
    stopping: AtomicBool,
    setup: Mutex<()>,
    custody: Mutex<Result<NativeProfileCustody, EngineError>>,
    terminal: Mutex<Option<Arc<dyn Host>>>,
    terminal_probe: Option<Mutex<crate::team::runtime::TerminalProbe>>,
    agents: Option<Arc<Agents>>,
    settings: Option<(PathBuf, PathBuf)>,
    reproving: Option<Arc<AtomicBool>>,
}

/// The words that start a research harness with no search path
/// ([`crate::harnesses::interactive::launch_words`]): its file, or for a script that asks
/// `env` for its interpreter, such as an npm-built Codex asking for `node`, that
/// interpreter's absolute file and then the script. Input: the harness's real file. Output:
/// the words, or the tool resolver's owner line when the interpreter is missing.
fn harness_words(program: &Path) -> Result<Vec<String>, RunError> {
    let words = crate::harnesses::interactive::launch_words(program)
        .map_err(|problem| RunError::Invalid(problem.0))?;
    assert!(
        words
            .first()
            .is_some_and(|word| Path::new(word).is_absolute())
    );
    Ok(words)
}

/// Lets a research harness run in `sandbox`, only once the tool resolver lets it launch
/// ([`crate::harnesses::interactive::launch_tool`]): the real file that starts, the Node
/// folders of an npm install ([`ProjectSandbox::allow_node_for`]) and its interpreters
/// ([`ProjectSandbox::allow_interpreters`]), the same grants a team agent gets. Inputs: the
/// sandbox and the harness's real file. Output: the sandbox, or the resolver's owner line.
fn allow_harness(sandbox: ProjectSandbox, program: &Path) -> Result<ProjectSandbox, RunError> {
    let places = crate::tools::Places::from_environment(None);
    let launched = crate::harnesses::interactive::launch_tool(program, &places)
        .map_err(|problem| RunError::Invalid(problem.0))?;
    let sandbox = sandbox
        .allow_exec(&launched.program)?
        .allow_node_for(&launched.program)?;
    Ok(sandbox.allow_interpreters(&launched)?)
}

/// Ends every research pane an earlier engine left in `host`, with every process its harness
/// started, since that engine's jobs went back to the queue. Takes the host. Returns the
/// host's plain code when it cannot list or end them.
fn end_left_panes(host: &dyn Host) -> Result<(), RunError> {
    let host_error = |problem: crate::terminal::HostError| RunError::Invalid(problem.0);
    for agent in host.alive().map_err(host_error)? {
        if !agent.as_str().starts_with(PANE_PREFIX) {
            continue;
        }
        match end_left_pane(host, &agent, "research") {
            Ok(_) => {}
            Err(problem) if problem.0 == "terminal_agent_unknown" => {}
            Err(problem) => return Err(host_error(problem)),
        }
    }
    Ok(())
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
    Cancelled,
    Interrupted,
    RateLimited,
    Narrowed(Narrowing),
}

/// What a running research job's sandbox profile was built with, checked against the saved
/// policy while it runs: its project and canonical folder, the access level and the granted
/// toolchains.
struct Granted {
    project: ProjectId,
    root: PathBuf,
    access: AgentAccess,
    toolchains: Vec<Toolchain>,
}

impl Granted {
    /// Tells whether the policy the owner saved now no longer covers what the job holds, so
    /// it must stop. Takes the job store. Returns the cause, or none.
    fn narrowed(&self, jobs: &ResearchJobs) -> Option<Narrowing> {
        let saved = jobs.agent_policy(&self.project).ok();
        narrowing(
            saved.as_ref(),
            Some(self.root.as_path()),
            Some(self.access),
            None,
            &self.toolchains,
        )
    }
}

/// The plain line a research job stopped for a narrowed policy shows while it waits to run
/// again. Takes the cause. Returns the line.
fn requeued_reason(cause: Narrowing) -> &'static str {
    match cause {
        Narrowing::Access => {
            "The project's agent access was lowered, so this research was stopped and queued again; its next run uses the new level"
        }
        Narrowing::Network => {
            "The project's agent network was narrowed, so this research was stopped and queued again; its next run uses the new network"
        }
        Narrowing::TurnedOff => {
            "A toolchain this research ran with was turned off, so it was stopped and queued again; its next run goes without it"
        }
        Narrowing::NotAllowed => {
            "A toolchain this research ran with is no longer allowed for the project (turned off, or no longer used by the project), so it was stopped and queued again; its next run goes without it"
        }
        Narrowing::PolicyUnreadable => {
            "The project's agent access could not be read, so this research was stopped and queued again"
        }
        Narrowing::FolderUnreadable => {
            "The project folder could not be read to check this research's toolchains, so it was stopped and queued again"
        }
    }
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
            lookup: None,
            limits,
            claimer: format!("runner-{}-{nanos}", std::process::id()),
            stopping: AtomicBool::new(false),
            setup: Mutex::new(()),
            custody: Mutex::new(custody),
            terminal: Mutex::new(None),
            terminal_probe: None,
            agents: None,
            settings: None,
            reproving: None,
        })
    }

    /// Reads the owner's model settings from these two files at each tools-on launch, so
    /// the agent's sandbox keeps the ports of the configured local model endpoints free of
    /// its listeners. Takes the global and project settings files. Returns the runner.
    pub fn with_settings(mut self, global: &Path, project: &Path) -> Self {
        self.settings = Some((global.to_path_buf(), project.to_path_buf()));
        self
    }

    /// Runs every research agent in its harness's interactive app in a pane of `host` named
    /// `research-<run>`, where the owner can watch it work. Panes an earlier engine left end
    /// here with every process their harness started, since that engine's jobs go back to
    /// the queue; a cleanup not confirmed is reported on stderr. Takes the host.
    /// Returns the runner, or the host's plain code.
    pub fn with_terminal(mut self, host: Arc<dyn Host>) -> Result<Self, RunError> {
        end_left_panes(host.as_ref())?;
        *self
            .terminal
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) = Some(host);
        Ok(self)
    }

    /// Lets a runner that started with no terminal host look for one again at each research
    /// agent's start, so tmux installed after the engine started runs the next agent in a
    /// pane with no engine restart. Takes the probe. Returns the runner.
    pub fn with_terminal_probe(mut self, probe: crate::team::runtime::TerminalProbe) -> Self {
        self.terminal_probe = Some(Mutex::new(probe));
        self
    }

    /// The terminal host a research agent starts in now: the one the runner holds, else the
    /// one its probe finds, kept once its earlier panes end ([`end_left_panes`]). Output:
    /// the host, or none while no terminal program is found, so the agent runs headless.
    fn terminal(&self) -> Option<Arc<dyn Host>> {
        let mut held = self.terminal.lock().unwrap_or_else(PoisonError::into_inner);
        if held.is_none() {
            let probe = self.terminal_probe.as_ref()?;
            let host = (probe.lock().unwrap_or_else(PoisonError::into_inner))()?;
            match end_left_panes(host.as_ref()) {
                Ok(()) => *held = Some(host),
                Err(problem) => {
                    eprintln!(
                        "research terminal found but not used ({problem}); agents run headless"
                    );
                }
            }
        }
        held.clone()
    }

    /// Names each running research job in the engine's agent register as a holder of the
    /// account it signs in with, so a refused sign-out can name and cancel it. Takes the
    /// register. Returns the runner.
    pub fn with_agents(mut self, agents: Arc<Agents>) -> Self {
        self.agents = Some(agents);
        self
    }

    /// Shares the engine's flag that is set while its engine-start re-proof of earlier
    /// sign-ins still runs (DEC-108), so work that finds no signed-in account then waits
    /// for the re-proof instead of reporting none. Takes the flag. Returns the runner.
    pub fn with_reproof(mut self, reproving: Arc<AtomicBool>) -> Self {
        self.reproving = Some(reproving);
        self
    }

    /// Whether the engine-start re-proof of earlier sign-ins still runs; false without the
    /// engine's flag. Output: the flag's value.
    pub fn reproving(&self) -> bool {
        self.reproving
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
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

    /// Lets the runner find a harness the engine pinned after it started, as after an
    /// install from the app. Only engine start sets it, so a runner built without it never
    /// looks past the executables it was given. Takes the engine's lookup. Returns the runner.
    pub fn with_harness_lookup(mut self, lookup: crate::paths::HarnessLookup) -> Self {
        self.lookup = Some(lookup);
        self
    }

    /// The entry of a harness: the one given at engine start, else the one the engine pinned
    /// since, when the lookup is set. Takes the harness. Returns the entry, or none for a
    /// harness that is not installed.
    fn harness(&self, harness: ResearchHarness) -> Option<PathBuf> {
        let given = match harness {
            ResearchHarness::Codex => &self.codex,
            ResearchHarness::ClaudeCode => &self.claude,
        };
        let entry = given
            .clone()
            .or_else(|| self.lookup.as_ref().and_then(|lookup| lookup(harness.id())))?;
        assert!(entry.is_absolute());
        Some(entry)
    }

    /// Whether this runner has the executable `harness` needs.
    pub fn can_run(&self, harness: ResearchHarness) -> bool {
        self.harness(harness).is_some()
    }

    /// The job store this runner claims from.
    pub fn database(&self) -> &Path {
        &self.database
    }

    /// The attempt limit of this runner's jobs, which stale recovery obeys too.
    pub fn attempts_max(&self) -> u32 {
        self.limits.attempts_max
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
        self.run_claimed(&jobs, job, project, None).map(Some)
    }

    /// Claims every job of `project` for the account's harness that the
    /// per-project limit allows right now, at most `running_max` in one call
    /// even while runs hand busy jobs back, and runs each on its own thread. `account` is
    /// the picked pool account's connection and account; a run refused for a rate or usage
    /// limit pauses that connection and hands its job back without spending an attempt.
    pub fn spawn_ready(
        self: &Arc<Self>,
        project: String,
        harness: &str,
        root: PathBuf,
        account: Option<(CredentialRef, ResearchAccount)>,
    ) -> Vec<JoinHandle<Result<ResearchRun, RunError>>> {
        let (member, account) = account.map_or((None, None), |(member, account)| {
            (Some(member), Some(account))
        });
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
            let member = member.clone();
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
                    member.as_ref(),
                )
            }));
        }
        assert!(handles.len() <= self.limits.running_max as usize);
        handles
    }

    /// Runs a claimed job to its end and records how it ended: done with its report,
    /// failed, cancelled, parked for a pool, or handed back. Takes the job store, the job,
    /// its project and the pool connection of the project's account, if known; a run
    /// refused for a rate or usage limit pauses it, else spends an attempt. A job whose
    /// project folder a projects home move holds goes back to the queue unspent.
    fn run_claimed(
        &self,
        jobs: &ResearchJobs,
        job: ResearchJob,
        project: &ResearchProject<'_>,
        member: Option<&CredentialRef>,
    ) -> Result<ResearchRun, RunError> {
        assert_eq!(job.status, ResearchStatus::Running);
        assert_eq!(job.project, project.id);
        let run = |status, report, process_group| ResearchRun {
            job: job.id,
            status,
            report,
            process_group,
        };
        if jobs.home_moving(project.root)? {
            jobs.release(&job, None)?;
            let status = jobs
                .get(job.id)?
                .map_or(ResearchStatus::Queued, |job| job.status);
            return Ok(run(status, None, None));
        }
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
        if self.harness(harness).is_none() {
            let status = jobs.fail(&job, "This harness is not installed on this computer", 1)?;
            return Ok(run(status, None, None));
        }
        if let Some(line) = confined_unavailable(std::env::consts::OS) {
            let status = jobs.fail(&job, line, 1)?;
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
                let relative = ResearchKind::parse(&job.kind).map_or_else(
                    || format!("context/research/{}.md", job.kind),
                    ResearchKind::report_path,
                );
                jobs.finish(&job, &relative)?;
                Ok(run(ResearchStatus::Done, Some(report), Some(group)))
            }
            Ok((ending, group, _)) => {
                if let Ending::Cancelled = ending {
                    jobs.cancelled(&job)?;
                    return Ok(run(ResearchStatus::Cancelled, None, Some(group)));
                }
                if let Ending::Interrupted | Ending::Narrowed(_) = ending {
                    let reason = match ending {
                        Ending::Narrowed(cause) => Some(requeued_reason(cause)),
                        _ => None,
                    };
                    let _ = jobs.release(&job, reason);
                    let status = jobs
                        .get(job.id)?
                        .map_or(ResearchStatus::Queued, |job| job.status);
                    return Ok(run(status, None, Some(group)));
                }
                if let (Ending::RateLimited, Some(member)) = (&ending, member) {
                    let (status, _) = jobs.rate_limited(&job, member)?;
                    return Ok(run(status, None, Some(group)));
                }
                let reason = match ending {
                    Ending::TimedOut => "Agent ran out of time",
                    Ending::RateLimited => "The account hit its rate limit",
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
                jobs.release(&job, None)?;
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
                let (reason, attempts_max) = match &error {
                    RunError::Context(problem) => (
                        crate::launch_context::secret_in_command_text(problem).unwrap_or_else(
                            || "Could not give the agent this project's context".to_owned(),
                        ),
                        self.limits.attempts_max,
                    ),
                    RunError::Uncertain(_) => {
                        (crate::sandbox::warden::CLEANUP_UNCERTAIN.to_owned(), 1)
                    }
                    _ => (
                        "Could not prepare the research sandbox".to_owned(),
                        self.limits.attempts_max,
                    ),
                };
                jobs.fail(&job, &reason, attempts_max)?;
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
        let (account, relay) = relayed(self.engine_folder()?, account)?;
        self.launch_leased(jobs, job, &account, place, lease, relay.as_ref())
    }

    /// The engine folder, which holds `engine.db` and the saved account keys.
    /// Output: the folder, or a refusal for a database path with no parent.
    fn engine_folder(&self) -> Result<&Path, RunError> {
        self.database
            .parent()
            .ok_or_else(|| RunError::Invalid("engine folder".into()))
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

    /// Registers an agent's home with the usage store, so its tokens count against the
    /// project and account; a run on an endpoint account counts under no account, and a
    /// store that cannot be written leaves the run uncounted while it still starts.
    /// Inputs: the home, the run kind, the project, the account and the agent key.
    fn register_usage(
        &self,
        home: &Path,
        kind: crate::usage::capture::RunKind,
        project: Option<&ProjectId>,
        account: &ResearchAccount,
        agent: &str,
    ) {
        let attribution = crate::usage::capture::Attribution {
            kind,
            project: project.cloned(),
            pool: None,
            account: match account {
                ResearchAccount::ManagedCodex { connection, .. }
                | ResearchAccount::ManagedClaude { connection, .. } => Some(connection.id.clone()),
                ResearchAccount::Endpoint { .. } | ResearchAccount::AnthropicEndpoint { .. } => {
                    None
                }
            },
            agent: agent.to_owned(),
            harness: account.harness().id().to_owned(),
        };
        let registered = self
            .database
            .parent()
            .ok_or_else(|| crate::engine::EngineError("usage_unavailable".into()))
            .and_then(crate::usage::open)
            .and_then(|db| {
                crate::usage::capture::register_run(&db, home, &attribution, crate::team::now_ms())
            });
        if let Err(problem) = registered {
            eprintln!("research usage not counted: {}", problem.0);
        }
    }

    /// Runs a claimed job's agent once its account is taken: the managed credential copy
    /// in the agent's home, the project context, the sandboxed worktree and the harness.
    /// Inputs: the job store, the job, its account, where it runs, the held lease, none for
    /// an endpoint account, and the key relay a keyed endpoint account runs behind, which
    /// outlives this call. Output: how the agent ended, its process group and output.
    fn launch_leased(
        &self,
        jobs: &ResearchJobs,
        job: &ResearchJob,
        account: &ResearchAccount,
        place: JobPlace,
        lease: Option<(NativeProfileUse, Option<Hold>)>,
        relay: Option<&crate::model_gateway::relay::Relay>,
    ) -> Result<(Ending, u32, PathBuf), RunError> {
        let JobPlace {
            root,
            project,
            context,
            name,
            worktree,
            agent,
        } = place;
        private_directory(&agent)?;
        let home = agent.join("home");
        private_directory(&home)?;
        self.register_usage(
            &home,
            crate::usage::capture::RunKind::Research,
            Some(&project),
            account,
            &format!("research-{name}"),
        );
        let harness = account.harness();
        let executable = &self.harness(harness).ok_or_else(|| {
            RunError::Invalid(match harness {
                ResearchHarness::Codex => "codex executable missing".into(),
                ResearchHarness::ClaudeCode => "claude executable missing".into(),
            })
        })?;
        let credential = match &lease {
            Some((lease, _)) => Some(
                lease
                    .hand_to_agent(&home, self.limits.wall_time, executable)
                    .map_err(|problem| RunError::Account(problem.0))?,
            ),
            None => {
                endpoint_keychain(&self.state, harness, &home)?;
                None
            }
        };
        let policy = jobs
            .agent_policy(&project)
            .map_err(|_| RunError::Invalid("project agent access unreadable".into()))?;
        if let Some(reason) = policy.access.unavailable() {
            return Err(RunError::Invalid(reason.into()));
        }
        let kept = job_kept_ports(self.settings.as_ref(), job.tools)?;
        let (network, proxy) = job_reach(&policy, job.tools, account, job.id, &kept)?;
        let display = jobs
            .terminal_display(&project)
            .map_err(|_| RunError::Invalid("project coordination settings unreadable".into()))?;
        let context = context.with_terminal_display(display);
        let context = match &proxy {
            Some(proxy) => context.with_model_proxy(proxy.port()),
            None => context,
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
        let unconfined = policy.access == AgentAccess::Full;
        let homes = ToolHomes::owner()?;
        let scratch = toolchains::ScopedTmp::make(&home)?;
        #[cfg(target_os = "macos")]
        toolchains::write_getconf(&home, false)?;
        toolchains::prepare_cargo_home(&home, &homes)?;
        let mut sandbox = ProjectSandbox::new(&root, &home, &tmp, network)?.keep_ports(&kept)?;
        if let Some(runtime) = self.state.parent() {
            sandbox = sandbox.guard_engine(runtime)?;
        }
        let extra = jobs
            .read_paths(&project)
            .map_err(|_| RunError::Invalid("project read-only folders unreadable".into()))?;
        let granted = Granted {
            project: project.clone(),
            root: root.clone(),
            access: policy.access,
            toolchains: policy.allowed(&root),
        };
        (sandbox, _) = sandbox
            .allow_tools(&Toolchains::grant(&homes, &granted.toolchains))?
            .allow_scratch(scratch.path())?
            .allow_extra_reads(&extra)?;
        let program =
            crate::paths::launch_executable(executable, std::env::var_os("PATH").as_deref())?;
        for folder in read_folders(&program, &context, job.tools)? {
            sandbox = sandbox.allow_read(&folder)?;
        }
        sandbox = allow_harness(sandbox, &program)?;
        let profile = self.state.join("profiles").join(format!("{name}.sb"));
        sandbox.install(&profile)?;
        let sandbox = self.keep_held(job, || -> Result<ProjectSandbox, RunError> {
            let _setup = self.setup.lock().unwrap_or_else(PoisonError::into_inner);
            ensure_repository(&root, &tmp)?;
            add_worktree(&sandbox, &profile, &worktree)?;
            let mut sandbox = sandbox.confine_to_worktree(&worktree)?;
            let hidden = jobs
                .hidden_paths(&project)
                .map_err(|_| RunError::Invalid("project hidden folders unreadable".into()))?;
            if let Err(code) = crate::sandbox::hidden_paths::none_tracked(&root, &hidden) {
                return Err(RunError::Invalid(code.into()));
            }
            for folder in &hidden {
                sandbox = sandbox.hide(folder)?;
            }
            if !unconfined {
                sandbox =
                    crate::sandbox::shared_folders::apply(sandbox, &root, &policy.shared_folders)
                        .map_err(|code| RunError::Invalid(code.into()))?;
            }
            sandbox.install(&profile)?;
            Ok(sandbox)
        })??;
        still_held(jobs, job)?;
        let linked = self.linked_research(job, &root)?;
        let prompt = research_prompt(&root, &job.kind, &linked)?;
        write_private(&agent, &agent.join("prompt.md"), prompt.as_bytes())?;
        if let Some(host) = self.terminal() {
            let output = match harness {
                ResearchHarness::Codex => tmp.join("report.md"),
                ResearchHarness::ClaudeCode => agent.join("output.md"),
            };
            let (binding, model) = pane_account(account, &home, relay)?;
            let request = LaunchRequest {
                harness: harness.id().to_owned(),
                executable: program.clone(),
                working_dir: worktree.clone(),
                account: binding,
                model,
                tools: job.tools,
                sandbox_profile: (!unconfined).then(|| profile.clone()),
                unconfined,
                git_identity: None,
                limits: LaunchLimits {
                    run_time: self.limits.wall_time,
                    ..LaunchLimits::default()
                },
            };
            let pane = AgentId::new(&format!("{PANE_PREFIX}{name}"))
                .map_err(|problem| RunError::Invalid(problem.0))?;
            let mut session =
                TerminalSession::start_installed(host.clone(), pane, request, installed, &context)
                    .map_err(|problem| {
                        if problem.1.is_some() {
                            RunError::Context(problem)
                        } else {
                            RunError::Invalid(format!("agent terminal: {}", problem.0))
                        }
                    })?;
            let group = session.process_group();
            #[cfg(unix)]
            let mut lifeline = Lifeline::watch(group)?;
            let ending =
                self.supervise_pane(jobs, job, &granted, &mut session, &prompt, harness, &output);
            let stopped = session.stop();
            #[cfg(unix)]
            lifeline.release();
            match stopped {
                Ok(_) => {}
                Err(Unstopped::Uncertain) => {
                    if let Some(credential) = credential {
                        credential.retain();
                    }
                    std::mem::forget(lease);
                    return Err(RunError::Uncertain(leftovers::UNCERTAIN));
                }
                Err(Unstopped::Left(left)) => {
                    let code = left.code();
                    drop(credential);
                    let account = lease.as_ref().map(|(profile, _)| profile.account().clone());
                    let by = leftovers::HoldBy {
                        project: Some(job.project.clone()),
                        who: format!("research job {}", job.id),
                    };
                    let mode = jobs.leftover_mode(&job.project);
                    left.hold(account, lease, by, mode);
                    return Err(RunError::Uncertain(code));
                }
            }
            drop(credential);
            drop(lease);
            return Ok((ending?, group, output));
        }
        if let Some(relay) = relay {
            write_relay_token(&home, relay)?;
        }
        let words = harness_words(&program)?;
        let mut launch = Command::new(&words[0]);
        launch.args(&words[1..]);
        let output = match harness {
            ResearchHarness::Codex => {
                let output = tmp.join("report.md");
                codex_arguments(&mut launch, &worktree, &output, &home, job.tools, account)?;
                output
            }
            ResearchHarness::ClaudeCode => {
                claude_arguments(&mut launch, &home, &tmp, job.tools, None, account)?;
                launch.args(installed.arguments());
                agent.join("output.md")
            }
        };
        launch.current_dir(&worktree);
        if let Some(port) = installed.model_proxy() {
            launch.envs(crate::sandbox::model_proxy::variables(port));
        }
        let mut command = if unconfined {
            sandbox.wrap_unconfined(&launch)?
        } else {
            sandbox.wrap(&profile, &launch)?
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        context
            .refuse_exposed(
                std::iter::once(command.get_program().as_encoded_bytes())
                    .chain(
                        command
                            .get_args()
                            .map(|argument| argument.as_encoded_bytes()),
                    )
                    .chain(command.get_envs().flat_map(|(name, value)| {
                        std::iter::once(name.as_encoded_bytes())
                            .chain(value.map(|value| value.as_encoded_bytes()))
                    })),
            )
            .map_err(RunError::Context)?;
        let mut running = AgentProcess::spawn(&mut command)?;
        let group = running.group;
        let counted = Arc::new(AtomicU64::new(0));
        let limited = Arc::new(AtomicBool::new(false));
        let mut drains = Vec::new();
        let follow = (harness == ResearchHarness::ClaudeCode)
            .then(|| ClaudeStream::new(report_bound(self.limits.report_bytes_max)));
        for (stream, name, watch, follow) in [
            (
                running
                    .child
                    .stdout
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Read + Send>),
                EVENTS_LOG,
                None,
                follow,
            ),
            (
                running
                    .child
                    .stderr
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Read + Send>),
                "stderr.log",
                (harness == ResearchHarness::Codex).then(|| limited.clone()),
                None,
            ),
        ] {
            let Some(stream) = stream else { continue };
            let log = create_private(&agent, &agent.join(name))?;
            let counted = counted.clone();
            let log_max = self.limits.output_bytes_max;
            drains.push(std::thread::spawn(move || {
                drain_watching(stream, &counted, log, log_max, watch.as_deref(), follow)
            }));
        }
        if let Some(mut stdin) = running.child.stdin.take() {
            let _ = stdin.write_all(prompt.as_bytes());
        }
        let ending = self.supervise(jobs, job, &granted, &mut running, &counted, &limited)?;
        let stopped = running.stop();
        let _ = jobs.heartbeat(job, counted.load(Ordering::Relaxed));
        drop(running);
        let draining = Instant::now();
        while drains.iter().any(|drain| !drain.is_finished()) && draining.elapsed() < DRAIN_WAIT {
            std::thread::sleep(POLL);
        }
        let followed = drains
            .into_iter()
            .filter(JoinHandle::is_finished)
            .find_map(|drain| drain.join().ok().flatten());
        assert!(followed.is_none() || harness == ResearchHarness::ClaudeCode);
        let ending = if matches!(ending, Ending::Exited(false))
            && refused_for_limit(
                harness,
                &agent,
                followed.as_ref(),
                self.limits.output_bytes_max,
            ) {
            Ending::RateLimited
        } else {
            ending
        };
        drop(installed);
        if let Err(code) = stopped {
            if let Some(credential) = credential {
                credential.retain();
            }
            std::mem::forget(lease);
            return Err(RunError::Uncertain(code));
        }
        drop(credential);
        drop(lease);
        if let Some(answer) = followed.as_ref().and_then(ClaudeStream::answer) {
            assert!(answer.len() as u64 <= self.limits.report_bytes_max);
            write_private(&agent, &output, answer.as_bytes())?;
        }
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

    /// Follows a direct agent process until it exits, its stderr shows it waiting out a
    /// rate or usage limit, the job is cancelled, the saved policy no longer covers what it
    /// was granted, the runner stops or a limit passes; the caller then stops it. Takes the
    /// job store, the job, its grants, the process, its output count and the limit flag its
    /// stderr drain sets. Returns how the run ended.
    fn supervise(
        &self,
        jobs: &ResearchJobs,
        job: &ResearchJob,
        granted: &Granted,
        agent: &mut AgentProcess,
        counted: &AtomicU64,
        limited: &AtomicBool,
    ) -> Result<Ending, RunError> {
        let started = Instant::now();
        let mut beat = Instant::now();
        let ending = loop {
            if limited.load(Ordering::SeqCst) {
                break Ending::RateLimited;
            }
            if let Some(status) = agent.child.try_wait()? {
                break Ending::Exited(status.success());
            }
            if started.elapsed() >= self.limits.wall_time {
                break Ending::TimedOut;
            }
            if self.is_stopping() {
                break Ending::Interrupted;
            }
            if beat.elapsed() >= HEARTBEAT {
                beat = Instant::now();
                match jobs.heartbeat(job, counted.load(Ordering::Relaxed)) {
                    Ok(true) => break Ending::Cancelled,
                    Err(JobError::NotHeld) => break Ending::Interrupted,
                    Ok(false) | Err(_) => {}
                }
                if let Some(cause) = granted.narrowed(jobs) {
                    break Ending::Narrowed(cause);
                }
            }
            std::thread::sleep(POLL);
        };
        Ok(ending)
    }

    /// Types the research prompt into the agent's pane and follows its session record until
    /// the turn ends or reports a rate or usage limit, the job is cancelled, the saved policy
    /// no longer covers what it was granted, the runner stops or a limit passes. The agent's
    /// [`Reply`] becomes the report at `output`. Returns how the run ended.
    #[allow(clippy::too_many_arguments)]
    fn supervise_pane(
        &self,
        jobs: &ResearchJobs,
        job: &ResearchJob,
        granted: &Granted,
        session: &mut TerminalSession,
        prompt: &str,
        harness: ResearchHarness,
        output: &Path,
    ) -> Result<Ending, RunError> {
        let mut reply = Reply::new(harness, report_bound(self.limits.report_bytes_max));
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
            if beat.elapsed() >= HEARTBEAT {
                beat = Instant::now();
                match jobs.heartbeat(job, counted) {
                    Ok(true) => break Ending::Cancelled,
                    Err(JobError::NotHeld) => break Ending::Interrupted,
                    Ok(false) | Err(_) => {}
                }
                if let Some(cause) = granted.narrowed(jobs) {
                    break Ending::Narrowed(cause);
                }
            }
            match session.next_event(POLL) {
                Ok(Some(LaunchEvent::Text { text })) => {
                    counted += text.len() as u64;
                    reply.said(&text);
                }
                Ok(Some(LaunchEvent::ToolUse { .. })) => reply.tool(),
                Ok(Some(LaunchEvent::TurnFinished { success })) => break Ending::Exited(success),
                Ok(Some(LaunchEvent::TurnError { rate_limited: true })) => {
                    break Ending::RateLimited;
                }
                Ok(Some(LaunchEvent::Exited { .. })) => break Ending::Exited(false),
                Ok(_) => {}
                Err(problem) => {
                    break match problem.0.as_str() {
                        "launch_deadline" => Ending::TimedOut,
                        _ => Ending::Exited(false),
                    };
                }
            }
        };
        let _ = jobs.heartbeat(job, counted);
        if matches!(ending, Ending::Exited(true))
            && let Some(text) = reply.text()
        {
            assert!(text.len() as u64 <= counted);
            write_private(&self.state.join("agents"), output, text.as_bytes())?;
        }
        Ok(ending)
    }

    /// Runs `work`, which can wait long on the setup locks and the bounded git steps, while
    /// a second connection refreshes the claim `job` every `HEARTBEAT`, so the wait is not
    /// taken back as stale; the refresh ends with `work`, also when it panics. Inputs: the
    /// claimed job and the work. Output: the work's result, or the store's open error.
    fn keep_held<T>(&self, job: &ResearchJob, work: impl FnOnce() -> T) -> Result<T, RunError> {
        let keeper = ResearchJobs::open(&self.database)?;
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        Ok(std::thread::scope(|scope| {
            scope.spawn(move || {
                while matches!(
                    stopped.recv_timeout(HEARTBEAT),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ) {
                    let _ = keeper.heartbeat(job, 0);
                }
            });
            let result = work();
            drop(stop);
            result
        }))
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

    /// The research files linked from the project's repository, for the planning job only:
    /// no other job's prompt carries them, each read through the links store with no link
    /// followed. Inputs: the job and the project folder. Output: each file's research kind,
    /// path and text, or the refusal when the links cannot be read.
    fn linked_research(
        &self,
        job: &ResearchJob,
        root: &Path,
    ) -> Result<Vec<(ResearchKind, LinkedText)>, RunError> {
        if job.kind != ResearchKind::RoadmapPlanning.name() {
            return Ok(Vec::new());
        }
        let unreadable = || RunError::Invalid(LINKS_UNREADABLE.into());
        let runtime = self.database.parent().ok_or_else(unreadable)?;
        let project = ProjectId::new(job.project.clone()).map_err(|_| unreadable())?;
        let db = crate::questions::open(runtime).map_err(|_| unreadable())?;
        let linked =
            crate::project_setup::links::research(&db, &project, root).map_err(|_| unreadable())?;
        assert!(linked.len() <= crate::project_setup::links::LINKS_MAX);
        assert!(
            linked
                .iter()
                .all(|(_, file)| file.text.len() as u64
                    <= crate::project_setup::links::LINK_BYTES_MAX)
        );
        Ok(linked)
    }

    /// Publishes a finished job's output: the planning job's reply as the roadmap documents
    /// with its open points raised, any other job's report under the project's research
    /// folder with the owner questions its agent returned taken out of it and raised.
    /// Inputs: the job, its harness, the project folder and the agent's output file.
    /// Output: the written file, or the plain failure the job ends with.
    fn publish(
        &self,
        job: &ResearchJob,
        harness: ResearchHarness,
        root: &Path,
        output: &Path,
    ) -> Result<PathBuf, &'static str> {
        let body = read_bounded(
            &self.state.join("agents"),
            output,
            self.limits.report_bytes_max,
        )
        .map_err(|_| "Agent returned no report")?;
        let body = body.trim();
        if body.is_empty() {
            return Err("Agent returned no report");
        }
        if job.kind == ResearchKind::RoadmapPlanning.name() {
            let state = crate::projects_home::state_folder(root);
            let plan = crate::planning::publish(&state, body, harness.label())?;
            crate::planning::raise(self.database(), &job.project, &plan)
                .map_err(|_| crate::planning::QUESTIONS_UNLOGGED)?;
            return Ok(state.join(crate::planning::ROADMAP));
        }
        let label = kind_label(&job.kind);
        let path = ResearchKind::parse(&job.kind).map_or_else(
            || format!("context/research/{}.md", job.kind),
            ResearchKind::report_path,
        );
        let (body, asks) = report_questions(body, &label, &path);
        if body.is_empty() {
            return Err("Agent returned no report");
        }
        let report = render_report(&label, harness.label(), &body);
        let written = write_report(root, &job.kind, &report)
            .map_err(|_| "Could not save the research report")?;
        let source = crate::questions::EngineSource::Research(job.kind.clone());
        crate::planning::raise_asks(self.database(), &job.project, &source, &asks)
            .map_err(|_| REPORT_QUESTIONS_UNLOGGED)?;
        Ok(written)
    }
}

/// One agent run outside the research queue that may only read `folder`,
/// such as onboarding's mapping agent: `name` names its agent state and
/// sandbox profile, `agent` and `project` name whose usage its tokens count
/// as, `prompt` goes to it on stdin, `wall_time` bounds it and
/// `output_tokens` caps each Claude Code request (`None` for Claude Code's own
/// limit; a Codex run has no such cap).
pub(crate) struct ReadOnlyRun<'a> {
    pub name: &'a str,
    #[cfg(unix)]
    pub agent: &'a str,
    #[cfg(unix)]
    pub project: Option<&'a ProjectId>,
    #[cfg(unix)]
    pub folder: &'a Path,
    #[cfg(unix)]
    pub prompt: &'a str,
    pub account: &'a ResearchAccount,
    pub wall_time: Duration,
    #[cfg(unix)]
    pub output_tokens: Option<u32>,
}

/// How a read-only agent run ended: its answer, a plain failure, out
/// of time, or stopped by its caller or the runner's shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadOnlyEnding {
    #[cfg(unix)]
    Answered(String),
    Failed(String),
    #[cfg(unix)]
    TimedOut,
    #[cfg(unix)]
    Stopped,
}

impl ResearchRunner {
    /// Runs one agent with tools off that can read `run.folder`, its secret files
    /// closed but the ones the owner shared back (DEC-147.9), but write only
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
        if let Some(line) = confined_unavailable(std::env::consts::OS) {
            return Ok(ReadOnlyEnding::Failed(line.into()));
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
        self.run_read_only_leased(run, lease, stop)
    }

    /// Runs [`Self::run_read_only`]'s agent on a custody lease already taken for a managed
    /// account, `None` for an endpoint account. Takes the run, the lease and `stop`.
    /// Returns how it ended, with the same checks and cleanup as [`Self::run_read_only`].
    #[cfg(unix)]
    pub(crate) fn run_read_only_leased(
        &self,
        run: &ReadOnlyRun<'_>,
        lease: Option<NativeProfileUse>,
        stop: &dyn Fn() -> bool,
    ) -> Result<ReadOnlyEnding, RunError> {
        if !valid_run_name(run.name) || run.wall_time.is_zero() {
            return Err(RunError::Invalid("read-only run name or limit".into()));
        }
        let managed = matches!(
            run.account,
            ResearchAccount::ManagedCodex { .. } | ResearchAccount::ManagedClaude { .. }
        );
        if managed != lease.is_some() {
            return Err(RunError::Invalid(
                "read-only run lease does not fit its account".into(),
            ));
        }
        let (behind, relay) = relayed(self.engine_folder()?, run.account)?;
        let account = &behind;
        let folder = run.folder.canonicalize()?;
        if folder != run.folder || !folder.is_dir() || folder.starts_with(&self.state) {
            return Err(RunError::Invalid(
                "read-only folder must be canonical".into(),
            ));
        }
        let agent = self.state.join("agents").join(run.name);
        if fs::symlink_metadata(&agent).is_ok() {
            return Err(RunError::Invalid(RUN_NAME_USED.into()));
        }
        private_directory(&agent)?;
        let home = agent.join("home");
        private_directory(&home)?;
        self.register_usage(
            &home,
            crate::usage::capture::RunKind::Research,
            run.project,
            account,
            run.agent,
        );
        let harness = account.harness();
        let executable = &self.harness(harness).ok_or_else(|| {
            RunError::Invalid(match harness {
                ResearchHarness::Codex => "codex executable missing".into(),
                ResearchHarness::ClaudeCode => "claude executable missing".into(),
            })
        })?;
        let credential = match &lease {
            Some(lease) => Some(
                lease
                    .hand_to_agent(&home, run.wall_time, executable)
                    .map_err(|problem| RunError::Account(problem.0))?,
            ),
            None => {
                endpoint_keychain(&self.state, harness, &home)?;
                None
            }
        };
        let work = agent.join("work");
        private_directory(&work)?;
        let tmp = agent.join("tmp");
        private_directory(&tmp)?;
        let mut sandbox = ProjectSandbox::new(
            &work,
            &home,
            &tmp,
            SandboxNetwork::Only(vec![model_remote(account)?]),
        )?
        .allow_read(&folder)?;
        let program =
            crate::paths::launch_executable(executable, std::env::var_os("PATH").as_deref())?;
        if let Some(parent) = program.parent() {
            sandbox = sandbox.allow_read(parent)?;
        }
        sandbox = allow_harness(sandbox, &program)?;
        let hidden = match run.project {
            Some(project) => ResearchJobs::open(&self.database)?
                .hidden_paths(project)
                .map_err(|_| RunError::Invalid("project hidden folders unreadable".into()))?,
            None => Vec::new(),
        };
        if let Err(code) = crate::sandbox::hidden_paths::none_tracked(&folder, &hidden) {
            return Err(RunError::Invalid(code.into()));
        }
        for sub in hidden.iter().filter(|path| path.starts_with(&folder)) {
            sandbox = sandbox.hide(sub)?;
        }
        let shared = match run.project {
            Some(project) => {
                ResearchJobs::open(&self.database)?
                    .agent_policy(project)
                    .map_err(|_| RunError::Invalid("project agent policy unreadable".into()))?
                    .shared_folders
            }
            None => crate::sandbox::shared_folders::SharedFolders::default(),
        };
        let read_only = crate::sandbox::shared_folders::SharedFolders {
            access: crate::sandbox::shared_folders::SharedAccess::ReadOnly,
            ..shared
        };
        sandbox = crate::sandbox::shared_folders::apply(sandbox, &folder, &read_only)
            .map_err(|code| RunError::Invalid(code.into()))?;
        let profile = self.state.join("profiles").join(format!("{}.sb", run.name));
        sandbox.install(&profile)?;
        write_private(&agent, &agent.join("prompt.md"), run.prompt.as_bytes())?;
        let words = harness_words(&program)?;
        let mut command = sandbox.command(&profile, Path::new(&words[0]));
        command.args(&words[1..]);
        let (output, stdout_log) = match harness {
            ResearchHarness::Codex => {
                let output = tmp.join("answer.md");
                codex_arguments(&mut command, &folder, &output, &home, false, account)?;
                (output, "events.jsonl")
            }
            ResearchHarness::ClaudeCode => {
                claude_arguments(&mut command, &home, &tmp, false, run.output_tokens, account)?;
                (agent.join(EVENTS_LOG), EVENTS_LOG)
            }
        };
        command
            .current_dir(&folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(relay) = &relay {
            write_relay_token(&home, relay)?;
        }
        let mut running = AgentProcess::spawn(&mut command)?;
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
            let log = create_private(&agent, &agent.join(name))?;
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
            std::thread::sleep(POLL);
        };
        let stopped = running.stop();
        drop(running);
        for handle in drains {
            let _ = handle.join();
        }
        if let Err(code) = stopped {
            if let Some(credential) = credential {
                credential.retain();
            }
            std::mem::forget(lease);
            return Err(RunError::Uncertain(code));
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
        let answer = read_bounded(&agent, &output, self.limits.report_bytes_max)
            .ok()
            .and_then(|text| match harness {
                ResearchHarness::Codex => Some(text),
                ResearchHarness::ClaudeCode => claude_answer(&text),
            })
            .map(|text| text.trim().to_owned())
            .unwrap_or_default();
        assert!(answer.len() as u64 <= self.limits.report_bytes_max);
        Ok(if answer.is_empty() {
            ReadOnlyEnding::Failed("Agent returned no answer".into())
        } else {
            ReadOnlyEnding::Answered(answer)
        })
    }

    /// Refuses a read-only run on Windows before any agent state is made: V1 runs no such
    /// agent there, so the run fails with the owner's plain line. Past that line, the
    /// AppContainer boundary refuses the endpoint-only reach until its firewall rule exists.
    /// Takes the run and `stop`, unused. Returns `Invalid` for a bad run, the plain failure,
    /// or the boundary's `Unavailable` refusal.
    #[cfg(windows)]
    pub(crate) fn run_read_only(
        &self,
        run: &ReadOnlyRun<'_>,
        _stop: &dyn Fn() -> bool,
    ) -> Result<ReadOnlyEnding, RunError> {
        if !valid_run_name(run.name) || run.wall_time.is_zero() {
            return Err(RunError::Invalid("read-only run name or limit".into()));
        }
        if let Some(line) = confined_unavailable(std::env::consts::OS) {
            return Ok(ReadOnlyEnding::Failed(line.into()));
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

/// The ports a research job never listens on beside `OWNER_PORTS`: the owner's configured
/// local model ports for a tools-on job, none for a tools-off job or a runner with no
/// settings files. Inputs: the runner's settings files and whether the job has tools.
/// Output: the ports, or Invalid when the settings files do not read (OWNER-PORTS-FAIL-OPEN).
fn job_kept_ports(
    settings: Option<&(PathBuf, PathBuf)>,
    tools: bool,
) -> Result<Vec<u16>, RunError> {
    match (settings, tools) {
        (Some((global, local)), true) => crate::sandbox::settings_owner_ports(global, local)
            .map_err(|_| {
                RunError::Invalid(format!(
                    "the settings files {} and {} do not read, so the owner's model ports cannot be kept from the job",
                    global.display(),
                    local.display()
                ))
            }),
        _ => Ok(Vec::new()),
    }
}

/// The sandbox network a research job runs under (DEC-111 Q17, S4R1-2): a tools-off job
/// reaches only its model endpoint's port, on loopback for a local model, otherwise on any
/// host, and a tools-on job follows the project's network choice with the model still
/// reachable. Inputs: the project's agent policy, whether the job has tools, and the model
/// endpoint. Output: the sandbox network.
pub(crate) fn job_network(
    policy: &crate::sandbox::policy::AgentPolicy,
    tools: bool,
    model: SandboxRemote,
) -> SandboxNetwork {
    if tools {
        policy.network.sandbox(Some(model))
    } else {
        SandboxNetwork::Only(vec![model])
    }
}

/// The sandbox network a research job runs under and the model proxy it reaches its model
/// through (S4B-M1). A tools-on job under None whose model is not on loopback gets a proxy
/// for its account's hosts and reaches only the proxy's port; any other job gets
/// [`job_network`]. Inputs: the project's agent policy, whether the job has tools, its
/// account, its id, which the proxy's refusal lines name, and the owner's model ports the
/// proxy leaves alone. Output: the network and the proxy, which must live as long as the
/// job runs, or the refusal of an endpoint no proxy can reach by name or a proxy that could
/// not start.
pub(crate) fn job_reach(
    policy: &crate::sandbox::policy::AgentPolicy,
    tools: bool,
    account: &ResearchAccount,
    job: i64,
    kept: &[u16],
) -> Result<(SandboxNetwork, Option<ModelProxy>), RunError> {
    let model = model_remote(account)?;
    let proxied = tools
        && policy.network == crate::sandbox::policy::AgentNetwork::None
        && matches!(model, SandboxRemote::AnyHost(_));
    if !proxied {
        return Ok((job_network(policy, tools, model), None));
    }
    let fixed = |hosts: &[(&str, u16)]| -> Vec<ModelHost> {
        hosts
            .iter()
            .filter_map(|(name, port)| ModelHost::new(name, *port))
            .collect()
    };
    let hosts = match account {
        ResearchAccount::ManagedCodex { .. } => fixed(CODEX_MODEL_HOSTS),
        ResearchAccount::ManagedClaude { .. } => fixed(CLAUDE_MODEL_HOSTS),
        ResearchAccount::Endpoint { base_url, .. }
        | ResearchAccount::AnthropicEndpoint { base_url, .. } => {
            endpoint_model_host(base_url).into_iter().collect()
        }
    };
    if hosts.is_empty() {
        return Err(RunError::Invalid(
            "with no network, a research job with tools reaches only an https endpoint named by a host name".into(),
        ));
    }
    let proxy = ModelProxy::bind(job, hosts, None, kept)?;
    let network = SandboxNetwork::Only(vec![SandboxRemote::Loopback(proxy.port())]);
    Ok((network, Some(proxy)))
}

/// Puts a keyed endpoint account behind an engine key relay for one run (DEC-141 PA-8): the
/// relay reads the saved key from the engine folder and holds it in memory, and the account
/// comes back naming the relay's loopback URL, so the agent never gets the key. Inputs: the
/// engine folder holding `account-keys` and the account. Output: the account to run and its
/// relay, which must outlive the run; any other account unchanged with no relay.
fn relayed(
    engine: &Path,
    account: &ResearchAccount,
) -> Result<(ResearchAccount, Option<crate::model_gateway::relay::Relay>), RunError> {
    let (upstream, key) = match account {
        ResearchAccount::Endpoint {
            base_url,
            key: Some(key),
            ..
        }
        | ResearchAccount::AnthropicEndpoint {
            base_url,
            key: Some(key),
            ..
        } => (base_url, key),
        _ => return Ok((account.clone(), None)),
    };
    let relay = crate::harnesses::key_relay(engine, upstream, key)
        .map_err(|problem| RunError::Account(problem.0))?;
    let mut behind = account.clone();
    if let ResearchAccount::Endpoint { base_url, .. }
    | ResearchAccount::AnthropicEndpoint { base_url, .. } = &mut behind
    {
        *base_url = relay.base_url();
    }
    assert!(
        matches!(model_remote(&behind), Ok(SandboxRemote::Loopback(port)) if port == relay.port())
    );
    Ok((behind, Some(relay)))
}

/// The key file a keyed research run's harness reads its relay token from, once the
/// account's URL is checked to be a relay's: plain http on 127.0.0.1, so a keyed account is
/// never reached directly. Inputs: the agent home and the account URL. Output: the file's
/// path as text, or a refusal.
fn relay_key_file(home: &Path, url: &url::Url) -> Result<String, RunError> {
    let relayed = url.scheme() == "http"
        && url.host() == Some(url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST));
    if !relayed {
        return Err(RunError::Invalid(
            "a keyed endpoint runs only through its key relay".into(),
        ));
    }
    crate::harnesses::launch::key_file_text(&home.join(crate::harnesses::interactive::KEY_FILE))
        .map_err(|problem| RunError::Invalid(problem.0))
}

/// Writes a relayed run's placeholder token where its harness reads it, `<home>/.crazyagent-key`,
/// owner-only; the provider key itself stays in the relay. Inputs: the agent home and the
/// relay. Output: Ok, or the refusal of a token or file that cannot be written.
fn write_relay_token(
    home: &Path,
    relay: &crate::model_gateway::relay::Relay,
) -> Result<(), RunError> {
    let token = crate::harnesses::launch::Secret::new(relay.token().to_owned())
        .map_err(|problem| RunError::Invalid(problem.0))?;
    crate::harnesses::interactive::write_key(home, &token)
        .map(|_| ())
        .map_err(|problem| RunError::Invalid(problem.0))
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

/// Tells whether a loopback endpoint account's model provider refuses every connect, so a
/// caller can fail at once with a plain line in place of a harness retrying it for minutes.
/// Managed accounts, remote endpoints and any other connect error are not named. Input: the
/// account. Output: the owner's line naming the provider's host and port, else none.
pub(crate) fn unreached_provider(account: &ResearchAccount) -> Option<String> {
    let (ResearchAccount::Endpoint { base_url, .. }
    | ResearchAccount::AnthropicEndpoint { base_url, .. }) = account
    else {
        return None;
    };
    let Ok(SandboxRemote::Loopback(port)) = model_remote(account) else {
        return None;
    };
    let url = endpoint_url(base_url).ok()?;
    let addresses: Vec<std::net::SocketAddr> = match url.host()? {
        url::Host::Ipv4(address) => vec![(address, port).into()],
        url::Host::Ipv6(address) => vec![(address, port).into()],
        url::Host::Domain(_) => vec![
            (std::net::Ipv4Addr::LOCALHOST, port).into(),
            (std::net::Ipv6Addr::LOCALHOST, port).into(),
        ],
    };
    assert!(
        !addresses.is_empty() && addresses.iter().all(|address| address.ip().is_loopback()),
        "a loopback endpoint is checked on loopback addresses only"
    );
    let unreached = addresses.iter().all(|address| {
        refused(crate::spawn_gate::connect_tcp_std_timeout(address, PROVIDER_REACH_TIME).map(drop))
    });
    let host = url.host_str()?;
    unreached.then(|| {
        format!(
            "Can't reach the model provider at {host}:{port}. Start it or pick another account in this pool, then try again."
        )
    })
}

/// [`unreached_provider`] where this OS runs confined model runs, else none, so a run's own
/// line that confined runs are unavailable here keeps its place. Every read-only run's caller
/// asks it before the run. Input: the account. Output: the owner's line, or none.
pub(crate) fn unreached_provider_here(account: &ResearchAccount) -> Option<String> {
    if confined_unavailable(std::env::consts::OS).is_some() {
        return None;
    }
    unreached_provider(account)
}

/// Tells whether one connect to a loopback model provider proves nothing listens there:
/// only a refused connect does, never a success, a timeout or another error such as too
/// many open files. Input: the connect's outcome. Output: true only for a refusal.
fn refused(outcome: std::io::Result<()>) -> bool {
    matches!(outcome, Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused)
}

/// The hosts a signed-in Claude account's model traffic goes to, the only ones a Claude
/// Code agent under the None network reaches through its model proxy (S4b).
pub(crate) const CLAUDE_MODEL_HOSTS: &[(&str, u16)] = &[("api.anthropic.com", 443)];

/// The hosts a signed-in Codex account's model traffic goes to: chatgpt.com for a ChatGPT
/// sign-in, api.openai.com for an API key. A managed profile does not say at launch which
/// one it holds, so a Codex agent under the None network may reach both (S4b).
pub(crate) const CODEX_MODEL_HOSTS: &[(&str, u16)] =
    &[("chatgpt.com", 443), ("api.openai.com", 443)];

/// The one host an endpoint account's traffic goes to through a model proxy (S4b): its
/// base URL's DNS name and port. Input: the base URL. Output: the host, or none for an
/// http URL or an IP literal, which a CONNECT proxy checking names cannot tell apart from
/// any other address.
pub(crate) fn endpoint_model_host(base_url: &str) -> Option<ModelHost> {
    let url = endpoint_url(base_url).ok()?;
    let port = url.port_or_known_default()?;
    match url.host()? {
        url::Host::Domain(name) if url.scheme() == "https" => ModelHost::new(name, port),
        _ => None,
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
        .env("CODEX_HOME", home)
        .env("RUST_LOG", CODEX_LOG);
    let trust = crate::harnesses::codex_folder::untrusted(worktree)
        .map_err(|_| RunError::Invalid("research worktree path".into()))?;
    command.arg("-c").arg(trust);
    if !tools {
        for setting in crate::harnesses::launch::CODEX_TOOLS_OFF {
            command.arg("-c").arg(setting);
        }
        command.args(["-c", crate::harnesses::launch::CODEX_NO_USER_INPUT]);
    }
    match account {
        ResearchAccount::Endpoint {
            base_url,
            model,
            key,
        } => {
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
                ));
            if key.is_some() {
                let file = relay_key_file(home, &url)?;
                command.arg("-c").arg(format!(
                    "model_providers.crazyagent_endpoint.auth={{command=\"/bin/cat\",args=[\"{file}\"]}}"
                ));
            }
            command.arg("-m").arg(model);
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
/// run: no MCP servers, settings sources, hooks or saved sessions, tools on, or
/// off with the no-tools system prompt and one model turn; the account as
/// either a managed config directory or a keyless Anthropic endpoint in bare
/// mode that never reads the keychain.
/// Takes the command, the agent home and temp dir, the tools switch, the most
/// output tokens of each request (`None` for Claude Code's own limit) and the
/// account. Returns an error for an account that does not fit Claude Code.
fn claude_arguments(
    command: &mut Command,
    home: &Path,
    tmp: &Path,
    tools: bool,
    output_tokens: Option<u32>,
    account: &ResearchAccount,
) -> Result<(), RunError> {
    let key_file = match account {
        ResearchAccount::AnthropicEndpoint {
            base_url,
            key: Some(_),
            ..
        } => Some(relay_key_file(home, &endpoint_url(base_url)?)?),
        _ => None,
    };
    let settings = match &key_file {
        Some(file) => {
            let mut settings: serde_json::Value =
                serde_json::from_str(CLAUDE_SETTINGS).expect("fixed Claude settings are JSON");
            settings["apiKeyHelper"] = format!("/bin/cat '{file}'").into();
            settings.to_string()
        }
        None => CLAUDE_SETTINGS.to_owned(),
    };
    command
        .args(CLAUDE_ARGUMENTS)
        .args(CLAUDE_EVENTS)
        .arg("--settings")
        .arg(settings)
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
        command.args(CLAUDE_TOOLS_OFF).args(CLAUDE_ONE_TURN);
    }
    if let Some(tokens) = output_tokens {
        assert!(tokens > 0, "an output cap of zero tokens leaves no answer");
        command.env(CLAUDE_OUTPUT_TOKENS, tokens.to_string());
    }
    match account {
        ResearchAccount::AnthropicEndpoint {
            base_url, model, ..
        } => {
            let url = endpoint_url(base_url)?;
            if !valid_model(model) {
                return Err(RunError::Invalid("endpoint model name".into()));
            }
            command
                .arg("--bare")
                .arg("--model")
                .arg(model)
                .env("ANTHROPIC_BASE_URL", url.as_str().trim_end_matches('/'))
                .env("ANTHROPIC_SMALL_FAST_MODEL", model);
            if key_file.is_none() {
                command.env("ANTHROPIC_API_KEY", LOCAL_PLACEHOLDER_KEY);
            }
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
/// Takes the research account, already behind its relay when keyed, the home and that
/// relay. Returns the binding and the model: a keyed account binds the relay's URL and
/// placeholder token, never the provider key; a keyed account with no relay is refused.
fn pane_account(
    account: &ResearchAccount,
    home: &Path,
    relay: Option<&crate::model_gateway::relay::Relay>,
) -> Result<(AccountBinding, Option<String>), RunError> {
    let home = home.to_path_buf();
    Ok(match account {
        ResearchAccount::Endpoint {
            base_url,
            model,
            key,
        }
        | ResearchAccount::AnthropicEndpoint {
            base_url,
            model,
            key,
        } => {
            let token = match (key, relay) {
                (None, None) => None,
                (Some(_), Some(relay)) => {
                    assert_eq!(base_url, &relay.base_url());
                    Some(
                        crate::harnesses::launch::Secret::new(relay.token().to_owned())
                            .map_err(|problem| RunError::Invalid(problem.0))?,
                    )
                }
                _ => {
                    return Err(RunError::Invalid(
                        "a keyed endpoint runs only through its key relay".into(),
                    ));
                }
            };
            (
                AccountBinding::Url {
                    home,
                    base_url: base_url.clone(),
                    key: token,
                },
                Some(model.clone()),
            )
        }
        ResearchAccount::ManagedCodex { model, .. }
        | ResearchAccount::ManagedClaude { model, .. } => {
            (AccountBinding::Subscription { home }, model.clone())
        }
    })
}

/// The error every owner-only research file or folder reports where none can be made.
/// Takes nothing. Returns an `Unsupported` I/O error reading `research_runner_unavailable`.
#[cfg(not(unix))]
fn unavailable() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "research_runner_unavailable")
}

/// Creates a fresh 0600 file below a folder the engine holds, following no link in any
/// part below that folder, so an agent that swapped its temp folder for a link is refused
/// rather than written through. Takes the folder and the path in it. Returns the new file,
/// or the I/O error, also when it exists.
#[cfg(unix)]
fn create_private(base: &Path, path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};
    let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
    let below = path.strip_prefix(base).map_err(|_| invalid())?;
    let name = below.file_name().ok_or_else(invalid)?;
    let folder = crate::platform::open_beneath(
        base,
        below.parent().unwrap_or(Path::new("")),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
    )?;
    let created = rustix::fs::openat(
        &folder,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?;
    Ok(File::from(created))
}

/// Refuses an owner-only research file where none can be made.
/// Takes the folder and path, unused. Returns `research_runner_unavailable`.
#[cfg(not(unix))]
fn create_private(_base: &Path, _path: &Path) -> io::Result<File> {
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

/// Writes a fresh 0600 file below a folder the engine holds ([`create_private`]). Takes
/// the folder, the path in it and the bytes. Returns Ok once synced, or the I/O error.
fn write_private(base: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = create_private(base, path)?;
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

/// Runs one git command the engine makes in a project folder it created: the safe git
/// options, a cleared environment with a private HOME and TMPDIR, no system config, no input
/// or output, bounded by SETUP_TIMEOUT. Inputs: the folder, the private home and the
/// arguments. Output: whether git succeeded, or the spawn or wait failure.
pub(crate) fn engine_git(root: &Path, tmp: &Path, arguments: &[&str]) -> Result<bool, RunError> {
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
    Ok(wait_bounded(crate::spawn_gate::spawn(&mut command)?, SETUP_TIMEOUT)? == Ended::Succeeded)
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
    let git = |arguments: &[&str]| engine_git(root, tmp, arguments);
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

/// The owner's line for a team agent, research job or onboarding mapping run on a system
/// that runs no such agent in V1: Linux (DEC-094), whose boundaries cannot keep a worktree's
/// git links read-only, and Windows, which cannot load the Seatbelt profile they run under.
/// Input: the operating system name as `std::env::consts::OS` gives it. Output: the line,
/// or None where such agents run.
pub(crate) fn confined_unavailable(os: &str) -> Option<&'static str> {
    match os {
        "linux" => Some("Team agents and research are not available on Linux in this version."),
        "windows" => Some("Team agents and research are not available on Windows in this version."),
        _ => None,
    }
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

/// Refuses to add a worktree at a commit, as `add_worktree` does where Seatbelt is missing.
/// Takes the sandbox, profile, worktree and commit, all unused. Returns the same refusal.
#[cfg(not(unix))]
pub(crate) fn add_worktree_at(
    sandbox: &ProjectSandbox,
    profile: &Path,
    worktree: &Path,
    _base: &str,
) -> Result<(), RunError> {
    add_worktree(sandbox, profile, worktree)
}

/// Adds a detached git worktree at `worktree` in the project folder, running git under the
/// sandbox's profile, which it installs at `profile` as the only one that may make a
/// checkout's `.git` link; the caller confines and reinstalls it before any agent runs. The
/// worktree is added without a checkout and made local-only, and the checkout blanks every
/// filter driver its own effective config names. Takes the sandbox, profile path, worktree.
#[cfg(unix)]
pub(crate) fn add_worktree(
    sandbox: &ProjectSandbox,
    profile: &Path,
    worktree: &Path,
) -> Result<(), RunError> {
    add_worktree_at(sandbox, profile, worktree, "HEAD")
}

/// Adds a detached git worktree as `add_worktree` does, checked out at `base`: `HEAD`, or
/// the full commit id a lead's worktree holds for a worker cut from it (N1).
/// Takes the sandbox, its installed profile, the new worktree path and the commit.
#[cfg(unix)]
pub(crate) fn add_worktree_at(
    sandbox: &ProjectSandbox,
    profile: &Path,
    worktree: &Path,
    base: &str,
) -> Result<(), RunError> {
    assert!(
        base == "HEAD"
            || (matches!(base.len(), 40 | 64) && base.bytes().all(|b| b.is_ascii_hexdigit()))
    );
    let _setup = repository_setup(sandbox.project_root());
    let worktrees = crate::projects_home::state_folder(sandbox.project_root()).join("worktrees");
    if worktree.parent() != Some(worktrees.as_path()) {
        return Err(RunError::Invalid("worktree must sit in worktrees/".into()));
    }
    fs::create_dir_all(&worktrees)?;
    sandbox.clone().adding_worktrees().install(profile)?;
    let mut add = worktree_git(sandbox, profile);
    add.args(["worktree", "add", "-q", "--no-checkout", "--detach"])
        .arg(worktree)
        .arg(base);
    setup_step(&mut add, "worktree_add_failed")?;
    crate::worktrees::lock_remotes(sandbox.project_root(), worktree)
        .map_err(|_| RunError::Setup("worktree_remote_lock_failed"))?;
    let overrides = filter_overrides(&filter_keys(sandbox, profile, worktree)?)?;
    let mut checkout = worktree_git(sandbox, profile);
    checkout
        .args(&overrides)
        .arg("-C")
        .arg(worktree)
        .args(["reset", "-q", "--hard", "HEAD"]);
    setup_step(&mut checkout, "worktree_checkout_failed")?;
    assert!(worktree.starts_with(sandbox.project_root()));
    Ok(())
}

/// Adds a linked lead's native teammate's own linked worktree of the project (OP-9b) at
/// `worktree`, in the lead's teammates' folder, on `branch`: a new branch cut from `base`,
/// or with no base the branch an earlier run left, checked out as it stands. Git runs
/// under the lead's teammates' setup profile as [`add_worktree_at`] runs it: added
/// without a checkout, its effective config listed in the new worktree, then checked out
/// with every filter driver that listing names blanked. Takes the sandbox, its installed
/// profile, the new worktree path, the branch and the commit. Returns unit, or the step
/// that failed.
#[cfg(unix)]
pub(crate) fn add_teammate_worktree_at(
    sandbox: &ProjectSandbox,
    profile: &Path,
    worktree: &Path,
    branch: &str,
    base: Option<&str>,
) -> Result<(), RunError> {
    if !worktree.starts_with(sandbox.project_root())
        || worktree
            .parent()
            .and_then(Path::file_name)
            .is_none_or(|folder| folder != crate::agent_git::TEAMMATES_FOLDER)
        || !branch.starts_with(crate::agent_git::LINKED_PREFIX)
        || base.is_some_and(|base| {
            !matches!(base.len(), 40 | 64) || !base.bytes().all(|b| b.is_ascii_hexdigit())
        })
    {
        return Err(RunError::Invalid(
            "a teammate's worktree must sit in its lead's teammates' folder on a teammate branch"
                .into(),
        ));
    }
    let _setup = repository_setup(sandbox.project_root());
    let mut add = worktree_git(sandbox, profile);
    add.args(["worktree", "add", "-q", "--no-checkout"]);
    match base {
        Some(base) => add.args(["-b", branch]).arg(worktree).arg(base),
        None => add.arg(worktree).arg(branch),
    };
    setup_step(&mut add, "worktree_add_failed")?;
    let overrides = filter_overrides(&filter_keys(sandbox, profile, worktree)?)?;
    let mut checkout = worktree_git(sandbox, profile);
    checkout
        .args(&overrides)
        .arg("-C")
        .arg(worktree)
        .args(["reset", "-q", "--hard", "HEAD"]);
    setup_step(&mut checkout, "worktree_checkout_failed")
}

/// Runs one worktree setup git step within the setup time limit. Takes the command and
/// the plain code for a step that exits with a failure. Returns unit, or [`RunError::Setup`]
/// with that code, or with `setup_timed_out` for a step stopped at the limit, so the owner
/// sees which step failed and how.
#[cfg(unix)]
fn setup_step(command: &mut Command, failed: &'static str) -> Result<(), RunError> {
    match wait_bounded(crate::spawn_gate::spawn(command)?, SETUP_TIMEOUT)? {
        Ended::Succeeded => Ok(()),
        Ended::Failed => Err(RunError::Setup(step_failure(failed, Path::new(TOOLS_ROOT)))),
        Ended::TimedOut => Err(RunError::Setup(SETUP_TIMED_OUT)),
    }
}

/// The real git binary for the engine's sandboxed setup steps, resolved once outside the
/// sandbox so no setup step runs the `/usr/bin/git` xcrun shim. Inside the sandbox the
/// shim's `xcrun_db` cache write (to the per-user temp dir, which it picks regardless of
/// TMPDIR) is denied on every call, and its developer-dir lookup is extra work that fails
/// under memory pressure; the resolved real git has neither. Falls back to [`GIT`] where
/// xcrun cannot resolve it (for example on Linux), no worse than before. Only a resolved
/// path is kept: a lookup that fails once under load is asked again at the next step
/// rather than leaving every later setup of the process on the shim. Returns the path.
#[cfg(unix)]
pub(crate) fn setup_git() -> &'static Path {
    static RESOLVED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    if let Some(path) = RESOLVED.get() {
        return path;
    }
    match real_git() {
        Some(path) => RESOLVED.get_or_init(|| path),
        None => Path::new(GIT),
    }
}

/// The Command Line Tools git, [`TOOLS_ROOT`]'s `usr/bin/git`. Every agent profile reads its
/// folder, so under Seatbelt this git finds its own helpers (`git-upload-pack`,
/// `git pack-objects` and the rest) in its `libexec/git-core` instead of falling through the
/// search path to the `/usr/bin` shims.
#[cfg(unix)]
pub(crate) const TOOLS_GIT: &str = "/Library/Developer/CommandLineTools/usr/bin/git";

/// The Command Line Tools root, which every agent profile reads.
#[cfg(unix)]
pub(crate) const TOOLS_ROOT: &str = "/Library/Developer/CommandLineTools";

#[cfg(all(test, unix))]
thread_local! {
    /// How many times this thread asked for [`sandboxed_git`], so a row can show an async path
    /// leaves that lookup, which can run xcrun, to a blocking thread.
    pub(crate) static SANDBOXED_GIT_LOOKUPS: std::cell::Cell<u32> =
        const { std::cell::Cell::new(0) };
}

/// The git for commands that run inside an agent's sandbox: the agent's own git script and
/// the engine's setup steps under the agent profile. On a Mac that is the Command Line Tools
/// git whenever it is installed, since a toolchain git elsewhere, such as Xcode's, runs under
/// the profile but cannot read its own helpers there (T3-09); else [`setup_git`]. Engine git
/// outside any sandbox keeps the selected toolchain's [`setup_git`]. Returns the path.
#[cfg(unix)]
pub(crate) fn sandboxed_git() -> &'static Path {
    #[cfg(test)]
    SANDBOXED_GIT_LOOKUPS.set(SANDBOXED_GIT_LOOKUPS.get() + 1);
    if cfg!(target_os = "macos") && agent_git_problem_at(Path::new(TOOLS_ROOT)).is_none() {
        Path::new(TOOLS_GIT)
    } else {
        setup_git()
    }
}

/// Whether agent git lacks the Command Line Tools on this Mac, so its helper calls would
/// reach the denied xcrun shims. Output: [`AGENT_GIT_NEEDS_TOOLS`], or none.
#[cfg(target_os = "macos")]
pub(crate) fn agent_git_problem() -> Option<&'static str> {
    agent_git_problem_at(Path::new(TOOLS_ROOT))
}

/// Whether agent git lacks the Command Line Tools; off a Mac agent git needs none.
/// Output: none.
#[cfg(not(target_os = "macos"))]
pub(crate) fn agent_git_problem() -> Option<&'static str> {
    None
}

/// Whether a toolchain root lacks the Command Line Tools git (`usr/bin/git` under it).
/// Input: the absolute root. Output: [`AGENT_GIT_NEEDS_TOOLS`], or none when the git is there.
#[cfg(unix)]
fn agent_git_problem_at(tools: &Path) -> Option<&'static str> {
    assert!(tools.is_absolute());
    (!tools.join("usr").join("bin").join("git").is_file()).then_some(AGENT_GIT_NEEDS_TOOLS)
}

/// The code a failed setup git step reports: the missing Command Line Tools when that is
/// the cause on a Mac, else the step's own code. Inputs: the step's code and the toolchain
/// root. Output: the code.
#[cfg(unix)]
fn step_failure(failed: &'static str, tools: &Path) -> &'static str {
    if cfg!(target_os = "macos") {
        agent_git_problem_at(tools).unwrap_or(failed)
    } else {
        failed
    }
}

/// Asks xcrun once, outside any sandbox, for the active toolchain's git. Returns an
/// absolute path that exists and is not the `/usr/bin/git` shim, or None when xcrun is
/// absent or fails so the caller keeps using [`GIT`].
#[cfg(unix)]
fn real_git() -> Option<PathBuf> {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/xcrun")
            .args(["--find", "git"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(std::str::from_utf8(&output.stdout).ok()?.trim());
    (path.is_absolute() && path != Path::new(GIT) && path.is_file()).then_some(path)
}

/// A git command for the engine's own worktree steps under the agent's installed profile:
/// no global config, no lazy fetch and the [`GIT_SAFE`] settings, with no input or output.
/// Takes the sandbox and its installed profile. Returns the command, ready for arguments.
#[cfg(unix)]
fn worktree_git(sandbox: &ProjectSandbox, profile: &Path) -> Command {
    let mut git = sandbox.command(profile, sandboxed_git());
    git.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(GIT_SAFE)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(setup_debug_stderr());
    git
}

/// What one git command run under an agent's profile ended with: its exit code, None when
/// it was stopped at the setup time limit or by a signal, its output up to the caller's
/// bound and whether it printed more.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct AgentGit {
    pub(crate) code: Option<i32>,
    pub(crate) stdout: Vec<u8>,
    pub(crate) overflow: bool,
}

/// Runs one git command in a folder an agent owns with that agent's own rights (DEC-104
/// K5b): under its installed profile, as the worktree setup steps run, with the
/// [`GIT_SAFE`] settings and the caller's extra variables, within the setup time limit.
/// Its output is read while it runs, so a long listing never stalls it. Takes the
/// agent's sandbox, its installed profile, the folder, the arguments, the variables and
/// the most output bytes kept. Returns how it ended, or the spawn failure.
#[cfg(unix)]
pub(crate) fn agent_git(
    sandbox: &ProjectSandbox,
    profile: &Path,
    folder: &Path,
    arguments: &[&str],
    variables: &[(&str, &str)],
    limit: usize,
) -> Result<AgentGit, RunError> {
    let mut command = worktree_git(sandbox, profile);
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .envs(variables.iter().copied())
        .arg("-C")
        .arg(folder)
        .args(arguments);
    let (status, stdout, overflow) = git_output(&mut command, limit)?;
    Ok(AgentGit {
        code: status.and_then(|status| status.code()),
        stdout,
        overflow,
    })
}

/// Runs one setup git command within the setup time limit, reading its output while it
/// runs, so output larger than a pipe holds never stalls it. Takes the command, ready but
/// for its output pipe, and the most output bytes kept. Returns its exit status, none when
/// it was stopped at the limit, its output up to the bound and whether it printed more.
#[cfg(unix)]
fn git_output(
    command: &mut Command,
    limit: usize,
) -> Result<(Option<ExitStatus>, Vec<u8>, bool), RunError> {
    command.stdout(Stdio::piped());
    let mut child = crate::spawn_gate::spawn(command)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| RunError::Invalid("git gave no output pipe".into()))?;
    let reader = std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut stdout = stdout;
        let read = stdout
            .by_ref()
            .take(limit as u64 + 1)
            .read_to_end(&mut kept);
        let _ = io::copy(&mut stdout, &mut io::sink());
        read.map(|_| kept)
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() >= SETUP_TIMEOUT {
            stop_timed_out(&mut child);
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stdout = reader
        .join()
        .map_err(|_| RunError::Invalid("git output could not be read".into()))??;
    let overflow = stdout.len() > limit;
    stdout.truncate(limit);
    assert!(stdout.len() <= limit);
    Ok((status, stdout, overflow))
}

#[cfg(unix)]
static SETUP_DEBUG_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The stderr for a setup git step: discarded, unless `CRAZYAGENT_SETUP_DEBUG` names a
/// directory, when each step's stderr goes to its own file there so a failing run can be
/// read afterwards. Diagnostic only; the default path drops stderr as before.
#[cfg(unix)]
fn setup_debug_stderr() -> Stdio {
    if let Some(dir) = std::env::var_os("CRAZYAGENT_SETUP_DEBUG") {
        let seq = SETUP_DEBUG_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = Path::new(&dir).join(format!("git-{}-{seq}.err", std::process::id()));
        if let Ok(file) = fs::File::create(&path) {
            return Stdio::from(file);
        }
    }
    Stdio::null()
}

/// The git arguments that list, NUL-separated, the filter keys of every config file git
/// reads, the only keys [`filter_overrides`] uses, so the listing grows with a repository's
/// filter drivers and never with its branches or remotes. The `-c` adds one key of the
/// engine's own, which git reads last: a listing that ends with it ([`FILTER_KEYS_END`])
/// is whole, and git always has a key to print, so no exit status stands for "no filter".
#[cfg(unix)]
pub(crate) const FILTER_KEYS: [&str; 7] = [
    "-c",
    "filter.crazyagent-listing-end.required=false",
    "config",
    "--name-only",
    "-z",
    "--get-regexp",
    "^filter\\.",
];

/// The key the `-c` of [`FILTER_KEYS`] sets with its NUL, as git prints it at the end of a
/// whole listing.
#[cfg(unix)]
const FILTER_KEYS_END: &[u8] = b"filter.crazyagent-listing-end.required\0";

/// The repository's own filter keys in what a [`FILTER_KEYS`] listing printed. Takes git's
/// whole output. Returns the NUL-separated keys before the engine's closing key, or none
/// for a listing that does not end with that key on its own, which is cut short or not a
/// listing at all.
#[cfg(unix)]
pub(crate) fn listed_filter_keys(listing: &[u8]) -> Option<&[u8]> {
    let keys = listing.strip_suffix(FILTER_KEYS_END)?;
    (keys.is_empty() || keys.ends_with(&[0])).then_some(keys)
}

/// Lists the filter keys a new worktree's effective config names, with git run under the
/// agent's installed profile within the setup time limit and its output read while it runs.
/// Takes the sandbox, its installed profile and the worktree. Returns the NUL-separated
/// keys for [`filter_overrides`], `Setup` with `worktree_config_failed` or `setup_timed_out`
/// for a git that fails or runs past the limit, or `Invalid` for a listing that is not
/// whole or is longer than [`GIT_CONFIG_LISTING_BYTES_MAX`].
#[cfg(unix)]
fn filter_keys(
    sandbox: &ProjectSandbox,
    profile: &Path,
    worktree: &Path,
) -> Result<Vec<u8>, RunError> {
    let refused = || RunError::Invalid("git could not list the worktree's config".into());
    let mut listing = worktree_git(sandbox, profile);
    listing.arg("-C").arg(worktree).args(FILTER_KEYS);
    let (status, printed, overflow) = git_output(&mut listing, GIT_CONFIG_LISTING_BYTES_MAX)?;
    let Some(status) = status else {
        return Err(RunError::Setup(SETUP_TIMED_OUT));
    };
    if !status.success() {
        return Err(RunError::Setup(step_failure(
            "worktree_config_failed",
            Path::new(TOOLS_ROOT),
        )));
    }
    if overflow {
        return Err(refused());
    }
    let keys = listed_filter_keys(&printed).ok_or_else(refused)?;
    assert!(keys.len() < printed.len());
    Ok(keys.to_vec())
}

/// The `-c` arguments that switch off every filter driver a config listing names: empty
/// smudge, clean and process commands and `required=false`, so a checkout runs none of them
/// and still succeeds. Takes NUL-separated config keys, as [`listed_filter_keys`] gives them.
/// Returns the arguments, or `Invalid` for a driver name a `-c` cannot carry or too many.
#[cfg(unix)]
pub(crate) fn filter_overrides(listing: &[u8]) -> Result<Vec<String>, RunError> {
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

/// How a setup command run within a time limit ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    Succeeded,
    Failed,
    TimedOut,
}

/// Waits for a setup command for at most `limit`, stopping its process group when it runs
/// past. Takes the command's process, which leads its own group, and the limit. Returns
/// whether it succeeded, failed or was stopped at the limit.
fn wait_bounded(mut child: Child, limit: Duration) -> Result<Ended, RunError> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(if status.success() {
                Ended::Succeeded
            } else {
                Ended::Failed
            });
        }
        if started.elapsed() >= limit {
            stop_timed_out(&mut child);
            return Ok(Ended::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Stops a setup command that ran past its limit: kills the process group it leads,
/// reaps it and waits for the group to empty. Takes the command's process.
#[cfg(unix)]
fn stop_timed_out(child: &mut Child) {
    let group = child.id();
    kill_group(group);
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
/// kills the whole group, reaps the agent, waits until the group is empty and
/// then has the agent's warden, where its sandbox gives one, stop every process
/// left in that sandbox, those that left the group included, so no agent
/// process outlives the run that started it. If the engine itself dies first,
/// its [`Lifeline`] kills the group and the warden stops the sandbox. `cleanup`
/// holds the code of a warden that did not confirm the sandbox empty.
pub(crate) struct AgentProcess {
    pub(crate) child: Child,
    pub(crate) group: u32,
    #[cfg(unix)]
    lifeline: Lifeline,
    warden: Option<Warden>,
    stopped: bool,
    cleanup: Result<(), &'static str>,
}

impl AgentProcess {
    /// Starts an agent command that leads its own process group, with the warden
    /// [`crate::sandbox::warden::spawn`] gives a sandboxed one, and starts its
    /// lifeline. Takes the command. Returns the guard, or the error that kept the
    /// agent or its lifeline from starting, after stopping what had started.
    pub(crate) fn spawn(command: &mut Command) -> io::Result<Self> {
        let (child, warden) = crate::sandbox::warden::spawn(command)?;
        Self::new(child, warden)
    }

    /// Takes a just spawned agent that leads its own process group and its
    /// warden, if any, and starts its lifeline. Returns the guard, or the error
    /// that kept the lifeline from starting, after stopping the agent's group
    /// and sandbox.
    pub(crate) fn new(child: Child, warden: Option<Warden>) -> io::Result<Self> {
        let group = child.id();
        assert!(group > 1);
        #[cfg(unix)]
        let lifeline = match Lifeline::watch(group) {
            Ok(lifeline) => lifeline,
            Err(error) => {
                let mut child = child;
                kill_group(group);
                let _ = child.wait();
                await_group_exit(group);
                let _ = crate::sandbox::warden::fire_reported(warden, "agent");
                return Err(error);
            }
        };
        Ok(Self {
            child,
            group,
            #[cfg(unix)]
            lifeline,
            warden,
            stopped: false,
            cleanup: Ok(()),
        })
    }

    /// Kills the agent's group once, reaps the agent, waits for the group to
    /// empty and fires its warden. Later calls do nothing, so a reused group id
    /// is never killed. Returns, on every call, Ok or the code of a warden that
    /// did not confirm the sandbox empty, for the caller to keep what the agent used.
    #[cfg(unix)]
    pub(crate) fn stop(&mut self) -> Result<(), &'static str> {
        if !self.stopped {
            kill_group(self.group);
            let _ = self.child.wait();
            await_group_exit(self.group);
            self.cleanup = crate::sandbox::warden::fire_reported(self.warden.take(), "agent");
            self.lifeline.release();
            self.stopped = true;
        }
        assert!(self.warden.is_none());
        self.cleanup
    }

    /// Kills the agent and reaps it where no process group exists, once; later calls do
    /// nothing. Takes the process. Returns, on every call, Ok or the warden's code.
    #[cfg(not(unix))]
    pub(crate) fn stop(&mut self) -> Result<(), &'static str> {
        if !self.stopped {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.cleanup = crate::sandbox::warden::fire_reported(self.warden.take(), "agent");
            self.stopped = true;
        }
        assert!(self.warden.is_none());
        self.cleanup
    }
}

impl Drop for AgentProcess {
    /// Stops the agent's process group if the run did not already stop it. A run that
    /// fails before its own stop has no outcome left to report; the warden logs one.
    fn drop(&mut self) {
        let _ = self.stop();
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

/// Copies an agent stream into its log, counting every byte read and logging
/// at most `log_max` bytes, until the stream ends.
#[cfg(unix)]
pub(crate) fn drain(stream: Box<dyn Read + Send>, counted: &AtomicU64, log: File, log_max: u64) {
    assert!(drain_watching(stream, counted, log, log_max, None, None).is_none());
}

/// Drains like [`drain`]. With `limited`, it sets the flag once a line shows
/// Codex waiting to retry a request refused for a rate or usage limit; a line
/// is checked on at most its first 16 KiB. With `follow`, it reads every byte
/// into that Claude Code stream, also past the log bound, and returns it.
fn drain_watching(
    mut stream: Box<dyn Read + Send>,
    counted: &AtomicU64,
    mut log: File,
    log_max: u64,
    limited: Option<&AtomicBool>,
    mut follow: Option<ClaudeStream>,
) -> Option<ClaudeStream> {
    assert!(limited.is_none() || follow.is_none());
    let mut buffer = [0_u8; 8192];
    let mut logged = 0_u64;
    let mut line = Vec::new();
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
        if let Some(follow) = follow.as_mut() {
            follow.read(&buffer[..read]);
        }
        let Some(limited) = limited else { continue };
        for &byte in &buffer[..read] {
            if byte == b'\n' {
                if codex_retry_limited(&String::from_utf8_lossy(&line)) {
                    limited.store(true, Ordering::SeqCst);
                }
                line.clear();
            } else if line.len() < LOG_LINE_BYTES_MAX {
                line.push(byte);
            }
        }
        assert!(line.len() <= LOG_LINE_BYTES_MAX);
    }
    if let Some(follow) = follow.as_mut() {
        follow.close();
    }
    follow
}

/// Tells whether one Codex log line is Codex waiting to retry a request the provider
/// refused for a rate or usage limit. Takes the line. Returns true only for the retry
/// target with a limit cause, never for ids or other targets holding the same digits.
fn codex_retry_limited(line: &str) -> bool {
    line.contains("codex_core::responses_retry")
        && line
            .split_once("sampling_error=")
            .is_some_and(|(_, cause)| rate_limited(cause))
}

/// Gives a Claude Code research agent that reaches its model by key or address, with no
/// signed-in account handed to it, its own keychain in its home as a handoff gives a
/// signed-in one, so a harness that stores anything there never makes macOS ask the owner
/// to create a keychain (DEC-146 KCA-M2). Codex keeps nothing in a keychain. Inputs: the
/// research state folder, the harness and the agent home. Output: done, or the refusal.
fn endpoint_keychain(state: &Path, harness: ResearchHarness, home: &Path) -> Result<(), RunError> {
    #[cfg(target_os = "macos")]
    if matches!(harness, ResearchHarness::ClaudeCode) {
        crate::engine::keychain::fresh_profile_keychain(state, home, &[])
            .map_err(|_| RunError::Account("agent_keychain_unavailable".into()))?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (state, harness, home);
    Ok(())
}

/// Tells whether a research agent that exited failing was refused for a rate or usage
/// limit: Codex's own failure message in its stdout log, read bounded and never through
/// a link, or a limit line Claude Code ended its stream with. Takes the harness, the agent
/// state folder, the Claude Code stream the run was read into, if any, and the log bound.
fn refused_for_limit(
    harness: ResearchHarness,
    agent: &Path,
    followed: Option<&ClaudeStream>,
    max: u64,
) -> bool {
    match harness {
        ResearchHarness::Codex => {
            assert!(followed.is_none());
            rate_limited(&agent_failure(&agent.join(EVENTS_LOG), harness, max))
        }
        ResearchHarness::ClaudeCode => followed.is_some_and(ClaudeStream::limited),
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

/// The reply a research agent ends its turn with, kept as its words arrive: for Claude Code
/// every part it said since its last tool call, joined, as it goes on with an answer cut at
/// the output cap in a new message; for Codex its last message. A reply that outgrows
/// `bytes_max` is none, as a report that long is refused, until a tool call starts over.
struct Reply {
    harness: ResearchHarness,
    text: String,
    bytes_max: usize,
    over: bool,
}

impl Reply {
    /// An empty reply. Inputs: the harness and the most bytes it may hold. Output: the reply.
    fn new(harness: ResearchHarness, bytes_max: usize) -> Self {
        Self {
            harness,
            text: String::new(),
            bytes_max,
            over: false,
        }
    }

    /// Takes words the agent said. Input: the words. Output: none.
    fn said(&mut self, words: &str) {
        if self.harness == ResearchHarness::Codex {
            self.start_over();
        }
        if !self.over && self.text.len().saturating_add(words.len()) > self.bytes_max {
            self.over = true;
            self.text = String::new();
        }
        if !self.over {
            self.text.push_str(words);
        }
        assert!(self.text.len() <= self.bytes_max);
        assert!(!self.over || self.text.is_empty());
    }

    /// Takes a tool call the agent made: a Claude Code reply starts over. Output: none.
    fn tool(&mut self) {
        if self.harness == ResearchHarness::ClaudeCode {
            self.start_over();
        }
        assert!(self.text.len() <= self.bytes_max);
    }

    /// Empties the reply. Output: none.
    fn start_over(&mut self) {
        self.text.clear();
        self.over = false;
        assert!(self.text.is_empty());
    }

    /// The reply. Output: its text, or `None` once it outgrew its bound.
    fn text(&self) -> Option<&str> {
        assert!(self.text.len() <= self.bytes_max);
        assert!(!self.over || self.text.is_empty());
        (!self.over).then_some(self.text.as_str())
    }
}

/// A Claude Code run's JSON event lines, read as they arrive into the [`Reply`] its main
/// thread ends with: a sub-agent's events are skipped, and Claude Code's own error messages
/// are kept apart from the model's words. A line past [`CLAUDE_LINE_BYTES_MAX`] is a main
/// tool result, a sub-agent's line, or else leaves the run with no answer ([`LongLine`]).
struct ClaudeStream {
    reply: Reply,
    line: Vec<u8>,
    long: Option<LongLine>,
    unread: bool,
    finished: bool,
    limited: bool,
}

impl ClaudeStream {
    /// An empty stream. Input: the most bytes its reply may hold. Output: the stream.
    fn new(bytes_max: usize) -> Self {
        Self {
            reply: Reply::new(ResearchHarness::ClaudeCode, bytes_max),
            line: Vec::new(),
            long: None,
            unread: false,
            finished: false,
            limited: false,
        }
    }

    /// Reads bytes the run printed, any part of a line or many lines. Input: the bytes.
    /// Output: none.
    fn read(&mut self, bytes: &[u8]) {
        for part in bytes.split_inclusive(|&byte| byte == b'\n') {
            let (body, ended) = match part.strip_suffix(b"\n") {
                Some(body) => (body, true),
                None => (part, false),
            };
            if let Some(long) = self.long.as_mut() {
                long.pass(body);
            } else if self.line.len().saturating_add(body.len()) > CLAUDE_LINE_BYTES_MAX {
                let mut long = LongLine::default();
                long.pass(&self.line);
                long.pass(body);
                self.long = Some(long);
                self.line = Vec::new();
            } else {
                self.line.extend_from_slice(body);
            }
            if ended {
                self.end_line();
            }
        }
        assert!(self.line.len() <= CLAUDE_LINE_BYTES_MAX);
        assert!(self.long.is_none() || self.line.is_empty());
    }

    /// Reads the last line of a stream that ended without a line break. Output: none.
    fn close(&mut self) {
        if self.long.is_some() || !self.line.is_empty() {
            self.end_line();
        }
        assert!(self.line.is_empty());
        assert!(self.long.is_none());
    }

    /// Takes the line read so far as one event: a long line of a sub-agent is skipped, a long
    /// user line of the main thread is its tool result, and any other long line leaves the
    /// run with no answer. Output: none.
    fn end_line(&mut self) {
        match self.long.take() {
            Some(long) if long.parent == Some(true) => {}
            Some(long) if long.head.starts_with(CLAUDE_USER_LINE) => self.reply.tool(),
            Some(_) => self.unread = true,
            None => {
                if let Ok(event) = serde_json::from_slice::<serde_json::Value>(&self.line) {
                    self.event(&event);
                }
            }
        }
        self.line.clear();
        assert!(self.long.is_none());
    }

    /// Takes one event of the run. Input: the event. Output: none.
    fn event(&mut self, event: &serde_json::Value) {
        if !event["parent_tool_use_id"].is_null() {
            return;
        }
        let blocks = || event["message"]["content"].as_array().into_iter().flatten();
        match event["type"].as_str() {
            Some("assistant") if claude_error(event) => {
                let said = blocks().filter_map(|block| block["text"].as_str());
                self.limited |= said.chain(event["error"].as_str()).any(limit_line);
            }
            Some("assistant") => {
                for block in blocks().filter(|block| block["type"] == "text") {
                    self.reply.said(block["text"].as_str().unwrap_or_default());
                }
            }
            Some("user") if blocks().any(|block| block["type"] == "tool_result") => {
                self.reply.tool();
            }
            Some("result") => {
                self.finished = event["subtype"] == "success" && event["is_error"] == false;
                self.limited |=
                    event["is_error"] == true && event["result"].as_str().is_some_and(limit_line);
            }
            _ => {}
        }
    }

    /// The answer the run ended with. Output: the reply, or `None` while the stream holds
    /// no successful result, a line it could not read, or a reply past its bound.
    fn answer(&self) -> Option<&str> {
        assert!(self.line.is_empty());
        if self.finished && !self.unread {
            self.reply.text()
        } else {
            None
        }
    }

    /// Whether Claude Code ended the run on a rate or usage limit. Output: the answer.
    fn limited(&self) -> bool {
        assert!(self.line.is_empty());
        self.limited
    }
}

/// What the engine keeps of a Claude Code line past [`CLAUDE_LINE_BYTES_MAX`] as its bytes
/// pass: how it begins, its last bytes, and whether its [`CLAUDE_PARENT_KEY`] names a
/// sub-agent (`Some(true)`) or the main thread (`Some(false)`, a `null` value), once seen.
#[derive(Default)]
struct LongLine {
    head: Vec<u8>,
    tail: Vec<u8>,
    parent: Option<bool>,
}

impl LongLine {
    /// Takes the next bytes of the line. Input: the bytes. Output: none.
    fn pass(&mut self, bytes: &[u8]) {
        let room = CLAUDE_USER_LINE.len().saturating_sub(self.head.len());
        self.head.extend_from_slice(&bytes[..room.min(bytes.len())]);
        if self.parent.is_none() {
            let reach = bytes.len().min(CLAUDE_PARENT_KEY.len());
            let joint: Vec<u8> = self.tail.iter().chain(&bytes[..reach]).copied().collect();
            self.parent = parent_named(&joint).or_else(|| parent_named(bytes));
        }
        self.tail
            .extend_from_slice(&bytes[bytes.len().saturating_sub(CLAUDE_PARENT_KEY.len())..]);
        let cut = self.tail.len().saturating_sub(CLAUDE_PARENT_KEY.len());
        self.tail.drain(..cut);
        assert!(self.head.len() <= CLAUDE_USER_LINE.len());
        assert!(self.tail.len() <= CLAUDE_PARENT_KEY.len());
    }
}

/// The first [`CLAUDE_PARENT_KEY`] in raw line bytes that its value's first byte follows.
/// Input: the bytes. Output: true for a sub-agent, false for `null`, or `None` when no key
/// with a value byte is in them.
fn parent_named(bytes: &[u8]) -> Option<bool> {
    let parent = bytes
        .windows(CLAUDE_PARENT_KEY.len() + 1)
        .filter(|window| window.starts_with(CLAUDE_PARENT_KEY))
        .find_map(|window| match window[CLAUDE_PARENT_KEY.len()] {
            b'"' => Some(true),
            b'n' => Some(false),
            _ => None,
        });
    assert!(parent.is_none() || bytes.len() > CLAUDE_PARENT_KEY.len());
    parent
}

/// Whether a message's first line that holds words names a rate or usage limit. Input:
/// the message. Output: the answer.
fn limit_line(message: &str) -> bool {
    message
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(rate_limited)
}

/// The report bound as a byte count a [`Reply`] can hold. Input: the bound. Output: the count.
fn report_bound(bytes_max: u64) -> usize {
    usize::try_from(bytes_max).unwrap_or(usize::MAX)
}

/// The whole answer of a Claude Code run in its JSON event lines, read by [`ClaudeStream`],
/// so an answer Claude Code continued past the output cap comes back whole, where `--print`
/// text keeps only the last part (DEC-160). Input: the event lines. Output: the answer, or
/// `None` while the stream ends in no successful result, such as a log cut at its bound or
/// a run Claude Code ended on an error.
#[cfg(unix)]
fn claude_answer(events: &str) -> Option<String> {
    let mut stream = ClaudeStream::new(events.len());
    stream.read(events.as_bytes());
    stream.close();
    let answer = stream.answer().map(str::to_owned);
    assert!(
        answer
            .as_ref()
            .is_none_or(|answer| answer.len() <= events.len())
    );
    answer
}

/// Whether one assistant event of a Claude Code stream is Claude Code's own error message,
/// such as the one it ends a run with when an answer is cut once more after its last
/// continue request, rather than the model's words. Input: the event. Output: the answer.
fn claude_error(event: &serde_json::Value) -> bool {
    assert!(event["type"] == "assistant");
    event["is_api_error_message"] == true || event["message"]["model"] == CLAUDE_OWN_MODEL
}

/// The plain reason a read-only agent stopped with an error: Codex's own
/// message from its last `turn.failed` event, else its last `error` event,
/// else the generic reason; a message that is a provider's JSON error body
/// gives its inner message. Takes the agent's stdout log, its harness and
/// the log bound; the log is read bounded and never through a link.
fn agent_failure(log: &Path, harness: ResearchHarness, max: u64) -> String {
    const GENERIC: &str = "Agent stopped with an error";
    if harness != ResearchHarness::Codex {
        return GENERIC.to_owned();
    }
    let Some(Ok(text)) = log.parent().map(|agent| read_bounded(agent, log, max)) else {
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

/// Reads a report or brief an agent may have replaced, opened from a folder the engine
/// holds with no link followed in any part below it and no FIFO waited on, so an agent
/// that swapped its temp folder for a link is refused; the file is kept only when regular
/// and within `max` bytes. Inputs: the engine-held folder, the path in it and the bound.
/// Output: the text or the I/O error.
fn read_bounded(base: &Path, path: &Path, max: u64) -> io::Result<String> {
    let below = path
        .strip_prefix(base)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let file = crate::platform::open_plain_beneath(base, below)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > max {
        return Err(io::Error::other("report is not a bounded regular file"));
    }
    let mut text = String::new();
    file.take(max).read_to_string(&mut text)?;
    Ok(text)
}

/// Builds a research job's prompt: the planning prompt with the linked research files for
/// the planning job, else the brief, the owner's attached documents, the kind's task and
/// the report rules, the owner questions block among them. Inputs: the project folder, the
/// job kind and the linked research files. Output: the prompt, or the read failure.
fn research_prompt(
    root: &Path,
    kind: &str,
    linked: &[(ResearchKind, LinkedText)],
) -> Result<String, RunError> {
    if kind == ResearchKind::RoadmapPlanning.name() {
        let state = crate::projects_home::state_folder(root);
        return Ok(crate::planning::prompt::prompt_with_links(&state, linked)?);
    }
    let context = crate::projects_home::state_folder(root).join("context");
    let brief = match read_bounded(&context, &context.join("brief.md"), BRIEF_BYTES_MAX) {
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
    prompt.push_str(QUESTIONS_RULE);
    assert!(prompt.ends_with('\n'));
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
        "roadmap_planning" => ResearchKind::RoadmapPlanning.label().into(),
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

/// The job failure when a report's questions could not be logged.
const REPORT_QUESTIONS_UNLOGGED: &str = "Could not log the report's questions for the owner";
/// The refusal of a planning run whose project's linked files cannot be read.
const LINKS_UNREADABLE: &str = "linked research files unreadable";
/// The report rule asking a research agent for the decisions only the owner can make, in
/// the shape every engine question has.
const QUESTIONS_RULE: &str = "- When a decision needs the owner, add one fenced json block after that section and nothing after it: {\"questions\":[{\"question\":\"...\",\"kind\":\"single\",\"options\":[\"...\",\"...\"],\"recommended\":\"...\"}]}. kind is single or multi with 2 to 6 short options and one of them as recommended, or text with no options only when no short list of answers fits. Leave the block out when nothing needs the owner.\n";

/// Takes the owner questions out of a research report: its last fenced JSON block holding a
/// `questions` list is cut from the text and each question in it is checked as the questions
/// log checks it, with where it came from as its context. Inputs: the agent's report, the
/// job's plain label and the report's path inside the project. Output: the report without
/// the block and the questions, the report as it came when it holds no such block.
fn report_questions(body: &str, label: &str, path: &str) -> (String, Vec<crate::questions::Ask>) {
    let Some((block, value)) = crate::planning::question_block(body) else {
        return (body.to_owned(), Vec::new());
    };
    let context = format!(
        "Raised by the {} job. Its report is in {path}.",
        label.to_lowercase()
    );
    let asks = crate::questions::asks_from_json(&value, &context);
    let before = body[..block.start].trim_end();
    let after = body[block.end..].trim_start();
    let gap = if before.is_empty() || after.is_empty() {
        ""
    } else {
        "\n\n"
    };
    let report = format!("{before}{gap}{after}");
    assert!(report.len() <= body.len());
    assert!(asks.len() <= crate::questions::ASKS_FROM_ONE_REPLY_MAX);
    (report, asks)
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

/// Formats an instant as a UTC minute for plain status text. Takes the instant.
/// Returns `YYYY-MM-DD HH:MM UTC`; an instant before the epoch reads as the epoch.
pub(crate) fn utc_date(time: SystemTime) -> String {
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
    use crate::harnesses::launch::CLAUDE_NO_TOOLS_PROMPT;
    use crate::protocol::{ConfigId, CredentialRef, Cursor};
    use std::os::unix::fs::PermissionsExt;

    /// A Claude Code research agent with no signed-in account handed to it gets one agent
    /// keychain of its own as its home's default and search list, and a Codex one gets no
    /// keychain folder at all (DEC-146 r1 KCA-M2).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_claude_research_agent_without_a_lease_gets_its_own_default_keychain() {
        use crate::credentials::test_keychain::TestKeychain;
        let dir = tempfile::Builder::new()
            .prefix("ca-research-keychain-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let state = dir.path().canonicalize().unwrap().join("research");
        let homes = [state.join("claude/home"), state.join("codex/home")];
        for home in &homes {
            std::fs::create_dir_all(home).unwrap();
            std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        endpoint_keychain(&state, ResearchHarness::Codex, &homes[1]).unwrap();
        assert!(!homes[1].join("Library").exists());
        endpoint_keychain(&state, ResearchHarness::ClaudeCode, &homes[0]).unwrap();
        let found: Vec<PathBuf> = fs::read_dir(homes[0].join("Library/Keychains"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_str().unwrap().ends_with(".keychain-db"))
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0]
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("agent-")
        );
        let keychain = TestKeychain::adopt(&homes[0], found[0].clone());
        let text = found[0].to_str().unwrap();
        assert!(keychain.setting("default-keychain").contains(text));
        assert!(keychain.setting("list-keychains").contains(text));
        assert!(
            endpoint_keychain(&state, ResearchHarness::ClaudeCode, dir.path()).is_err(),
            "a home outside the research state got a keychain"
        );
    }

    #[test]
    fn a_research_job_network_follows_the_project_choice_only_with_tools() {
        use crate::sandbox::policy::{AgentNetwork, AgentPolicy};
        let model = SandboxRemote::AnyHost(8443);
        let profile = |choice: AgentNetwork, tools: bool| {
            let policy = AgentPolicy {
                network: choice,
                ..AgentPolicy::default()
            };
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path().canonicalize().unwrap();
            let make = |name: &str| {
                let folder = base.join(name);
                std::fs::create_dir(&folder).unwrap();
                folder
            };
            ProjectSandbox::new(
                &make("project"),
                &make("home"),
                &make("tmp"),
                job_network(&policy, tools, model),
            )
            .unwrap()
            .profile()
        };
        let off = profile(AgentNetwork::Open, false);
        assert!(off.contains("(remote ip \"*:8443\")"), "{off}");
        assert!(!off.contains("(remote ip \"*:*\")"), "{off}");
        let open = profile(AgentNetwork::Open, true);
        assert!(open.contains("(remote ip \"*:*\")"), "{open}");
        let https = profile(AgentNetwork::Https, true);
        assert!(https.contains("(remote ip \"*:443\")"), "{https}");
        assert!(!https.contains("(remote ip \"*:*\")"), "{https}");
        let none = profile(AgentNetwork::None, true);
        assert!(!none.contains("(remote ip \"*:"), "{none}");
        assert!(!none.contains("mDNSResponder"), "{none}");
    }

    /// OWNER-PORTS-FAIL-OPEN r1 L2: a tools-on job whose runner's settings files do not
    /// read is refused as Invalid instead of starting with the owner's model ports unknown;
    /// a tools-off job keeps no ports either way, as do settings files not made yet.
    #[test]
    fn a_tools_on_job_whose_settings_do_not_read_is_refused() {
        let folder = tempfile::tempdir().unwrap();
        let settings = (
            folder.path().join("global.toml"),
            folder.path().join("project.toml"),
        );
        assert_eq!(job_kept_ports(None, true).unwrap(), Vec::<u16>::new());
        assert_eq!(
            job_kept_ports(Some(&settings), true).unwrap(),
            Vec::<u16>::new()
        );
        std::fs::write(&settings.0, "[[[\n").unwrap();
        assert!(matches!(
            job_kept_ports(Some(&settings), true),
            Err(RunError::Invalid(reason)) if reason.contains("do not read")
        ));
        assert_eq!(
            job_kept_ports(Some(&settings), false).unwrap(),
            Vec::<u16>::new()
        );
    }

    /// A research job's Claude Code pane gets the display the owner chose for the project's
    /// terminals (N-D9): Classic while none is saved, Fullscreen once chosen, and a project
    /// without settings stays Classic.
    #[test]
    fn a_research_job_reads_the_claude_code_display_of_its_project() {
        use crate::coord::settings::{CoordSettings, TerminalDisplay, write};
        use std::os::unix::fs::PermissionsExt;
        let folder = tempfile::tempdir().unwrap();
        let database = folder.path().join("engine.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
        let project = ProjectId::new("research-display").unwrap();
        connection
            .execute(
                "INSERT INTO projects(id, body) VALUES (?1, '{}')",
                [project.as_str()],
            )
            .unwrap();
        let jobs = ResearchJobs::open(&database).unwrap();
        assert_eq!(
            jobs.terminal_display(&project).unwrap(),
            TerminalDisplay::Classic
        );
        write(
            &connection,
            &project,
            &CoordSettings {
                terminal_display: TerminalDisplay::Fullscreen,
                ..CoordSettings::default()
            },
        )
        .unwrap();
        assert_eq!(
            jobs.terminal_display(&project).unwrap(),
            TerminalDisplay::Fullscreen
        );
        let other = ProjectId::new("research-elsewhere").unwrap();
        assert_eq!(
            jobs.terminal_display(&other).unwrap(),
            TerminalDisplay::Classic
        );
    }

    /// A research job with tools under None reaches its model only through a model proxy
    /// for its account's hosts; a job with tools off, or on a loopback endpoint, reaches its
    /// model directly, and an endpoint no proxy can name is refused (S4B-M1).
    #[test]
    fn a_research_job_with_tools_under_no_network_reaches_its_model_only_through_a_model_proxy() {
        use crate::sandbox::policy::{AgentNetwork, AgentPolicy};
        use std::io::{BufRead, Write};
        let none = AgentPolicy {
            network: AgentNetwork::None,
            ..AgentPolicy::default()
        };
        let account = ResearchAccount::ManagedCodex {
            connection: codex_connection(),
            model: None,
        };
        let (network, proxy) = job_reach(&none, true, &account, 41, &[]).unwrap();
        assert!(
            proxy.is_some(),
            "a tools-on job under None gets a model proxy, not {network:?}"
        );
        let port = proxy.as_ref().unwrap().port();
        assert_eq!(
            network,
            SandboxNetwork::Only(vec![SandboxRemote::Loopback(port)])
        );
        for (target, status) in [
            ("chatgpt.com:443", "HTTP/1.1 200 Connection Established"),
            ("api.openai.com:443", "HTTP/1.1 200 Connection Established"),
            ("example.com:443", "HTTP/1.1 403 Forbidden"),
        ] {
            let mut client = crate::spawn_gate::connect_tcp_std(("127.0.0.1", port)).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            client
                .write_all(
                    format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes(),
                )
                .unwrap();
            let mut line = String::new();
            std::io::BufReader::new(client)
                .read_line(&mut line)
                .unwrap();
            assert_eq!(line.trim_end(), status, "{target}");
        }
        let (off, unproxied) = job_reach(&none, false, &account, 41, &[]).unwrap();
        assert!(unproxied.is_none());
        assert_eq!(off, SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]));
        let local = ResearchAccount::Endpoint {
            base_url: "http://localhost:11434/v1".into(),
            model: "qwen2.5:0.5b".into(),
            key: None,
        };
        let (direct, unproxied) = job_reach(&none, true, &local, 41, &[]).unwrap();
        assert!(unproxied.is_none());
        assert_eq!(
            direct,
            SandboxNetwork::Only(vec![SandboxRemote::Loopback(11_434)])
        );
        let unnamed = ResearchAccount::Endpoint {
            base_url: "http://203.0.113.9:8080/v1".into(),
            model: "qwen2.5:0.5b".into(),
            key: None,
        };
        assert!(matches!(
            job_reach(&none, true, &unnamed, 41, &[]),
            Err(RunError::Invalid(_))
        ));
    }

    #[test]
    fn linux_and_windows_refuse_team_agents_and_research_with_a_plain_line() {
        assert_eq!(
            confined_unavailable("linux"),
            Some("Team agents and research are not available on Linux in this version.")
        );
        assert_eq!(
            confined_unavailable("windows"),
            Some("Team agents and research are not available on Windows in this version.")
        );
        assert_eq!(confined_unavailable("macos"), None);
    }

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

    /// Records `id` as a project of the engine database with `folder` as its folder, so a
    /// research run reads the project's launch context. Inputs: the database, the project
    /// id and its folder.
    #[cfg(target_os = "macos")]
    fn register_project(database: &Path, id: &str, folder: &Path) {
        let connection = rusqlite::Connection::open(database).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        let project = ProjectId::new(id).unwrap();
        let setup = crate::project_setup::StoredSetup {
            project_id: project.clone(),
            name: id.into(),
            about: String::new(),
            tech_stack: vec![],
            platforms: vec![],
            research: vec![],
            pool: None,
            team: crate::project_setup::TeamPlan {
                master: "codex".into(),
                master_tools: false,
                sub_harnesses: vec![],
                min_agents: 1,
                max_agents: 1,
            },
            documents: vec![],
            folder: folder.to_str().unwrap().into(),
            created_ms: 1,
            revision: 1,
        };
        let row = crate::protocol::StoredProject {
            project_id: project,
            spec: crate::protocol::ProjectSpec {
                name: id.into(),
                repositories: vec![],
            },
        };
        connection
            .execute(
                "INSERT INTO projects VALUES (?1,?2)",
                rusqlite::params![id, serde_json::to_string(&row).unwrap()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project_setups VALUES (?1,?2)",
                rusqlite::params![id, serde_json::to_string(&setup).unwrap()],
            )
            .unwrap();
    }

    /// A run that waits for its repository's setup lock longer than the stale limit keeps
    /// its claim: the wait is heartbeated, so stale recovery neither takes the job back nor
    /// leaves the run setting up a worktree for a job it lost (finding_40456ca5a72c5454).
    #[test]
    #[cfg(target_os = "macos")]
    fn a_run_waiting_for_its_repository_setup_keeps_its_claim() {
        let root = engine_root();
        let folder = root.base.join("repo");
        fs::create_dir(&folder).unwrap();
        repository_git(&folder, &["init", "-q"]);
        repository_git(&folder, &["commit", "-q", "--allow-empty", "-m", "Start"]);
        register_project(&root.database, "alpha", &folder);
        let limits = ResearchLimits {
            wall_time: Duration::from_secs(10),
            ..ResearchLimits::default()
        };
        let runner = ResearchRunner::new(
            &root.database,
            &root.state,
            Path::new("/usr/bin/true"),
            limits,
        )
        .unwrap();
        let jobs = ResearchJobs::open(&root.database).unwrap();
        let id = jobs
            .enqueue("alpha", "market_research", "codex", false)
            .unwrap();
        let project = ResearchProject {
            id: "alpha",
            root: &folder,
            account: Some(ResearchAccount::Endpoint {
                base_url: "http://127.0.0.1:9/v1".into(),
                model: "local-model".into(),
                key: None,
            }),
        };
        let stale = Duration::from_millis(1500);
        let held = repository_setup(&folder);
        let (waiting, recovered) = std::thread::scope(|scope| {
            let run = scope.spawn(|| runner.run_once(&project));
            let started = Instant::now();
            while jobs.get(id).unwrap().unwrap().status != ResearchStatus::Running
                && started.elapsed() < Duration::from_secs(30)
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(stale * 2);
            let waiting = !run.is_finished();
            let recovered = jobs.recover_stale(stale.as_millis() as u64, 2).unwrap();
            drop(held);
            let _ = run.join();
            (waiting, recovered)
        });
        assert!(waiting, "the run never waited for its repository setup");
        assert_eq!(
            recovered, 0,
            "a run waiting for its repository setup lost its claim as stale"
        );
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

    /// A terminal host that lists the panes it was given and fails every other call, or
    /// fails its listing too, as a tmux server that cannot answer.
    struct Listed(Result<Vec<AgentId>, crate::terminal::HostError>);

    impl Host for Listed {
        fn spawn(
            &self,
            _agent: &AgentId,
            _command: &crate::harnesses::interactive::InteractiveCommand,
        ) -> Result<crate::terminal::PaneRef, crate::terminal::HostError> {
            Err(crate::terminal::HostError("terminal_unavailable".into()))
        }

        fn attach(
            &self,
            _agent: &AgentId,
        ) -> Result<crate::terminal::TerminalStream, crate::terminal::HostError> {
            Err(crate::terminal::HostError("terminal_unavailable".into()))
        }

        fn input(&self, _agent: &AgentId, _bytes: &[u8]) -> Result<(), crate::terminal::HostError> {
            Err(crate::terminal::HostError("terminal_unavailable".into()))
        }

        fn resize(
            &self,
            _agent: &AgentId,
            _cols: u16,
            _rows: u16,
        ) -> Result<(), crate::terminal::HostError> {
            Err(crate::terminal::HostError("terminal_unavailable".into()))
        }

        fn kill(&self, _agent: &AgentId) -> Result<(), crate::terminal::HostError> {
            Err(crate::terminal::HostError("terminal_agent_unknown".into()))
        }

        fn alive(&self) -> Result<Vec<AgentId>, crate::terminal::HostError> {
            self.0.clone()
        }
    }

    /// On a fresh Mac the engine starts before setup installs tmux. Research agents then run
    /// headless only until tmux is found: the runner looks for the terminal again at each
    /// agent's start, keeps the first host that answers and asks no more, and a host that
    /// cannot list its panes is not kept, so the next agent looks again.
    #[test]
    fn a_terminal_found_after_engine_start_runs_the_next_research_agent_in_a_pane() {
        use std::sync::atomic::AtomicUsize;
        let root = engine_root();
        let found: Arc<Mutex<Option<Arc<dyn Host>>>> = Arc::new(Mutex::new(None));
        let asked = Arc::new(AtomicUsize::new(0));
        let probe = {
            let found = found.clone();
            let asked = asked.clone();
            Box::new(move || {
                asked.fetch_add(1, Ordering::SeqCst);
                found.lock().unwrap().clone()
            })
        };
        let runner =
            ResearchRunner::without_codex(&root.database, &root.state, ResearchLimits::default())
                .unwrap()
                .with_terminal_probe(probe);
        assert!(runner.terminal().is_none());
        assert!(runner.terminal().is_none());
        assert_eq!(
            asked.load(Ordering::SeqCst),
            2,
            "each agent's start looks again"
        );
        let broken: Arc<dyn Host> = Arc::new(Listed(Err(crate::terminal::HostError(
            "terminal_unavailable".into(),
        ))));
        *found.lock().unwrap() = Some(broken);
        assert!(
            runner.terminal().is_none(),
            "a host that cannot answer is not kept"
        );
        assert_eq!(asked.load(Ordering::SeqCst), 3);
        let host: Arc<dyn Host> = Arc::new(Listed(Ok(vec![AgentId::new("team-7").unwrap()])));
        *found.lock().unwrap() = Some(host.clone());
        let first = runner
            .terminal()
            .expect("tmux installed after start is found");
        assert!(Arc::ptr_eq(&first, &host));
        *found.lock().unwrap() = None;
        let kept = runner.terminal().expect("the found host is kept");
        assert!(Arc::ptr_eq(&kept, &host));
        assert_eq!(
            asked.load(Ordering::SeqCst),
            4,
            "a kept host is not looked for again"
        );
    }

    /// An npm-built harness asks `env` for its interpreter, and a research agent runs with
    /// no search path that finds Node from nvm, volta or Homebrew. Its launch words start
    /// the interpreter the tool resolver finds, by its absolute file, then the script; a
    /// native harness is one word. The stand-in asks for `sh`, found on every Mac.
    #[test]
    fn a_research_harness_script_starts_through_its_resolved_interpreter() {
        let install = tempfile::tempdir().unwrap();
        let base = install.path().canonicalize().unwrap();
        let script = base.join("lib/node_modules/@openai/codex/bin/codex.js");
        fs::create_dir_all(script.parent().unwrap()).unwrap();
        fs::write(&script, "#!/usr/bin/env sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let interpreter =
            crate::tools::resolve("sh", &crate::tools::Places::from_environment(None)).unwrap();
        assert_eq!(
            harness_words(&script).unwrap(),
            vec![
                interpreter.program.to_str().unwrap().to_owned(),
                script.to_str().unwrap().to_owned()
            ]
        );
        let native = base.join("codex");
        fs::write(&native, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&native, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            harness_words(&native).unwrap(),
            vec![native.to_str().unwrap().to_owned()]
        );
    }

    /// A research harness whose file sits inside a project, where agents can write, gets
    /// no launch words and no sandbox grant: both answer with the tool resolver's owner line
    /// before anything runs.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_research_harness_inside_a_project_is_refused_before_any_grant() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let project = base.join("project");
        let home = base.join("agent").join("home");
        let tmp = base.join("agent").join("tmp");
        for folder in [&project, &home, &tmp] {
            fs::create_dir_all(folder).unwrap();
        }
        let harness = base.join("installed").join("project");
        fs::create_dir_all(harness.join(".crazyagent")).unwrap();
        let program = harness.join("bin").join("codex");
        fs::create_dir_all(program.parent().unwrap()).unwrap();
        fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let line =
            crate::tools::resolve_entry(&program, &crate::tools::Places::from_environment(None))
                .unwrap_err()
                .line();
        assert!(matches!(harness_words(&program), Err(RunError::Invalid(code)) if code == line));
        let sandbox =
            ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new())).unwrap();
        assert!(matches!(
            allow_harness(sandbox, &program),
            Err(RunError::Invalid(code)) if code == line
        ));
    }

    #[test]
    fn a_harness_installed_after_engine_start_can_run_only_through_the_engines_lookup() {
        let root = engine_root();
        let codex = root.base.join("bin").join("codex");
        let runner =
            ResearchRunner::without_codex(&root.database, &root.state, ResearchLimits::default())
                .unwrap();
        assert!(!runner.can_run(ResearchHarness::Codex));
        let pinned = codex.clone();
        let runner = runner.with_harness_lookup(Arc::new(move |harness: &str| {
            (harness == "codex").then(|| pinned.clone())
        }));
        assert!(runner.can_run(ResearchHarness::Codex));
        assert_eq!(runner.harness(ResearchHarness::Codex), Some(codex));
        assert!(!runner.can_run(ResearchHarness::ClaudeCode));
    }

    /// A loopback provider that refuses connections is named at once with its host and port
    /// and the way out, for either endpoint kind; one that listens and a remote endpoint are
    /// never named, and no check waits on a remote host.
    #[test]
    fn a_loopback_provider_that_refuses_connections_is_named_at_once_and_a_listening_one_is_not() {
        let (_held, port) = crate::spawn_gate::refusing_tcp_port().unwrap();
        let open = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
        let listening = open.local_addr().unwrap().port();
        let codex = |base_url: String| ResearchAccount::Endpoint {
            base_url,
            model: "local-model".into(),
            key: None,
        };
        let claude = |base_url: String| ResearchAccount::AnthropicEndpoint {
            base_url,
            model: "local-model".into(),
            key: None,
        };
        let line = format!(
            "Can't reach the model provider at 127.0.0.1:{port}. Start it or pick another account in this pool, then try again."
        );
        let started = Instant::now();
        assert_eq!(
            unreached_provider(&codex(format!("http://127.0.0.1:{port}/v1"))),
            Some(line.clone())
        );
        assert_eq!(
            unreached_provider(&claude(format!("http://127.0.0.1:{port}"))),
            Some(line)
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(
            unreached_provider(&codex(format!("http://127.0.0.1:{listening}/v1"))),
            None
        );
        assert_eq!(
            unreached_provider(&claude(format!("http://localhost:{listening}"))),
            None
        );
        let started = Instant::now();
        assert_eq!(
            unreached_provider(&codex("https://models.example.test/v1".into())),
            None
        );
        assert!(started.elapsed() < Duration::from_millis(500));
        drop(open);
    }

    /// The reach check every read-only run's caller asks names a refusing loopback provider
    /// only where this OS runs confined model runs; elsewhere the run's own unavailable line
    /// keeps its place.
    #[test]
    fn the_reach_check_here_names_a_refusing_provider_only_where_confined_runs_exist() {
        let (_held, port) = crate::spawn_gate::refusing_tcp_port().unwrap();
        let account = ResearchAccount::Endpoint {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "local-model".into(),
            key: None,
        };
        let line = format!(
            "Can't reach the model provider at 127.0.0.1:{port}. Start it or pick another account in this pool, then try again."
        );
        assert_eq!(
            unreached_provider_here(&account),
            confined_unavailable(std::env::consts::OS)
                .is_none()
                .then_some(line)
        );
    }

    /// MR-L1: only a refused connect proves nothing listens on a loopback provider. A
    /// success, a connect that ran out of time, too many open files or any other error never
    /// does, so such a mapping step still runs its try as before.
    #[test]
    fn only_a_refused_connect_counts_as_an_unreached_provider() {
        use std::io::{Error, ErrorKind};
        assert!(refused(Err(Error::from(ErrorKind::ConnectionRefused))));
        assert!(!refused(Ok(())));
        assert!(!refused(Err(Error::from(ErrorKind::TimedOut))));
        assert!(!refused(Err(Error::from_raw_os_error(libc::EMFILE))));
        assert!(!refused(Err(Error::from(ErrorKind::AddrNotAvailable))));
        assert!(!refused(Err(Error::from(ErrorKind::PermissionDenied))));
    }

    /// REACH-FLAKE: the port a reach row names for a down provider stays in use while the row
    /// runs, so the system gives it to no port-0 bind and refuses a bind that names it without
    /// `SO_REUSEADDR`, and every connect to it is refused at once: the row never finds a
    /// provider it did not start on that port.
    #[test]
    fn a_port_kept_for_a_down_provider_refuses_connects_and_no_plain_bind_takes_it() {
        let (_held, port) = crate::spawn_gate::refusing_tcp_port().unwrap();
        let address = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port));
        let taken = crate::spawn_gate::bind_tcp_std_exclusive(address);
        assert_eq!(
            taken.as_ref().map(drop).map_err(std::io::Error::kind),
            Err(std::io::ErrorKind::AddrInUse),
            "a plain bind took the port the row named for a down provider"
        );
        assert!(refused(
            crate::spawn_gate::connect_tcp_std_timeout(&address, PROVIDER_REACH_TIME).map(drop)
        ));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_read_only_run_counts_under_its_project_even_on_an_endpoint_account() {
        let root = engine_root();
        let runner =
            ResearchRunner::without_codex(&root.database, &root.state, ResearchLimits::default())
                .unwrap();
        let folder = root.base.join("repo");
        fs::create_dir(&folder).unwrap();
        let account = ResearchAccount::Endpoint {
            base_url: "http://127.0.0.1:9/v1".into(),
            model: "local-model".into(),
            key: None,
        };
        let project = ProjectId::new("alpha").unwrap();
        let run = ReadOnlyRun {
            name: "onboard-m-alpha-1",
            agent: "onboarding",
            project: Some(&project),
            folder: &folder,
            prompt: "Map this repository.",
            account: &account,
            wall_time: Duration::from_secs(5),
            output_tokens: None,
        };
        assert!(matches!(
            runner.run_read_only(&run, &|| false),
            Err(RunError::Invalid(_))
        ));
        let db = crate::usage::open(root.database.parent().unwrap()).unwrap();
        let row: (String, String, String, String, String) = db
            .query_row(
                "SELECT home,project,account,agent,harness FROM usage_runs",
                [],
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
            .unwrap();
        let home = root.state.join("agents/onboard-m-alpha-1/home");
        assert_eq!(row.0, home.to_string_lossy());
        assert_eq!(
            (
                row.1.as_str(),
                row.2.as_str(),
                row.3.as_str(),
                row.4.as_str()
            ),
            ("alpha", "", "onboarding", "codex")
        );
    }

    /// DEC-147.9: a read-only run on a project folder runs under the secret deny of that
    /// folder, with the secret the owner shared back readable again and nothing else.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_read_only_run_keeps_the_project_secrets_closed_but_the_shared_back_ones() {
        let root = engine_root();
        let codex = root.base.join("bin").join("codex");
        fs::create_dir_all(codex.parent().unwrap()).unwrap();
        fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
        let pinned = codex.clone();
        let runner =
            ResearchRunner::without_codex(&root.database, &root.state, ResearchLimits::default())
                .unwrap()
                .with_harness_lookup(Arc::new(move |harness: &str| {
                    (harness == "codex").then(|| pinned.clone())
                }));
        let folder = root.base.join("repo");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join(".env"), "TOKEN=1\n").unwrap();
        let project = ProjectId::new("alpha").unwrap();
        let connection = rusqlite::Connection::open(&root.database).unwrap();
        let policy = crate::sandbox::policy::AgentPolicy {
            shared_folders: crate::sandbox::shared_folders::SharedFolders {
                secrets: vec![".env".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        crate::sandbox::policy::save(&connection, project.as_str(), &policy).unwrap();
        drop(connection);
        let account = ResearchAccount::Endpoint {
            base_url: "http://127.0.0.1:9/v1".into(),
            model: "local-model".into(),
            key: None,
        };
        let run = ReadOnlyRun {
            name: "overview-alpha-1",
            agent: "overview",
            project: Some(&project),
            folder: &folder,
            prompt: "Describe this project.",
            account: &account,
            wall_time: Duration::from_secs(5),
            output_tokens: None,
        };
        let _ = runner.run_read_only(&run, &|| false);
        let profile = fs::read_to_string(root.state.join("profiles").join("overview-alpha-1.sb"))
            .expect("the run installed its profile");
        assert!(
            profile.contains(&format!(
                "(require-not (subpath \"{}/.git\"))",
                folder.display()
            )),
            "{profile}"
        );
        assert!(
            profile.contains(&format!(
                "(allow file-read-data file-read-xattr\n  (subpath \"{}\")",
                folder.join(".env").display()
            )),
            "{profile}"
        );
    }

    /// L4 r1 H1: a read-only run follows the project's shared folders choice too. Under
    /// Chosen an untracked local folder not picked stays closed, and a saved Read-write never
    /// opens the picked one for writes, since a read-only run only reads.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_read_only_run_keeps_the_folders_the_project_does_not_share_closed() {
        let root = engine_root();
        let codex = root.base.join("bin").join("codex");
        fs::create_dir_all(codex.parent().unwrap()).unwrap();
        fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
        let pinned = codex.clone();
        let runner =
            ResearchRunner::without_codex(&root.database, &root.state, ResearchLimits::default())
                .unwrap()
                .with_harness_lookup(Arc::new(move |harness: &str| {
                    (harness == "codex").then(|| pinned.clone())
                }));
        let folder = root.base.join("repo");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("README.md"), "readme\n").unwrap();
        repository_git(&folder, &["init", "-q"]);
        repository_git(&folder, &["add", "README.md"]);
        repository_git(&folder, &["commit", "-q", "-m", "Start"]);
        for name in ["notes", "drafts"] {
            fs::create_dir(folder.join(name)).unwrap();
            fs::write(folder.join(name).join("plan.txt"), "plan\n").unwrap();
        }
        let project = ProjectId::new("alpha").unwrap();
        let connection = rusqlite::Connection::open(&root.database).unwrap();
        let policy = crate::sandbox::policy::AgentPolicy {
            shared_folders: crate::sandbox::shared_folders::SharedFolders {
                scope: crate::sandbox::shared_folders::SharedScope::Chosen,
                access: crate::sandbox::shared_folders::SharedAccess::ReadWrite,
                chosen: vec!["notes".into()],
                secrets: Vec::new(),
            },
            ..Default::default()
        };
        crate::sandbox::policy::save(&connection, project.as_str(), &policy).unwrap();
        drop(connection);
        let account = ResearchAccount::Endpoint {
            base_url: "http://127.0.0.1:9/v1".into(),
            model: "local-model".into(),
            key: None,
        };
        let run = ReadOnlyRun {
            name: "overview-alpha-2",
            agent: "overview",
            project: Some(&project),
            folder: &folder,
            prompt: "Describe this project.",
            account: &account,
            wall_time: Duration::from_secs(5),
            output_tokens: None,
        };
        let _ = runner.run_read_only(&run, &|| false);
        let profile = fs::read_to_string(root.state.join("profiles").join("overview-alpha-2.sb"))
            .expect("the run installed its profile");
        assert!(
            profile.contains(&format!(
                "(subpath \"{}\")",
                folder.join("drafts").display()
            )),
            "{profile}"
        );
        let writes = profile
            .split("(allow file-write*")
            .nth(1)
            .and_then(|rest| rest.split("))\n").next())
            .expect("the profile allows its own writes");
        assert!(!writes.contains(&folder.display().to_string()), "{writes}");
        assert!(
            !profile.contains(&format!(
                "(deny file-write-unlink\n  (require-all\n    (subpath \"{}\")",
                folder.join("notes").display()
            )),
            "{profile}"
        );
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

    #[test]
    fn a_research_codex_records_its_folders_untrusted() {
        let account = ResearchAccount::ManagedCodex {
            connection: codex_connection(),
            model: None,
        };
        let scratch = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
        let worktree = scratch.path().canonicalize().unwrap();
        std::fs::create_dir(worktree.join(".git")).unwrap();
        let wanted = format!(
            "projects={{\"{}\"={{trust_level=\"untrusted\"}}}}",
            worktree.display()
        );
        for tools in [false, true] {
            let mut command = Command::new("/usr/bin/true");
            codex_arguments(
                &mut command,
                &worktree,
                &worktree.join("report.md"),
                Path::new("/home"),
                tools,
                &account,
            )
            .unwrap();
            let args: Vec<&str> = command
                .get_args()
                .map(|argument| argument.to_str().unwrap())
                .collect();
            assert!(
                args.windows(2).any(|pair| pair == ["-c", wanted.as_str()]),
                "{tools}: {args:?}"
            );
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
            let agent = AgentProcess::new(child, None).unwrap();
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
        kill_group(kept_group);
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

    /// The retry line is Codex 0.164's own stderr record of a provider 429 in a real
    /// research run; ids that happen to hold 429 and other log targets never count.
    #[test]
    fn a_codex_retry_after_a_limit_counts_as_a_rate_limit_and_its_ids_never_do() {
        const RETRY: &str = r#"2026-10-26T09:47:31.137351Z  WARN session_loop{thread_id=01a1779b-98a8-72d0-97aa-dd1317b20845}:submission_dispatch{otel.name="op.dispatch.turn_input" submission.id="01a1779b-9a6e-73e0-b67d-9282fb1fdcd6" codex.op="turn_input"}:turn{otel.name="session_task.turn" thread.id=01a1779b-98a8-72d0-97aa-dd1317b20845 turn.id=01a1779b-9a6e-73e0-b67d-9282fb1fdcd6 model=gpt-6.1-sol codex.turn.reasoning_effort=low}: codex_core::responses_retry: stream disconnected - retrying sampling request (1/5 in 102.999894334s)... turn_id=01a1779b-9a6e-73e0-b67d-9282fb1fdcd6 retries=1 max_retries=5 sampling_error=exceeded retry limit, last status: 429 Too Many Requests"#;
        assert!(codex_retry_limited(RETRY));
        let other_cause = RETRY
            .replace(
                "last status: 429 Too Many Requests",
                "stream closed before response.completed",
            )
            .replace("9282fb1fdcd6", "9282fb1f4296");
        assert!(other_cause.contains("429"));
        assert!(!codex_retry_limited(&other_cause));
        assert!(!codex_retry_limited(
            &RETRY.replace("codex_core::responses_retry", "codex_core::client")
        ));
        assert!(!codex_retry_limited(""));
    }

    #[test]
    fn links_are_collected_once_without_trailing_punctuation() {
        let links = links_in(
            "See [Vet](https://vet.example/a), https://vet.example/a. and <http://b.example/x>; https://",
        );
        assert_eq!(links, vec!["https://vet.example/a", "http://b.example/x"]);
    }

    #[test]
    fn a_report_question_block_is_raised_and_left_out_of_the_report() {
        let body = "## Findings\n\nRivals charge less.\n\n```json\n{\"questions\":[{\"question\":\"Which rival matters most?\",\"kind\":\"single\",\"options\":[\"Scite\",\"Elicit\"],\"recommended\":\"Scite\"},{\"question\":\"No recommended one?\",\"kind\":\"multi\",\"options\":[\"A\",\"B\"]}]}\n```\n\n## Links and references\n\n- <https://a.example/x>\n";
        let (report, asks) = report_questions(
            body,
            "Competitor research",
            "context/research/competitor_research.md",
        );
        assert_eq!(
            asks.len(),
            1,
            "a question outside the kind rules is dropped"
        );
        assert_eq!(asks[0].kind, crate::questions::QuestionKind::Single);
        assert_eq!(asks[0].recommended.as_deref(), Some("Scite"));
        assert_eq!(
            asks[0].context.as_deref(),
            Some(
                "Raised by the competitor research job. Its report is in context/research/competitor_research.md."
            )
        );
        assert!(!report.contains("\"questions\"") && !report.contains("```"));
        assert!(report.contains("Rivals charge less."));
        assert!(report.contains("## Links and references\n\n- <https://a.example/x>"));
        let plain = "## Findings\n\n```json\n{\"prices\":[1,2]}\n```\n";
        let (kept, none) = report_questions(
            plain,
            "Market research",
            "context/research/market_research.md",
        );
        assert_eq!(kept, plain);
        assert!(none.is_empty());
    }

    #[test]
    fn the_research_prompt_asks_for_owner_questions_in_the_shared_shape() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("context")).unwrap();
        fs::write(root.join("context").join("brief.md"), "# Planner\n").unwrap();
        let prompt = research_prompt(&root, "market_research", &[]).unwrap();
        assert!(prompt.contains(
            "{\"questions\":[{\"question\":\"...\",\"kind\":\"single\",\"options\":[\"...\",\"...\"],\"recommended\":\"...\"}]}"
        ));
        assert!(
            prompt.contains(
                "single or multi with 2 to 6 short options and one of them as recommended"
            )
        );
        assert!(prompt.contains("Leave the block out when nothing needs the owner."));
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
    fn a_fifo_left_at_the_report_path_is_refused_without_waiting_for_a_writer() {
        let folder = tempfile::tempdir().unwrap();
        let report = folder.path().join("report.md");
        fs::write(&report, "Findings\n").unwrap();
        assert_eq!(
            read_bounded(folder.path(), &report, 64).unwrap(),
            "Findings\n"
        );
        let link = folder.path().join("link.md");
        std::os::unix::fs::symlink(&report, &link).unwrap();
        assert!(read_bounded(folder.path(), &link, 64).is_err());
        let fifo = folder.path().join("fifo.md");
        let made = crate::spawn_gate::status(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(&fifo)
                .stdin(std::process::Stdio::null()),
        )
        .unwrap();
        assert!(made.success());
        let (sent, read) = std::sync::mpsc::channel();
        let (base, reader) = (folder.path().to_path_buf(), fifo.clone());
        std::thread::spawn(move || {
            let _ = sent.send(read_bounded(&base, &reader, 64).map_err(|error| error.to_string()));
        });
        let answer = read.recv_timeout(Duration::from_secs(5));
        if answer.is_err() {
            let _ = OpenOptions::new()
                .write(true)
                .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
                .open(&fifo);
        }
        assert!(
            matches!(answer, Ok(Err(_))),
            "a FIFO at the report path made the research worker wait: {answer:?}"
        );
    }

    #[test]
    fn a_temp_folder_swapped_for_a_link_is_neither_read_nor_written_through() {
        let folder = tempfile::tempdir().unwrap();
        let agents = folder.path().join("agents");
        let agent = agents.join("job-1");
        let tmp = agent.join("tmp");
        fs::create_dir_all(&tmp).unwrap();
        let report = tmp.join("report.md");
        fs::write(&report, "Own findings\n").unwrap();
        assert_eq!(
            read_bounded(&agents, &report, 64).unwrap(),
            "Own findings\n"
        );
        let outside = folder.path().join("other-project");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("report.md"), "Another project's findings\n").unwrap();
        fs::rename(&tmp, agent.join("home-tmp")).unwrap();
        std::os::unix::fs::symlink(&outside, &tmp).unwrap();
        let read = read_bounded(&agents, &report, 64);
        assert!(
            read.is_err(),
            "the report was read through the swapped folder: {read:?}"
        );
        fs::remove_file(outside.join("report.md")).unwrap();
        let written = write_private(&agents, &report, b"Reply\n");
        assert!(
            written.is_err(),
            "the reply was written through the swapped folder"
        );
        assert!(!outside.join("report.md").exists());
        assert!(read_bounded(&agent, &outside.join("report.md"), 64).is_err());
        write_private(&agents, &agent.join("prompt.md"), b"Plan\n").unwrap();
        assert_eq!(
            read_bounded(&agent, &agent.join("prompt.md"), 64).unwrap(),
            "Plan\n"
        );
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
            key: None,
        };
        assert_eq!(
            model_remote(&loopback).unwrap(),
            SandboxRemote::Loopback(11434)
        );
        let remote = ResearchAccount::Endpoint {
            base_url: "https://openrouter.ai/api/v1".into(),
            model: "m".into(),
            key: None,
        };
        assert_eq!(model_remote(&remote).unwrap(), SandboxRemote::AnyHost(443));
    }

    #[cfg(target_os = "macos")]
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

    /// A team or research agent's worktree, added the way the runtime adds it, is
    /// local-only: the owner's own git pushing from it gets the plain refusal and the
    /// remote gains nothing.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_added_agent_worktree_refuses_remotes_with_the_plain_message() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let project = base.join("project");
        let home = base.join("agent").join("home");
        let tmp = base.join("agent").join("tmp");
        let profiles = base.join("profiles");
        let remote = base.join("remote.git");
        for folder in [&project, &home, &tmp, &profiles] {
            fs::create_dir_all(folder).unwrap();
        }
        ensure_repository(&project, &tmp).unwrap();
        let owner_git = |dir: &Path, args: &[&str]| {
            crate::spawn_gate::output(
                Command::new(GIT)
                    .env_clear()
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .arg("-C")
                    .arg(dir)
                    .args(args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()),
            )
            .unwrap()
        };
        let remote_path = remote.to_str().unwrap();
        assert!(
            owner_git(&base, &["init", "-q", "--bare", remote_path])
                .status
                .success()
        );
        assert!(
            owner_git(&project, &["remote", "add", "origin", remote_path])
                .status
                .success()
        );
        let sandbox =
            ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new())).unwrap();
        let profile = profiles.join("agent.sb");
        sandbox.install(&profile).unwrap();
        let worktree = project.join("worktrees").join("agent-one");
        add_worktree(&sandbox, &profile, &worktree).unwrap();
        let pushed = owner_git(&worktree, &["push", "origin", "HEAD:refs/heads/agent-work"]);
        assert!(!pushed.status.success(), "{pushed:?}");
        assert!(
            String::from_utf8_lossy(&pushed.stderr).contains(crate::worktrees::REMOTE_REFUSAL),
            "{pushed:?}"
        );
        let refs = owner_git(&remote, &["for-each-ref"]);
        assert!(refs.status.success(), "{refs:?}");
        assert!(refs.stdout.is_empty(), "{refs:?}");
    }

    /// The engine's sandboxed setup git must be the real git, not the `/usr/bin/git` xcrun
    /// shim: under the agent profile the shim's per-user-temp cache write is denied on every
    /// call and its developer-dir lookup forks xcrun, the fragile path behind the load flake.
    /// The resolved git runs `git version` under the profile with no xcrun work and so prints
    /// nothing to stderr. Skipped only where no toolchain git resolves and the fix falls back.
    /// Engine git outside any sandbox keeps the toolchain xcode-select chose; only git that
    /// runs inside an agent sandbox is the Command Line Tools git, whose helpers the agent
    /// profile reads (T3-09 r1).
    #[cfg(target_os = "macos")]
    #[test]
    fn engine_git_keeps_the_selected_toolchain_and_sandboxed_git_is_the_command_line_tools_git() {
        let found = crate::spawn_gate::output(
            Command::new("/usr/bin/xcrun")
                .args(["--find", "git"])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .unwrap();
        assert!(found.status.success());
        let selected = PathBuf::from(String::from_utf8(found.stdout).unwrap().trim());
        assert_eq!(setup_git(), selected);
        if Path::new(TOOLS_GIT).is_file() {
            assert_eq!(sandboxed_git(), Path::new(TOOLS_GIT));
            assert_eq!(agent_git_problem(), None);
        } else {
            assert_eq!(sandboxed_git(), selected);
            assert_eq!(agent_git_problem(), Some(AGENT_GIT_NEEDS_TOOLS));
        }
    }

    /// A toolchain root with no Command Line Tools git names the plain reason, and a failed
    /// setup git step reports it in place of its own code; a root that has one does not.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_toolchain_root_without_the_command_line_tools_git_names_the_reason() {
        let root = tempfile::tempdir().unwrap();
        let tools = root.path().canonicalize().unwrap();
        assert_eq!(agent_git_problem_at(&tools), Some(AGENT_GIT_NEEDS_TOOLS));
        assert_eq!(
            step_failure("worktree_add_failed", &tools),
            AGENT_GIT_NEEDS_TOOLS
        );
        let bin = tools.join("usr").join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("git"), "").unwrap();
        assert_eq!(agent_git_problem_at(&tools), None);
        assert_eq!(
            step_failure("worktree_add_failed", &tools),
            "worktree_add_failed"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn engine_setup_git_does_not_run_the_xcrun_shim_under_the_sandbox() {
        if setup_git() == Path::new(GIT) {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let project = base.join("project");
        let home = base.join("home");
        let tmp = base.join("tmp");
        let profiles = base.join("profiles");
        for folder in [&project, &home, &tmp, &profiles] {
            fs::create_dir_all(folder).unwrap();
        }
        ensure_repository(&project, &tmp).unwrap();
        let sandbox =
            ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new())).unwrap();
        let profile = profiles.join("agent.sb");
        sandbox.install(&profile).unwrap();
        let printed = crate::spawn_gate::output(
            sandbox
                .command(&profile, sandboxed_git())
                .args(["version"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert!(
            printed.status.success(),
            "setup git could not run under the sandbox"
        );
        let stderr = String::from_utf8_lossy(&printed.stderr);
        assert!(
            !stderr.contains("xcrun") && !stderr.contains("Operation not permitted"),
            "setup git still runs the xcrun shim under the sandbox: {stderr}"
        );
    }

    /// Many agent worktrees add at once, each project in its own sandbox under one profile,
    /// the way a run of the team tests drives the setup path. Every add must succeed; a
    /// refusal here is the load flake the real-git setup fixes.
    #[cfg(target_os = "macos")]
    #[test]
    fn add_worktree_survives_concurrent_setup() {
        const PROJECTS: usize = 8;
        const PER_PROJECT: usize = 3;
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let mut setups = Vec::new();
        for index in 0..PROJECTS {
            let project = base.join(format!("project-{index}"));
            let home = base.join(format!("home-{index}"));
            let tmp = base.join(format!("tmp-{index}"));
            let profiles = base.join(format!("profiles-{index}"));
            for folder in [&project, &home, &tmp, &profiles] {
                fs::create_dir_all(folder).unwrap();
            }
            ensure_repository(&project, &tmp).unwrap();
            let sandbox =
                ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new()))
                    .unwrap();
            let profile = profiles.join("agent.sb");
            sandbox.install(&profile).unwrap();
            setups.push((sandbox, profile, project));
        }
        std::thread::scope(|scope| {
            let handles: Vec<_> = setups
                .into_iter()
                .map(|(sandbox, profile, project)| {
                    scope.spawn(move || {
                        for worktree_index in 0..PER_PROJECT {
                            let worktree = project
                                .join("worktrees")
                                .join(format!("agent-{worktree_index}"));
                            add_worktree(&sandbox, &profile, &worktree)
                                .expect("add_worktree under concurrent load");
                            assert!(worktree.join(".git").is_file());
                        }
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
        });
    }

    /// Many sandboxed setup commands started at once each get their own session. The shim
    /// leaves the group it was started in and calls setsid at once, and under load macOS can
    /// still see that group for a moment, so setsid failed with EPERM ("no new session") on
    /// about one start in a thousand; that was the `setup_refused` flake.
    #[cfg(target_os = "macos")]
    #[test]
    fn many_sandboxed_setup_commands_started_at_once_each_get_their_own_session() {
        const THREADS: usize = 32;
        const STARTS: usize = 40;
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let (project, home, tmp) = (base.join("project"), base.join("home"), base.join("tmp"));
        for folder in [&project, &home, &tmp] {
            fs::create_dir_all(folder).unwrap();
        }
        let sandbox =
            ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new())).unwrap();
        let profile = base.join("agent.sb");
        sandbox.install(&profile).unwrap();
        let refused = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    for _ in 0..STARTS {
                        let mut command = sandbox.command(&profile, Path::new("/usr/bin/true"));
                        command
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::piped());
                        let output = crate::spawn_gate::output(&mut command).unwrap();
                        if !output.status.success() {
                            refused
                                .lock()
                                .unwrap()
                                .push(String::from_utf8_lossy(&output.stderr).into_owned());
                        }
                    }
                });
            }
        });
        assert_eq!(refused.into_inner().unwrap(), Vec::<String>::new());
    }

    /// A setup command that exits well, exits with a failure or runs past its limit ends
    /// as each of the three, and the one past its limit is stopped with its group.
    #[test]
    fn a_bounded_setup_command_tells_success_failure_and_timeout_apart() {
        use std::os::unix::process::CommandExt;
        let run = |program: &str, arguments: &[&str], limit: Duration| {
            let mut command = Command::new(program);
            command
                .args(arguments)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let child = crate::spawn_gate::spawn(&mut command).unwrap();
            let group = child.id();
            (wait_bounded(child, limit).unwrap(), group)
        };
        let long = Duration::from_secs(30);
        assert_eq!(run("/usr/bin/true", &[], long).0, Ended::Succeeded);
        assert_eq!(run("/usr/bin/false", &[], long).0, Ended::Failed);
        let started = Instant::now();
        let (ended, group) = run("/bin/sleep", &["30"], Duration::from_millis(200));
        assert_eq!(ended, Ended::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(20));
        let pid = rustix::process::Pid::from_raw(i32::try_from(group).unwrap()).unwrap();
        assert!(rustix::process::test_kill_process_group(pid).is_err());
    }

    /// A worktree add that git refuses names its own step, not the generic setup refusal:
    /// a repository with no commit has no HEAD to add a worktree at.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_refused_worktree_add_names_its_step() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let project = base.join("project");
        let home = base.join("home");
        let tmp = base.join("tmp");
        for folder in [&project, &home, &tmp] {
            fs::create_dir_all(folder).unwrap();
        }
        repository_git(&project, &["init", "-q", "--template="]);
        let sandbox =
            ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new())).unwrap();
        let profile = base.join("agent.sb");
        sandbox.install(&profile).unwrap();
        let worktree = crate::projects_home::state_folder(&project)
            .join("worktrees")
            .join("agent-1");

        let refused = add_worktree(&sandbox, &profile, &worktree);

        assert!(
            matches!(refused, Err(RunError::Setup("worktree_add_failed"))),
            "{refused:?}"
        );
        assert!(!worktree.join(".git").exists());
    }

    /// A project whose config names a thousand stale branches lists more keys than a pipe
    /// holds (ScholarScope, v2.2.1 item 8): its agent worktree is still added, at once,
    /// since the setup step asks only for the filter keys it reads.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_worktree_is_added_when_the_project_config_lists_more_than_64_kib_of_keys() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let project = base.join("project");
        let home = base.join("home");
        let tmp = base.join("tmp");
        for folder in [&project, &home, &tmp] {
            fs::create_dir_all(folder).unwrap();
        }
        ensure_repository(&project, &tmp).unwrap();
        let sections: String = (0..1000)
            .map(|number| {
                format!(
                    "[branch \"stale-branch-number-{number}\"]\n\tremote = origin\n\tmerge = refs/heads/stale-branch-number-{number}\n"
                )
            })
            .collect();
        let mut config = OpenOptions::new()
            .append(true)
            .open(project.join(".git").join("config"))
            .unwrap();
        config.write_all(sections.as_bytes()).unwrap();
        drop(config);
        let sandbox =
            ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Only(Vec::new())).unwrap();
        let profile = base.join("agent.sb");
        sandbox.install(&profile).unwrap();
        let worktree = crate::projects_home::state_folder(&project)
            .join("worktrees")
            .join("agent-1");
        let started = Instant::now();

        let added = add_worktree(&sandbox, &profile, &worktree);

        assert!(added.is_ok(), "{added:?}");
        assert!(worktree.join(".git").is_file());
        assert!(started.elapsed() < SETUP_TIMEOUT);
    }

    #[cfg(target_os = "macos")]
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

    /// A filter listing is taken only whole: it must end with the engine's own closing key
    /// standing alone, and the keys before it are the repository's.
    #[test]
    fn a_filter_listing_is_whole_only_when_it_ends_with_the_engines_closing_key() {
        let end = FILTER_KEYS_END;
        assert!(FILTER_KEYS[1].as_bytes().starts_with(&end[..end.len() - 1]));
        assert_eq!(listed_filter_keys(end), Some(&b""[..]));
        let whole = [&b"filter.lfs.smudge\0filter.lfs.clean\0"[..], end].concat();
        assert_eq!(
            listed_filter_keys(&whole),
            Some(&b"filter.lfs.smudge\0filter.lfs.clean\0"[..])
        );
        let cut: [Vec<u8>; 5] = [
            Vec::new(),
            b"filter.lfs.smudge\0".to_vec(),
            end[..end.len() - 1].to_vec(),
            [&b"filter.lfs.smudge"[..], end].concat(),
            [end, &b"filter.late.smudge\0"[..]].concat(),
        ];
        for listing in &cut {
            assert_eq!(listed_filter_keys(listing), None, "{listing:?}");
        }
        assert_eq!(
            filter_overrides(listed_filter_keys(end).unwrap()).unwrap(),
            Vec::<String>::new()
        );
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

    #[cfg(target_os = "macos")]
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
    fn a_tools_off_claude_run_takes_the_no_tools_prompt_one_turn_and_its_output_cap() {
        let account = ResearchAccount::AnthropicEndpoint {
            base_url: "http://127.0.0.1:11434".into(),
            model: "local-model".into(),
            key: None,
        };
        let launch = |tools: bool, output_tokens: Option<u32>| {
            let mut command = Command::new("/usr/bin/true");
            claude_arguments(
                &mut command,
                Path::new("/agent/home"),
                Path::new("/agent/tmp"),
                tools,
                output_tokens,
                &account,
            )
            .unwrap();
            let arguments: Vec<String> = command
                .get_args()
                .map(|argument| argument.to_str().unwrap().to_owned())
                .collect();
            let cap = command
                .get_envs()
                .find(|(key, _)| *key == "CLAUDE_CODE_MAX_OUTPUT_TOKENS")
                .map(|(_, value)| value.unwrap().to_str().unwrap().to_owned());
            (arguments, cap)
        };
        let has = |arguments: &[String], pair: [&str; 2]| {
            arguments.windows(2).any(|window| window == pair)
        };
        let (off, cap) = launch(false, Some(16_000));
        assert!(has(&off, ["--tools", ""]), "{off:?}");
        assert!(
            has(&off, ["--system-prompt", CLAUDE_NO_TOOLS_PROMPT]),
            "{off:?}"
        );
        assert!(has(&off, ["--max-turns", "1"]), "{off:?}");
        assert!(has(&off, ["--output-format", "stream-json"]), "{off:?}");
        assert!(
            off.iter().any(|argument| argument == "--verbose"),
            "{off:?}"
        );
        assert!(!has(&off, ["--output-format", "text"]), "{off:?}");
        assert_eq!(cap.as_deref(), Some("16000"));
        assert!(CLAUDE_NO_TOOLS_PROMPT.contains("no tools"));
        let (on, cap) = launch(true, None);
        assert!(has(&on, ["--tools", "default"]), "{on:?}");
        assert!(has(&on, ["--output-format", "stream-json"]), "{on:?}");
        assert!(!has(&on, ["--output-format", "text"]), "{on:?}");
        assert!(
            !on.iter()
                .any(|argument| argument == "--system-prompt" || argument == "--max-turns")
        );
        assert_eq!(cap, None);
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

    const STREAM_START: &str = "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n";
    const STREAM_THINKING: &str = concat!(
        "{\"type\":\"assistant\",\"message\":{\"model\":\"m\",",
        "\"content\":[{\"type\":\"thinking\",\"thinking\":\"\"}]},\"parent_tool_use_id\":null}\n",
    );
    const STREAM_FIRST: &str = concat!(
        "{\"type\":\"assistant\",\"message\":{\"model\":\"m\",\"content\":[{\"type\":\"text\",",
        "\"text\":\"## Numbers\\n\\n1. one\\n2. tw\"}]},\"parent_tool_use_id\":null}\n",
    );
    const STREAM_CONTINUE: &str = concat!(
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",",
        "\"text\":\"Output token limit hit. Resume directly\"}]}}\n",
    );
    const STREAM_SECOND: &str = concat!(
        "{\"type\":\"assistant\",\"message\":{\"model\":\"m\",\"content\":[{\"type\":\"text\",",
        "\"text\":\"o\\n3. three\\n\"}]},\"parent_tool_use_id\":null}\n",
    );
    const STREAM_OWN_ERROR: &str = concat!(
        "{\"type\":\"assistant\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{",
        "\"type\":\"text\",\"text\":\"API Error: the response exceeded the output token ",
        "maximum\"}]},\"is_api_error_message\":true,\"error\":\"max_output_tokens\"}\n",
    );
    const STREAM_DONE: &str = concat!(
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,",
        "\"result\":\"o\\n3. three\\n\"}\n",
    );
    const STREAM_FAILED: &str = concat!(
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,",
        "\"result\":\"API Error\"}\n",
    );
    const STREAM_NARRATION: &str = concat!(
        "{\"type\":\"assistant\",\"message\":{\"model\":\"m\",\"content\":[{\"type\":\"text\",",
        "\"text\":\"I will read the notes first.\\n\"}]},\"parent_tool_use_id\":null}\n",
    );
    const STREAM_TOOL: &str = concat!(
        "{\"type\":\"assistant\",\"message\":{\"model\":\"m\",\"content\":[{\"type\":\"tool_use\",",
        "\"id\":\"toolu_1\",\"name\":\"Read\",\"input\":{}}]},\"parent_tool_use_id\":null}\n",
    );
    const STREAM_TOOL_RESULT: &str = concat!(
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",",
        "\"tool_use_id\":\"toolu_1\",\"content\":\"alpha\"}]},\"parent_tool_use_id\":null}\n",
    );
    const STREAM_HELPER_TEXT: &str = concat!(
        "{\"type\":\"assistant\",\"message\":{\"model\":\"m\",\"content\":[{\"type\":\"text\",",
        "\"text\":\"Notes from the helper.\\n\"}]},\"parent_tool_use_id\":\"toolu_2\"}\n",
    );
    const STREAM_HELPER_RESULT: &str = concat!(
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",",
        "\"tool_use_id\":\"toolu_3\",\"content\":\"beta\"}]},\"parent_tool_use_id\":\"toolu_2\"}\n",
    );

    /// DEC-160: Claude Code continues an answer cut at the output cap in new requests, and
    /// `--print` text kept only the last one, so the mapping stored a JSON tail as the
    /// overview. The answer is every assistant message's text, joined in order.
    #[test]
    fn a_tools_off_claude_answer_continued_past_its_output_cap_comes_back_whole() {
        let stream = [
            STREAM_START,
            STREAM_THINKING,
            STREAM_FIRST,
            STREAM_CONTINUE,
            STREAM_THINKING,
            STREAM_SECOND,
            STREAM_DONE,
        ]
        .concat();
        assert_eq!(
            claude_answer(&stream).as_deref(),
            Some("## Numbers\n\n1. one\n2. two\n3. three\n")
        );
    }

    /// An answer is taken only from a stream that ends in a successful result, and never
    /// holds Claude Code's own error message.
    #[test]
    fn a_claude_stream_without_a_successful_result_gives_no_answer() {
        let cut = [STREAM_START, STREAM_FIRST, STREAM_CONTINUE, STREAM_SECOND].concat();
        assert_eq!(claude_answer(&cut), None);
        let failed = [STREAM_START, STREAM_FIRST, STREAM_OWN_ERROR, STREAM_FAILED].concat();
        assert_eq!(claude_answer(&failed), None);
        let noted = [STREAM_START, STREAM_SECOND, STREAM_OWN_ERROR, STREAM_DONE].concat();
        assert_eq!(claude_answer(&noted).as_deref(), Some("o\n3. three\n"));
        assert_eq!(claude_answer(""), None);
    }

    /// A Claude Code run with tools gives back only the answer it ends with: the words after
    /// its last tool result, continued past the output cap, without the notes it wrote before
    /// a tool call, the words of a sub-agent, or a sub-agent's tool result cutting it short.
    #[test]
    fn a_claude_answer_after_its_tools_comes_back_whole_without_its_working_notes() {
        let stream = [
            STREAM_START,
            STREAM_NARRATION,
            STREAM_TOOL,
            STREAM_TOOL_RESULT,
            STREAM_FIRST,
            STREAM_HELPER_TEXT,
            STREAM_HELPER_RESULT,
            STREAM_CONTINUE,
            STREAM_SECOND,
            STREAM_DONE,
        ]
        .concat();
        assert_eq!(
            claude_answer(&stream).as_deref(),
            Some("## Numbers\n\n1. one\n2. two\n3. three\n")
        );
    }

    /// A Claude Code pane's reply is every part it said since its last tool call, so an
    /// answer continued past the output cap stays whole; a Codex pane's reply is its last
    /// message; a reply that outgrows the report bound is none until a tool call starts over.
    #[test]
    fn a_research_pane_keeps_the_whole_answer_its_agent_ends_with() {
        let mut claude = Reply::new(ResearchHarness::ClaudeCode, 64);
        claude.said("I will read the notes first.\n");
        claude.tool();
        claude.said("1. one\n2. tw");
        claude.said("o\n3. three\n");
        assert_eq!(claude.text(), Some("1. one\n2. two\n3. three\n"));
        let mut codex = Reply::new(ResearchHarness::Codex, 64);
        codex.said("Reading the notes.");
        codex.tool();
        codex.said("The report.");
        assert_eq!(codex.text(), Some("The report."));
        let mut long = Reply::new(ResearchHarness::ClaudeCode, 8);
        long.said("1234");
        long.said("56789");
        assert_eq!(long.text(), None);
        let mut again = Reply::new(ResearchHarness::ClaudeCode, 8);
        again.said("123456789");
        again.tool();
        again.said("1234");
        assert_eq!(again.text(), Some("1234"));
    }

    /// A long line's parent key is found wherever the reads split it, and the same words
    /// inside a string, where their quotes are escaped, are no key.
    #[test]
    fn a_long_claude_line_names_its_parent_however_its_bytes_are_split() {
        let lines: [(&[u8], Option<bool>); 3] = [
            (
                br#"{"type":"user","message":{},"parent_tool_use_id":null}"#,
                Some(false),
            ),
            (
                br#"{"type":"user","message":{"text":"\"parent_tool_use_id\":\"x"},"parent_tool_use_id":"toolu_2"}"#,
                Some(true),
            ),
            (br#"{"type":"assistant","message":{}}"#, None),
        ];
        for (line, parent) in lines {
            for split in 0..=line.len() {
                let mut long = LongLine::default();
                long.pass(&line[..split]);
                long.pass(&line[split..]);
                assert_eq!(long.parent, parent, "split at {split}");
                assert_eq!(long.head.starts_with(CLAUDE_USER_LINE), parent.is_some());
            }
            let mut bytewise = LongLine::default();
            for byte in line {
                bytewise.pass(std::slice::from_ref(byte));
            }
            assert_eq!(bytewise.parent, parent);
        }
    }

    /// A tool result past the line bound, such as a large PDF the agent read, which Claude
    /// Code prints twice in one line, is still the main thread's tool result or a sub-agent's
    /// line, so the answer the run ends with after it is kept whole (rv-code F1 r1).
    #[test]
    fn a_finished_claude_report_survives_a_tool_result_line_past_the_line_bound() {
        let long = |parent: &str| {
            let data = "A".repeat(CLAUDE_LINE_BYTES_MAX / 2);
            format!(
                concat!(
                    "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{",
                    "\"type\":\"tool_result\",\"tool_use_id\":\"toolu_1\",",
                    "\"content\":\"{data}\"}}]}},\"parent_tool_use_id\":{parent},",
                    "\"tool_use_result\":{{\"file\":\"{data}\"}}}}\n",
                ),
                data = data,
                parent = parent,
            )
        };
        let read = |lines: &[&str]| {
            let mut stream = ClaudeStream::new(1024);
            for part in lines.concat().as_bytes().chunks(8192) {
                stream.read(part);
            }
            stream.close();
            stream
        };
        let numbers = Some("## Numbers\n\n1. one\n2. two\n3. three\n");
        let main = long("null");
        assert!(main.len() > CLAUDE_LINE_BYTES_MAX);
        let after = read(&[
            STREAM_START,
            STREAM_NARRATION,
            STREAM_TOOL,
            &main,
            STREAM_FIRST,
            STREAM_CONTINUE,
            STREAM_SECOND,
            STREAM_DONE,
        ]);
        assert_eq!(after.answer(), numbers);
        let helper = long("\"toolu_2\"");
        assert!(helper.len() > CLAUDE_LINE_BYTES_MAX);
        let during = read(&[
            STREAM_START,
            STREAM_TOOL,
            STREAM_TOOL_RESULT,
            STREAM_FIRST,
            &helper,
            STREAM_CONTINUE,
            STREAM_SECOND,
            STREAM_DONE,
        ]);
        assert_eq!(during.answer(), numbers);
    }

    /// A Claude Code stream is read as its bytes arrive, lines split over reads and a last
    /// line with no break included; a line past the line bound leaves the run with no
    /// answer rather than a guess; a limit line Claude Code ends on is told from other errors.
    #[test]
    fn a_claude_stream_read_in_pieces_keeps_its_answer_and_never_guesses_past_a_long_line() {
        let whole = [
            STREAM_START,
            STREAM_FIRST,
            STREAM_CONTINUE,
            STREAM_SECOND,
            STREAM_DONE,
        ]
        .concat();
        let read = |bytes: &[u8], piece: usize| {
            let mut stream = ClaudeStream::new(1024);
            for part in bytes.chunks(piece) {
                stream.read(part);
            }
            stream.close();
            stream
        };
        let numbers = Some("## Numbers\n\n1. one\n2. two\n3. three\n");
        assert_eq!(read(whole.as_bytes(), 7).answer(), numbers);
        assert_eq!(read(whole.trim_end().as_bytes(), 1).answer(), numbers);
        assert_eq!(read(whole.as_bytes(), whole.len()).answer(), numbers);
        let mut long = ClaudeStream::new(1024);
        long.read(STREAM_FIRST.as_bytes());
        long.read(&vec![b'x'; CLAUDE_LINE_BYTES_MAX + 1]);
        long.read(
            [STREAM_CONTINUE, STREAM_SECOND, STREAM_DONE]
                .concat()
                .as_bytes(),
        );
        long.close();
        assert_eq!(long.answer(), None);
        let failed = [STREAM_START, STREAM_FIRST, STREAM_OWN_ERROR, STREAM_FAILED].concat();
        let failed = read(failed.as_bytes(), 64);
        assert_eq!(failed.answer(), None);
        assert!(!failed.limited());
        let limited = concat!(
            "{\"type\":\"assistant\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{",
            "\"type\":\"text\",\"text\":\"Claude AI usage limit reached\"}]},",
            "\"is_api_error_message\":true}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,",
            "\"result\":\"Claude AI usage limit reached\"}\n",
        );
        let limited = read(limited.as_bytes(), 64);
        assert_eq!(limited.answer(), None);
        assert!(limited.limited());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn a_read_only_run_on_windows_stops_with_the_plain_unavailable_line() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let engine = base.join("engine");
        fs::create_dir(&engine).unwrap();
        let database = engine.join("engine.db");
        let sqlite = rusqlite::Connection::open(&database).unwrap();
        sqlite.execute_batch("PRAGMA user_version=1").unwrap();
        sqlite.close().map_err(|(_, error)| error).unwrap();
        let state = engine.join("research");
        fs::create_dir(&state).unwrap();
        let runner =
            ResearchRunner::without_codex(&database, &state, ResearchLimits::default()).unwrap();
        let account = ResearchAccount::Endpoint {
            base_url: "http://127.0.0.1:9/v1".into(),
            model: "windows-model".into(),
            key: None,
        };
        let run = ReadOnlyRun {
            name: "onboard-m-windows",
            account: &account,
            wall_time: Duration::from_secs(5),
        };
        assert_eq!(
            runner.run_read_only(&run, &|| false).unwrap(),
            ReadOnlyEnding::Failed(
                "Team agents and research are not available on Windows in this version.".into()
            )
        );
        assert!(fs::read_dir(state.join("agents")).unwrap().next().is_none());
    }
}

#[cfg(all(test, unix))]
#[path = "runner/keyed_tests.rs"]
mod keyed_tests;
#[cfg(all(test, target_os = "macos"))]
#[path = "runner/live_claude_tests.rs"]
mod live_claude_tests;
#[cfg(all(test, target_os = "macos"))]
#[path = "runner/live_codex_tests.rs"]
mod live_codex_tests;
#[cfg(all(test, target_os = "macos"))]
#[path = "runner/live_engine_tests.rs"]
pub(crate) mod live_engine_tests;
#[cfg(all(test, target_os = "macos"))]
#[path = "runner/warden_tests.rs"]
mod warden_tests;
