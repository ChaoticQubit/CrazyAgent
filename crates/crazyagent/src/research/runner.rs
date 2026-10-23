use super::jobs::{JobError, ResearchJob, ResearchJobs, ResearchStatus, valid_kind};
use crate::sandbox::{ProjectSandbox, SandboxError, SandboxNetwork, SandboxRemote};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const GIT: &str = "/usr/bin/git";
const BRIEF_BYTES_MAX: u64 = 32 * 1024;
const DOCUMENTS_LISTED_MAX: usize = 50;
const LINKS_MAX: usize = 100;
const POLL: Duration = Duration::from_millis(100);
const HEARTBEAT: Duration = Duration::from_millis(250);
const MODEL_BYTES_MAX: usize = 256;
const URL_BYTES_MAX: usize = 2048;
const SETUP_TIMEOUT: Duration = Duration::from_secs(60);

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

/// The pool account a research agent runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResearchAccount {
    /// An OpenAI-compatible URL or a local model server, with no key.
    Endpoint { base_url: String, model: String },
    /// A signed-in managed Codex profile directory owned by the engine.
    ManagedCodex {
        home: PathBuf,
        model: Option<String>,
    },
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
    Io(io::Error),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "research refused: {reason}"),
            Self::Jobs(error) => write!(f, "{error}"),
            Self::Sandbox(error) => write!(f, "{error}"),
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

/// Claims queued research jobs and runs each one as a sandboxed Codex agent
/// in its own git worktree under the project folder, then writes the agent's
/// report to `context/research/<kind>.md`.
pub struct ResearchRunner {
    database: PathBuf,
    state: PathBuf,
    codex: PathBuf,
    limits: ResearchLimits,
    claimer: String,
    stopping: AtomicBool,
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
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Ok(Self {
            database: database.to_path_buf(),
            state: state.to_path_buf(),
            codex: codex.to_path_buf(),
            limits,
            claimer: format!("runner-{}-{nanos}", std::process::id()),
            stopping: AtomicBool::new(false),
        })
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

    /// Claims every job the per-project limit allows right now and runs each
    /// on its own thread.
    pub fn spawn_ready(
        self: &Arc<Self>,
        project: String,
        root: PathBuf,
        account: Option<ResearchAccount>,
    ) -> Vec<JoinHandle<Result<ResearchRun, RunError>>> {
        let mut handles = Vec::new();
        let Ok(jobs) = ResearchJobs::open(&self.database) else {
            return handles;
        };
        while !self.is_stopping()
            && let Ok(Some(job)) = jobs.claim(&project, &self.claimer, self.limits.running_max)
        {
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
            assert!(handles.len() <= self.limits.running_max as usize);
        }
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
        if job.harness != "codex" {
            jobs.fail(
                job.id,
                &self.claimer,
                "This harness cannot run research yet",
                1,
            )?;
            return Ok(run(ResearchStatus::Failed, None, None));
        }
        let Some(account) = &project.account else {
            jobs.needs_pool(job.id, &self.claimer)?;
            return Ok(run(ResearchStatus::NeedsPool, None, None));
        };
        match self.launch(jobs, &job, project, account) {
            Ok((Ending::Exited(true), group, output)) => {
                let report = match self.publish(&job, project.root, &output) {
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
            Err(error) => {
                jobs.fail(
                    job.id,
                    &self.claimer,
                    "Could not prepare the research sandbox",
                    self.limits.attempts_max,
                )?;
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
        let name = format!("job-{}-{}", job.id, job.attempts);
        let agent = self.state.join("agents").join(&name);
        private_directory(&agent)?;
        let tmp = agent.join("tmp");
        private_directory(&tmp)?;
        let home = match account {
            ResearchAccount::Endpoint { .. } => {
                let home = agent.join("home");
                private_directory(&home)?;
                home
            }
            ResearchAccount::ManagedCodex { home, .. } => home.canonicalize()?,
        };
        let network = if job.tools {
            SandboxNetwork::Open
        } else {
            SandboxNetwork::Only(vec![model_remote(account)?])
        };
        let mut sandbox = ProjectSandbox::new(&root, &home, &tmp, network)?;
        if let Some(install) = self.codex.canonicalize()?.parent() {
            sandbox = sandbox.allow_read(install)?;
        }
        let profile = self.state.join("profiles").join(format!("{name}.sb"));
        sandbox.install(&profile)?;
        ensure_repository(&root, &tmp)?;
        let worktree = root
            .join("worktrees")
            .join(format!("research-{}-{}-{}", job.kind, job.id, job.attempts));
        fs::create_dir_all(root.join("worktrees"))?;
        let mut git = sandbox.command(&profile, Path::new(GIT));
        git.args([
            "-c",
            "core.hooksPath=/dev/null",
            "worktree",
            "add",
            "-q",
            "--detach",
        ])
        .arg(&worktree)
        .arg("HEAD")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        if !wait_bounded(git.spawn()?, SETUP_TIMEOUT)? {
            return Err(RunError::Invalid("git worktree add failed".into()));
        }
        let prompt = research_prompt(&root, &job.kind)?;
        let output = tmp.join("report.md");
        let mut codex = sandbox.command(&profile, &self.codex);
        codex
            .arg("exec")
            .args(["--skip-git-repo-check", "--color", "never", "--json"])
            .arg("-C")
            .arg(&worktree)
            .arg("-o")
            .arg(&output)
            .arg("-s")
            .arg(if job.tools {
                "danger-full-access"
            } else {
                "read-only"
            })
            .env("CODEX_HOME", &home)
            .current_dir(&worktree);
        account_arguments(&mut codex, account)?;
        codex
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = codex.spawn()?;
        let group = child.id();
        assert!(group > 1);
        let counted = Arc::new(AtomicU64::new(0));
        for (stream, name) in [
            (
                child
                    .stdout
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Read + Send>),
                "events.jsonl",
            ),
            (
                child
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
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(prompt.as_bytes());
        }
        let ending = self.supervise(jobs, job, &mut child, group, &counted)?;
        Ok((ending, group, output))
    }

    fn supervise(
        &self,
        jobs: &ResearchJobs,
        job: &ResearchJob,
        child: &mut Child,
        group: u32,
        counted: &AtomicU64,
    ) -> Result<Ending, RunError> {
        let started = Instant::now();
        let mut beat = Instant::now();
        let ending = loop {
            if let Some(status) = child.try_wait()? {
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
        stop_group(group);
        let _ = child.wait();
        let _ = jobs.heartbeat(job.id, &self.claimer, counted.load(Ordering::Relaxed));
        Ok(ending)
    }

    fn publish(
        &self,
        job: &ResearchJob,
        root: &Path,
        output: &Path,
    ) -> Result<PathBuf, &'static str> {
        let body = read_bounded(output, self.limits.report_bytes_max)
            .map_err(|_| "Agent returned no report")?;
        let body = body.trim();
        if body.is_empty() {
            return Err("Agent returned no report");
        }
        let report = render_report(&kind_label(&job.kind), "Codex", body);
        write_report(root, &job.kind, &report).map_err(|_| "Could not save the research report")
    }
}

fn model_remote(account: &ResearchAccount) -> Result<SandboxRemote, RunError> {
    match account {
        ResearchAccount::ManagedCodex { .. } => Ok(SandboxRemote::AnyHost(443)),
        ResearchAccount::Endpoint { base_url, .. } => {
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

fn account_arguments(command: &mut Command, account: &ResearchAccount) -> Result<(), RunError> {
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
    }
    Ok(())
}

fn ensure_repository(root: &Path, tmp: &Path) -> Result<(), RunError> {
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
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(20));
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
    let context = root.join("context");
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
    let mut report = format!(
        "# {label}\n\nResearched by {harness} on {}.\n\n{body}\n",
        utc_date(SystemTime::now())
    );
    if !body.to_lowercase().contains("## links and references") {
        report.push_str("\n## Links and references\n\n");
        let links = links_in(body);
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
    let context = root.join("context");
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
