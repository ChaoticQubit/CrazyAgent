#[cfg(unix)]
use super::MappingFacts;
use super::draft::{self, Answer, Drafted};
use super::graph::{self, GRAPHIFY_MISSING, GraphEnding};
use super::{Step, now_ms};
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use crate::protocol::{CredentialRef, ProjectId};
use crate::research::accounts::AccountPick;
#[cfg(unix)]
use crate::research::accounts::SettingsResolver;
use crate::research::jobs::JobError;
#[cfg(unix)]
use crate::research::runner::ResearchLimits;
use crate::research::runner::{
    RUN_NAME_USED, ReadOnlyEnding, ReadOnlyRun, ResearchHarness, ResearchRunner, RunError,
};
use crate::sandbox::SandboxError;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::cell::Cell;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STALE_MS: i64 = 30_000;
const HEARTBEAT_MS: i64 = 5_000;
const DATABASE_WAIT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const ENGINE_TICK: Duration = Duration::from_secs(2);
/// How long after its last check a mapping that waits out an account pause asks for
/// its account again before the pause ends, so a pool member added or freed early
/// takes effect while the claim still lets newer projects go first.
const PAUSE_RECHECK_MS: i64 = 10_000;
const RUN_NAME_BYTES_MAX: usize = 96;
const NO_ACCOUNT: &str = "No signed-in account in this pool can map the project";
const MAPPING_HARNESSES: [ResearchHarness; 2] =
    [ResearchHarness::Codex, ResearchHarness::ClaudeCode];

/// The plain reason a mapping step fails with when neither Codex nor Claude
/// Code is installed, so no agent can map the project.
pub const CODEX_MISSING: &str = "codex not installed";
#[cfg(unix)]
const GRAPH_TIMED_OUT: &str = "Graph build ran out of time";
const GRAPH_NOT_STARTED: &str = "Graph build could not start";
#[cfg(unix)]
const MAPPING_TIMED_OUT: &str = "Mapping agent ran out of time";
const MAPPING_NOT_STARTED: &str = "Mapping agent could not start";
const DRAFT_UNSAVED: &str = "Could not save the project draft";
#[cfg(unix)]
pub(crate) const MAPPING_NO_OVERVIEW: &str = "Mapping agent returned no overview";
#[cfg(unix)]
const GRAPHIFY_CANDIDATES: [&str; 2] = ["/opt/homebrew/bin/graphify", "/usr/local/bin/graphify"];

/// How long each onboarding step may run before it fails as out of time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnboardingLimits {
    pub graph_time: Duration,
    pub mapping_time: Duration,
}

impl Default for OnboardingLimits {
    /// Ten minutes for the graph build and fifteen for the mapping agent.
    fn default() -> Self {
        Self {
            graph_time: Duration::from_secs(600),
            mapping_time: Duration::from_secs(900),
        }
    }
}

/// Which step of an onboarding row a worker claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Graph,
    Mapping,
}

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Graph => "graph",
            Self::Mapping => "mapping",
        }
    }

    fn short(self) -> &'static str {
        match self {
            Self::Graph => "g",
            Self::Mapping => "m",
        }
    }
}

/// One claimed step with the row facts the worker needs to run it.
#[derive(Debug, Clone)]
struct Claimed {
    project: String,
    repo: PathBuf,
    pool: CredentialRef,
    drafted: Drafted,
    created_ms: i64,
    runs: i64,
    kind: Kind,
}

/// Picks the research account for a pool and the harnesses the runner can
/// start. Input: the pool reference and the usable harnesses. Output: the
/// account, or why there is none, such as every fitting member paused.
type AccountLookup = dyn Fn(&CredentialRef, &[ResearchHarness]) -> AccountPick + Send;

/// What the account and graphify lookups resolve to on every step, so a
/// newly assigned pool member or a newly installed graphify takes effect
/// without a restart.
struct Work {
    runner: Arc<ResearchRunner>,
    accounts: Box<AccountLookup>,
    graphify: Box<dyn Fn() -> Option<PathBuf> + Send>,
    limits: OnboardingLimits,
    tick: Duration,
    claimer: String,
}

/// Background loop that moves every onboarding forward one step at a time:
/// the graph build first, then the mapping agent, each sandboxed, bounded in
/// time and stopped with its whole process group on shutdown.
pub struct OnboardingService {
    runner: Arc<ResearchRunner>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl OnboardingService {
    /// Starts the loop. `accounts` maps a pool and the harnesses the runner
    /// has, Codex first, to the account its mapping agent runs on; `graphify`
    /// finds the graphify executable; both are asked again for every step.
    /// `tick` is the idle wait between claims.
    pub fn start<A, G>(
        runner: Arc<ResearchRunner>,
        accounts: A,
        graphify: G,
        limits: OnboardingLimits,
        tick: Duration,
    ) -> Self
    where
        A: Fn(&CredentialRef, &[ResearchHarness]) -> AccountPick + Send + 'static,
        G: Fn() -> Option<PathBuf> + Send + 'static,
    {
        assert!(!tick.is_zero());
        assert!(!limits.graph_time.is_zero() && !limits.mapping_time.is_zero());
        let stop = Arc::new(AtomicBool::new(false));
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let work = Work {
            runner: runner.clone(),
            accounts: Box::new(accounts),
            graphify: Box::new(graphify),
            limits,
            tick,
            claimer: format!("onboarding-{}-{nanos}", std::process::id()),
        };
        let worker = {
            let stop = stop.clone();
            std::thread::spawn(move || serve(&work, &stop))
        };
        Self {
            runner,
            stop,
            worker: Some(worker),
        }
    }

    /// Stops the loop and the running step's processes; an interrupted step
    /// goes back to waiting so the next engine run starts it again.
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.runner.shutdown();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for OnboardingService {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Opens the engine database for the worker without following a link.
fn open(database: &Path) -> Option<Connection> {
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .ok()?;
    connection.busy_timeout(DATABASE_WAIT).ok()?;
    Some(connection)
}

fn serve(work: &Work, stop: &AtomicBool) {
    let Some(database) = open(work.runner.database()) else {
        return;
    };
    let halted = || stop.load(Ordering::SeqCst) || work.runner.is_stopping();
    while !halted() {
        let _ = recover_stale(&database);
        match claim(&database, &work.claimer) {
            Ok(Some(claimed)) => {
                let waits = run_step(work, &database, &claimed, &halted);
                if waits && !halted() {
                    std::thread::sleep(work.tick);
                }
            }
            Ok(None) | Err(_) => std::thread::sleep(work.tick),
        }
    }
}

/// Sends steps whose worker stopped sending heartbeats back to waiting.
fn recover_stale(database: &Connection) -> rusqlite::Result<usize> {
    database.execute(
        "UPDATE onboarding SET
            graph_state=CASE WHEN graph_state='running' THEN 'waiting' ELSE graph_state END,
            mapping_state=CASE WHEN mapping_state='running' THEN 'waiting' ELSE mapping_state END,
            claimer=NULL, heartbeat_ms=NULL
         WHERE claimer IS NOT NULL AND (heartbeat_ms IS NULL OR heartbeat_ms<?1)",
        [now_ms() - STALE_MS],
    )
}

/// Claims the oldest waiting step: a graph build, or a mapping once its
/// graph step has finished either way, while the owner has not saved the
/// setup and no account pause it waits on is still running, or that pause was
/// last checked [`PAUSE_RECHECK_MS`] ago. Marks it running under `claimer`
/// and counts the attempt. Returns `None` when nothing waits.
fn claim(database: &Connection, claimer: &str) -> Result<Option<Claimed>, String> {
    let tx = rusqlite::Transaction::new_unchecked(database, TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let now = now_ms();
    let row = tx
        .query_row(
            "SELECT project,repo,pool,drafted,runs,graph_state='waiting',created_ms FROM onboarding o
             WHERE claimer IS NULL AND (graph_state='waiting'
                OR (mapping_state='waiting' AND graph_state IN ('done','failed','off')
                    AND (mapping_reason IS NULL OR CAST(mapping_reason AS INTEGER)<=?2
                        OR updated_ms<=?3)
                    AND NOT EXISTS (SELECT 1 FROM project_setups s WHERE s.project=o.project
                        AND json_extract(s.body,'$.revision')>?1)))
             ORDER BY created_ms,project LIMIT 1",
            params![
                crate::project_setup::FIRST_REVISION as i64,
                now,
                now - PAUSE_RECHECK_MS
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let Some((project, repo, pool, drafted, runs, graph, created_ms)) = row else {
        return Ok(None);
    };
    let kind = if graph { Kind::Graph } else { Kind::Mapping };
    let changed = tx
        .execute(
            &format!(
                "UPDATE onboarding SET {0}_state='running',{0}_reason=NULL,runs=runs+1,
                    claimer=?2,heartbeat_ms=?3,updated_ms=?3
                 WHERE project=?1 AND claimer IS NULL",
                kind.prefix()
            ),
            params![project, claimer, now],
        )
        .map_err(|error| error.to_string())?;
    assert_eq!(changed, 1);
    tx.commit().map_err(|error| error.to_string())?;
    Ok(Some(Claimed {
        project,
        repo: PathBuf::from(repo),
        pool: CredentialRef::new(pool).map_err(|problem| problem.0)?,
        drafted: serde_json::from_str(&drafted).map_err(|error| error.to_string())?,
        created_ms,
        runs: runs + 1,
        kind,
    }))
}

/// Records how a claimed step ended and releases the claim, with the new
/// draft for a mapping step. Changes nothing once another worker holds it.
fn finish(
    database: &Connection,
    claimer: &str,
    claimed: &Claimed,
    step: &Step,
    drafted: Option<&Drafted>,
) -> Result<(), String> {
    let (state, reason) = step.columns();
    assert_ne!(state, "running");
    let drafted = drafted
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| error.to_string())?;
    database
        .execute(
            &format!(
                "UPDATE onboarding SET {0}_state=?3,{0}_reason=?4,
                    drafted=COALESCE(?5,drafted),claimer=NULL,heartbeat_ms=NULL,updated_ms=?6
                 WHERE project=?1 AND claimer=?2",
                claimed.kind.prefix()
            ),
            params![claimed.project, claimer, state, reason, drafted, now_ms()],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Saves a draft on the claimed row while its step still runs.
fn store_draft(
    database: &Connection,
    claimer: &str,
    claimed: &Claimed,
    drafted: &Drafted,
) -> Result<(), String> {
    let body = serde_json::to_string(drafted).map_err(|error| error.to_string())?;
    database
        .execute(
            "UPDATE onboarding SET drafted=?3,updated_ms=?4 WHERE project=?1 AND claimer=?2",
            params![claimed.project, claimer, body, now_ms()],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// The agent state name of one step attempt: the step, the project, when its
/// onboarding row was made and the attempt, so no attempt reuses a folder an
/// earlier onboarding of the same project id left. The project part is cut
/// short only when the whole name would pass the bound. Returns the name.
fn run_name(claimed: &Claimed) -> String {
    assert!(claimed.project.is_ascii() && !claimed.project.is_empty());
    let head = format!("onboard-{}-", claimed.kind.short());
    let tail = format!("-{}-{}", claimed.created_ms, claimed.runs);
    let room = RUN_NAME_BYTES_MAX - head.len() - tail.len();
    let project = &claimed.project[..claimed.project.len().min(room)];
    let name = format!("{head}{project}{tail}");
    assert!(name.len() <= RUN_NAME_BYTES_MAX);
    name
}

/// Runs one claimed step to its end and records it. Takes the work, the
/// worker's database, the claim and `halted`. Returns whether the step went
/// back to waiting, so the loop pauses before claiming again.
fn run_step(
    work: &Work,
    database: &Connection,
    claimed: &Claimed,
    halted: &dyn Fn() -> bool,
) -> bool {
    let last_beat = Cell::new(now_ms());
    let lost = Cell::new(false);
    let beat = || {
        let now = now_ms();
        if now - last_beat.get() >= HEARTBEAT_MS {
            last_beat.set(now);
            let kept = database.execute(
                "UPDATE onboarding SET heartbeat_ms=?3 WHERE project=?1 AND claimer=?2",
                params![claimed.project, work.claimer, now],
            );
            if !matches!(kept, Ok(1)) {
                lost.set(true);
            }
        }
        lost.get() || halted()
    };
    let (step, drafted) = match claimed.kind {
        Kind::Graph => (graph_step(work, claimed, &beat), None),
        Kind::Mapping => {
            let (step, drafted) = mapping_step(work, database, claimed, &beat);
            (step, Some(drafted))
        }
    };
    let waits = matches!(step, Step::Waiting { .. });
    let _ = finish(database, &work.claimer, claimed, &step, drafted.as_ref());
    waits
}

fn graph_step(work: &Work, claimed: &Claimed, beat: &dyn Fn() -> bool) -> Step {
    let Some(graphify) = (work.graphify)() else {
        return Step::failed(GRAPHIFY_MISSING);
    };
    match graph::update(
        &graphify,
        &claimed.repo,
        work.runner.state(),
        &run_name(claimed),
        work.limits.graph_time,
        beat,
    ) {
        #[cfg(unix)]
        Ok(GraphEnding::Built) => Step::Done,
        Ok(GraphEnding::Failed(reason)) => Step::failed(reason),
        #[cfg(unix)]
        Ok(GraphEnding::TimedOut) => Step::failed(GRAPH_TIMED_OUT),
        #[cfg(unix)]
        Ok(GraphEnding::Stopped) => Step::WAITING,
        Err(RunError::Uncertain(_)) => Step::failed(crate::sandbox::warden::CLEANUP_UNCERTAIN),
        Err(error) => not_started(Kind::Graph, &error),
    }
}

/// Fails a step that could not start with its plain sentence and a short cause
/// code, and writes one diagnostic line naming the step and the cause to the
/// engine's stderr log. Takes the step kind and the runner error. Returns the step.
fn not_started(kind: Kind, error: &RunError) -> Step {
    let cause = start_cause(error);
    eprintln!("{}", start_diagnostic(kind, &cause));
    let sentence = match kind {
        Kind::Graph => GRAPH_NOT_STARTED,
        Kind::Mapping => MAPPING_NOT_STARTED,
    };
    Step::failed(&format!("{sentence} ({cause})"))
}

/// The engine log line for a step that could not start. Takes the step kind and
/// its cause code. Returns the line.
fn start_diagnostic(kind: Kind, cause: &str) -> String {
    format!("onboarding_{}_not_started cause={cause}", kind.prefix())
}

/// The short cause code of a runner error: which refusal or failure stopped the
/// start, never the error text, which can name paths. Takes the error. Returns
/// lowercase letters, digits and `_` only.
fn start_cause(error: &RunError) -> String {
    let cause = match error {
        RunError::Invalid(reason) if reason == RUN_NAME_USED => "run_name_used".to_owned(),
        RunError::Invalid(_) => "run_refused".to_owned(),
        RunError::Jobs(JobError::Invalid(_)) => "job_refused".to_owned(),
        RunError::Jobs(JobError::NotHeld) => "job_not_held".to_owned(),
        RunError::Jobs(JobError::Storage(_)) => "job_storage".to_owned(),
        RunError::Sandbox(SandboxError::Invalid(_)) => "sandbox_refused".to_owned(),
        RunError::Sandbox(SandboxError::Unavailable(_)) => "sandbox_unavailable".to_owned(),
        RunError::Sandbox(SandboxError::Io(error)) => io_cause("sandbox_io", error),
        RunError::Context(_) => "context_refused".to_owned(),
        RunError::Account(_) => "account_unavailable".to_owned(),
        RunError::Busy => "account_busy".to_owned(),
        RunError::Uncertain(_) => "cleanup_uncertain".to_owned(),
        RunError::Setup(code) => (*code).to_owned(),
        RunError::Io(error) => io_cause("io", error),
    };
    assert!(
        cause
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    );
    cause
}

/// `prefix` and the kind of an I/O error, such as `io_permission_denied`, without
/// the error's own text. Takes the prefix and the error. Returns the code.
fn io_cause(prefix: &str, error: &std::io::Error) -> String {
    let kind: String = error
        .kind()
        .to_string()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{prefix}_{kind}")
}

/// Maps the project: collects the bounded evidence, saves the draft the
/// manifests alone give, then asks a tools-off agent on the pool for an
/// overview and the remaining fields and merges its answer into the gaps.
/// Fails with [`CODEX_MISSING`] when the runner has no harness to map with,
/// and waits while every pool member that fits is paused after a limit.
/// Returns the step's ending and the draft to keep.
fn mapping_step(
    work: &Work,
    database: &Connection,
    claimed: &Claimed,
    beat: &dyn Fn() -> bool,
) -> (Step, Drafted) {
    let layout = claimed.repo.join(CRAZYAGENT_DIRECTORY);
    let evidence = draft::gather(&claimed.repo);
    let (found, stack) = draft::found_facts(&evidence, &claimed.repo);
    let mut base = claimed.drafted.clone();
    base.draft.pool = Some(claimed.pool.clone());
    let base = draft::merge(&base, &Answer::default(), &found, &stack);
    if base != claimed.drafted {
        let _ = store_draft(database, &work.claimer, claimed, &base);
    }
    if draft::save(&layout, &base).is_err() {
        return (Step::failed(DRAFT_UNSAVED), base);
    }
    let harnesses: Vec<ResearchHarness> = MAPPING_HARNESSES
        .into_iter()
        .filter(|harness| work.runner.can_run(*harness))
        .collect();
    if harnesses.is_empty() {
        return (Step::failed(CODEX_MISSING), base);
    }
    let account = match (work.accounts)(&claimed.pool, &harnesses) {
        AccountPick::Account { account, .. } if harnesses.contains(&account.harness()) => *account,
        AccountPick::Paused { until_ms } => {
            return (
                Step::Waiting {
                    until_ms: Some(until_ms),
                },
                base,
            );
        }
        _ => return (Step::failed(NO_ACCOUNT), base),
    };
    let name = run_name(claimed);
    #[cfg(unix)]
    let prompt = draft::prompt(&evidence, &found);
    let project = ProjectId::new(claimed.project.clone()).ok();
    let run = ReadOnlyRun {
        name: &name,
        agent: "onboarding",
        project: project.as_ref(),
        #[cfg(unix)]
        folder: &claimed.repo,
        #[cfg(unix)]
        prompt: &prompt,
        account: &account,
        wall_time: work.limits.mapping_time,
    };
    match work.runner.run_read_only(&run, beat) {
        #[cfg(unix)]
        Ok(ReadOnlyEnding::Answered(text)) => answered(&layout, &base, &text, &found, &stack),
        Ok(ReadOnlyEnding::Failed(reason)) => (Step::failed(&reason), base),
        #[cfg(unix)]
        Ok(ReadOnlyEnding::TimedOut) => (Step::failed(MAPPING_TIMED_OUT), base),
        #[cfg(unix)]
        Ok(ReadOnlyEnding::Stopped) => (Step::WAITING, base),
        Err(RunError::Busy) => (Step::WAITING, base),
        Err(RunError::Account(_)) => (Step::failed(NO_ACCOUNT), base),
        Err(RunError::Uncertain(_)) => (
            Step::failed(crate::sandbox::warden::CLEANUP_UNCERTAIN),
            base,
        ),
        Err(error) => (not_started(Kind::Mapping, &error), base),
    }
}

/// Finishes a mapping step from the agent's answer: merges its fields into the gaps of
/// `base`, saves the draft and writes `overview.md` when none exists yet. An answer with
/// no overview text, while no overview is kept from before, fails the step with
/// [`MAPPING_NO_OVERVIEW`] and still keeps the merged draft. Takes the `.crazyagent`
/// folder, the draft so far, the answer text and the facts the manifests gave.
/// Returns the step's ending and the draft to keep.
#[cfg(unix)]
pub(crate) fn answered(
    layout: &Path,
    base: &Drafted,
    text: &str,
    found: &MappingFacts,
    stack: &[String],
) -> (Step, Drafted) {
    let answer = draft::parse_answer(text);
    let merged = draft::merge(base, &answer, found, stack);
    let Ok(wrote) = draft::write_overview(layout, &answer.overview) else {
        return (Step::failed(DRAFT_UNSAVED), base.clone());
    };
    if draft::save(layout, &merged).is_err() {
        return (Step::failed(DRAFT_UNSAVED), base.clone());
    }
    if !wrote && draft::overview(layout).is_none() {
        return (Step::failed(MAPPING_NO_OVERVIEW), merged);
    }
    (Step::Done, merged)
}

/// The graphify executable this machine has, looked up again on each call:
/// the owner's `~/.local/bin/graphify` from a uv tool install, then the
/// Homebrew locations. Returns the canonical file, or `None`.
#[cfg(unix)]
pub fn installed_graphify() -> Option<PathBuf> {
    let home = nix::unistd::User::from_uid(nix::unistd::getuid())
        .ok()
        .flatten()
        .map(|user| user.dir.join(".local/bin/graphify"));
    home.into_iter()
        .chain(GRAPHIFY_CANDIDATES.iter().map(PathBuf::from))
        .filter_map(|candidate| candidate.canonicalize().ok())
        .find(|candidate| candidate.is_file())
}

/// The graphify executable a uv tool install puts in the owner's
/// `.local\bin` folder, looked up again on each call. Returns the canonical
/// file, or `None`.
#[cfg(not(unix))]
pub fn installed_graphify() -> Option<PathBuf> {
    std::env::home_dir()
        .map(|home| home.join(".local").join("bin").join("graphify.exe"))
        .and_then(|candidate| candidate.canonicalize().ok())
        .filter(|candidate| candidate.is_file())
}

/// Refuses engine onboarding where its 0700 state folders and private database cannot
/// be made or checked. Takes the runtime, settings files and the optional Codex and
/// Claude Code paths, all unused. Returns `onboarding_runtime_unavailable`.
#[cfg(not(unix))]
pub fn start_for_engine(
    _runtime: &Path,
    _global: &Path,
    _project: &Path,
    _codex: Option<&Path>,
    _claude: Option<&Path>,
) -> Result<OnboardingService, String> {
    Err("onboarding_runtime_unavailable".into())
}

/// Starts onboarding for a running writable engine: steps claimed from the
/// engine database's `onboarding` table, agent state in
/// `<runtime>/onboarding`, the mapping account picked live from the settings
/// files among the installed harnesses and graphify found on every graph
/// step. Codex and Claude Code are both optional: without either, graphs
/// still build and only mapping fails with [`CODEX_MISSING`].
#[cfg(unix)]
pub fn start_for_engine(
    runtime: &Path,
    global: &Path,
    project: &Path,
    codex: Option<&Path>,
    claude: Option<&Path>,
) -> Result<OnboardingService, String> {
    let runtime = runtime
        .canonicalize()
        .map_err(|error| format!("onboarding runtime: {error}"))?;
    let state = runtime.join("onboarding");
    let projects = runtime.join(crate::project_setup::PROJECTS_DIRECTORY);
    for directory in [&state, &projects] {
        match std::fs::DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("onboarding folder: {error}")),
        }
        if !std::fs::symlink_metadata(directory)
            .map_err(|error| format!("onboarding folder: {error}"))?
            .file_type()
            .is_dir()
        {
            return Err("onboarding folder is not a directory".into());
        }
    }
    let database = runtime.join("engine.db");
    let metadata = std::fs::symlink_metadata(&database)
        .map_err(|error| format!("engine database: {error}"))?;
    if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err("engine database must be a private file".into());
    }
    let mut runner = match codex {
        Some(codex) => ResearchRunner::new(&database, &state, codex, ResearchLimits::default()),
        None => ResearchRunner::without_codex(&database, &state, ResearchLimits::default()),
    }
    .map_err(|error| error.to_string())?;
    if let Some(claude) = claude {
        runner = runner
            .with_claude(claude)
            .map_err(|error| error.to_string())?;
    }
    let resolver = SettingsResolver::new(global, project, &projects, &database)?;
    Ok(OnboardingService::start(
        Arc::new(runner),
        move |pool: &CredentialRef, harnesses: &[ResearchHarness]| {
            resolver.pool_pick(pool, harnesses)
        },
        move || engine_graphify(&runtime),
        OnboardingLimits::default(),
        ENGINE_TICK,
    ))
}

/// The graphify an engine's graph step runs, looked up again on each step the way
/// setup finds it: `<engine root>/setup/bin`, where setup installs it, first, then
/// the system folders setup searches. Input: the engine root. Output: the program,
/// or `None` when neither has one.
#[cfg(unix)]
pub(crate) fn engine_graphify(runtime: &Path) -> Option<PathBuf> {
    crate::setup::platform_installer(runtime).program(crate::setup::Dependency::Graphify.program())
}

#[cfg(test)]
mod claim_tests;
#[cfg(test)]
mod start_tests;
