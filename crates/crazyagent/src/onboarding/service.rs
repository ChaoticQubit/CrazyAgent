use super::draft::{self, Answer, Drafted};
use super::graph::{self, GRAPHIFY_MISSING, GraphEnding};
use super::{Step, now_ms};
use crate::projects_home::CRAZYAGENT_DIRECTORY;
use crate::protocol::CredentialRef;
use crate::research::accounts::SettingsResolver;
use crate::research::runner::{
    ReadOnlyEnding, ReadOnlyRun, ResearchAccount, ResearchLimits, ResearchRunner, RunError,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::cell::Cell;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STALE_MS: i64 = 30_000;
const HEARTBEAT_MS: i64 = 5_000;
const DATABASE_WAIT: Duration = Duration::from_secs(5);
const ENGINE_TICK: Duration = Duration::from_secs(2);
const RUN_NAME_BYTES_MAX: usize = 96;
const NO_ACCOUNT: &str = "No signed-in account in this pool can map the project";
const GRAPH_TIMED_OUT: &str = "Graph build ran out of time";
const GRAPH_NOT_STARTED: &str = "Graph build could not start";
const MAPPING_TIMED_OUT: &str = "Mapping agent ran out of time";
const MAPPING_NOT_STARTED: &str = "Mapping agent could not start";
const DRAFT_UNSAVED: &str = "Could not save the project draft";
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
    runs: i64,
    kind: Kind,
}

/// What the account and graphify lookups resolve to on every step, so a
/// newly assigned pool member or a newly installed graphify takes effect
/// without a restart.
struct Work {
    runner: Arc<ResearchRunner>,
    accounts: Box<dyn Fn(&CredentialRef) -> Option<ResearchAccount> + Send>,
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
    /// Starts the loop. `accounts` maps a pool to the account its mapping
    /// agent runs on, `graphify` finds the graphify executable, both asked
    /// again for every step; `tick` is the idle wait between claims.
    pub fn start<A, G>(
        runner: Arc<ResearchRunner>,
        accounts: A,
        graphify: G,
        limits: OnboardingLimits,
        tick: Duration,
    ) -> Self
    where
        A: Fn(&CredentialRef) -> Option<ResearchAccount> + Send + 'static,
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
/// graph step has finished either way. Marks it running under `claimer`
/// and counts the attempt. Returns `None` when nothing waits.
fn claim(database: &Connection, claimer: &str) -> Result<Option<Claimed>, String> {
    let tx = rusqlite::Transaction::new_unchecked(database, TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let row = tx
        .query_row(
            "SELECT project,repo,pool,drafted,runs,graph_state='waiting' FROM onboarding
             WHERE claimer IS NULL AND (graph_state='waiting'
                OR (mapping_state='waiting' AND graph_state IN ('done','failed')))
             ORDER BY created_ms,project LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, bool>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let Some((project, repo, pool, drafted, runs, graph)) = row else {
        return Ok(None);
    };
    let kind = if graph { Kind::Graph } else { Kind::Mapping };
    let now = now_ms();
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

/// The agent state name of one step attempt, unique per row and attempt.
fn run_name(claimed: &Claimed) -> String {
    let name = format!(
        "onboard-{}-{}-{}",
        claimed.kind.short(),
        claimed.project,
        claimed.runs
    );
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
    let waits = step == Step::Waiting;
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
        Ok(GraphEnding::Built) => Step::Done,
        Ok(GraphEnding::Failed(reason)) => Step::failed(reason),
        Ok(GraphEnding::TimedOut) => Step::failed(GRAPH_TIMED_OUT),
        Ok(GraphEnding::Stopped) => Step::Waiting,
        Err(_) => Step::failed(GRAPH_NOT_STARTED),
    }
}

/// Maps the project: collects the bounded evidence, saves the draft the
/// manifests alone give, then asks a tools-off agent on the pool for an
/// overview and the remaining fields and merges its answer into the gaps.
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
    let _ = store_draft(database, &work.claimer, claimed, &base);
    if draft::save(&layout, &base).is_err() {
        return (Step::failed(DRAFT_UNSAVED), base);
    }
    let Some(account) = (work.accounts)(&claimed.pool) else {
        return (Step::failed(NO_ACCOUNT), base);
    };
    let name = run_name(claimed);
    let prompt = draft::prompt(&evidence, &found);
    let run = ReadOnlyRun {
        name: &name,
        folder: &claimed.repo,
        prompt: &prompt,
        account: &account,
        wall_time: work.limits.mapping_time,
    };
    match work.runner.run_read_only(&run, beat) {
        Ok(ReadOnlyEnding::Answered(text)) => {
            let answer = draft::parse_answer(&text);
            let merged = draft::merge(&base, &answer, &found, &stack);
            if draft::write_overview(&layout, &answer.overview).is_err()
                || draft::save(&layout, &merged).is_err()
            {
                return (Step::failed(DRAFT_UNSAVED), base);
            }
            (Step::Done, merged)
        }
        Ok(ReadOnlyEnding::Failed(reason)) => (Step::failed(reason), base),
        Ok(ReadOnlyEnding::TimedOut) => (Step::failed(MAPPING_TIMED_OUT), base),
        Ok(ReadOnlyEnding::Stopped) | Err(RunError::Busy) => (Step::Waiting, base),
        Err(RunError::Account(_)) => (Step::failed(NO_ACCOUNT), base),
        Err(_) => (Step::failed(MAPPING_NOT_STARTED), base),
    }
}

/// The graphify executable this machine has, looked up again on each call:
/// the owner's `~/.local/bin/graphify` from a uv tool install, then the
/// Homebrew locations. Returns the canonical file, or `None`.
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

/// Starts onboarding for a running engine: steps claimed from the engine
/// database's `onboarding` table, agent state in `<runtime>/onboarding`, the
/// mapping account picked live from the settings files and graphify found
/// on every graph step.
pub fn start_for_engine(
    runtime: &Path,
    global: &Path,
    project: &Path,
    codex: &Path,
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
    let mut runner = ResearchRunner::new(&database, &state, codex, ResearchLimits::default())
        .map_err(|error| error.to_string())?;
    if let Some(claude) = claude {
        runner = runner
            .with_claude(claude)
            .map_err(|error| error.to_string())?;
    }
    let resolver = SettingsResolver::new(global, project, &projects, &database)?;
    Ok(OnboardingService::start(
        Arc::new(runner),
        move |pool: &CredentialRef| resolver.pool_pick(pool),
        installed_graphify,
        OnboardingLimits::default(),
        ENGINE_TICK,
    ))
}
