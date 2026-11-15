#[cfg(unix)]
use super::accounts::SettingsResolver;
use super::accounts::{AccountPick, Reach};
use super::jobs::{JobError, ResearchJobs, ResearchStatus};
#[cfg(unix)]
use super::runner::ResearchAccount;
#[cfg(unix)]
use super::runner::ResearchLimits;
use super::runner::{ResearchHarness, ResearchRun, ResearchRunner, RunError, kind_label, utc_date};
#[cfg(unix)]
use crate::protocol::ProjectId;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STALE_MS: u64 = 180_000;
const RUNS_MAX: usize = 256;
#[cfg(unix)]
const ENGINE_TICK: Duration = Duration::from_secs(1);
#[cfg(unix)]
const STATUS_EVERY_TICKS: u32 = 5;
#[cfg(unix)]
const CHECKS_MAX: usize = 8;
/// How soon a status run that found no account tries again while the engine still
/// re-proves earlier sign-ins (DEC-108) or an account check of this engine run holds its
/// profile: each settles within its own bound.
#[cfg(unix)]
const SETTLING_RETRY_MS: i64 = 10 * 1000;
#[cfg(unix)]
const _: () = assert!(
    SETTLING_RETRY_MS > 0
        && SETTLING_RETRY_MS < crate::roadmap_status::RETRY_MS
        && SETTLING_RETRY_MS < crate::project_overview::agent::RETRY_MS
);
/// The most finished status runs whose folders wait for the usage upkeep at once; past it
/// the oldest is removed at once.
#[cfg(unix)]
const LEFT_MAX: usize = 64;
#[cfg(unix)]
const STATUS_HARNESSES: [ResearchHarness; 2] =
    [ResearchHarness::Codex, ResearchHarness::ClaudeCode];

/// One research job as the UI polls it: plain title, plain status, an
/// optional plain reason, the report path inside the project folder and
/// timing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResearchProgress {
    pub job: i64,
    pub kind: String,
    pub title: String,
    pub status: &'static str,
    pub detail: Option<String>,
    pub report: Option<String>,
    pub elapsed_seconds: Option<u64>,
    pub output_bytes: u64,
}

/// The live research rows of `project`, oldest first. Polling this is the
/// status-event interface for now.
pub fn progress(jobs: &ResearchJobs, project: &str) -> Result<Vec<ResearchProgress>, JobError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    Ok(jobs
        .list(project)?
        .into_iter()
        .map(|job| {
            let detail = match job.status {
                ResearchStatus::Failed | ResearchStatus::NeedsPool => job.error.clone(),
                ResearchStatus::Queued => job
                    .error
                    .clone()
                    .filter(|line| super::runner::WAITING_LINES.contains(&line.as_str())),
                _ => None,
            };
            let elapsed_seconds = job.started_ms.map(|started| {
                let end = match job.status {
                    ResearchStatus::Running => now,
                    _ => job.finished_ms.unwrap_or(now),
                };
                end.saturating_sub(started) / 1000
            });
            ResearchProgress {
                job: job.id,
                title: kind_label(&job.kind),
                kind: job.kind,
                status: job.status.label(),
                detail,
                report: job.report_path,
                elapsed_seconds,
                output_bytes: job.output_bytes,
            }
        })
        .collect())
}

/// Background loop that keeps every project's research moving: for each
/// project and job harness it parks jobs whose pool has no account that
/// harness can use, or only accounts paused after a rate limit with the
/// `rate_limited` code and the pause's end, resumes them once one is free, and
/// starts queued jobs up to the per-project limit.
pub struct ResearchService {
    runner: Arc<ResearchRunner>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ResearchService {
    /// Starts the loop. `resolve` maps a project id, a job harness and a reach rule to the
    /// project folder and the account picked for that harness from its pool,
    /// or `None` for a project the engine does not know yet. Jobs and the tick pick
    /// with [`Reach::Unprobed`]; a started upkeep run picks again in its own thread.
    pub fn start<F>(runner: Arc<ResearchRunner>, resolve: F, tick: Duration) -> Self
    where
        F: Fn(&str, &str, Reach) -> Option<(PathBuf, AccountPick)> + Send + Sync + 'static,
    {
        assert!(!tick.is_zero());
        let stop = Arc::new(AtomicBool::new(false));
        let resolve = Arc::new(resolve);
        let worker = {
            let runner = runner.clone();
            let stop = stop.clone();
            std::thread::spawn(move || serve(&runner, &resolve, tick, &stop))
        };
        Self {
            runner,
            stop,
            worker: Some(worker),
        }
    }

    /// Stops the loop and every running agent; interrupted jobs go back to the
    /// queue without spending an attempt.
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

impl Drop for ResearchService {
    fn drop(&mut self) {
        self.halt();
    }
}

fn serve<F>(runner: &Arc<ResearchRunner>, resolve: &Arc<F>, tick: Duration, stop: &AtomicBool)
where
    F: Fn(&str, &str, Reach) -> Option<(PathBuf, AccountPick)> + Send + Sync + 'static,
{
    let mut runs: Vec<JoinHandle<Result<ResearchRun, RunError>>> = Vec::new();
    let Ok(jobs) = ResearchJobs::open(runner.database()) else {
        return;
    };
    #[cfg(unix)]
    let mut status = StatusUpkeep::open(runner.database());
    while !stop.load(Ordering::SeqCst) {
        let _ = jobs.recover_stale(STALE_MS, runner.attempts_max());
        for (project, harness) in jobs.work().unwrap_or_default() {
            if stop.load(Ordering::SeqCst) || runs.len() >= RUNS_MAX {
                break;
            }
            match resolve(&project, &harness, Reach::Unprobed) {
                None => {}
                Some((root, _)) if ResearchHarness::of(&harness).is_none() => {
                    let _ = jobs.resume_waiting(&project, &harness);
                    runs.extend(runner.spawn_ready(project, &harness, root, None));
                }
                Some((_, AccountPick::NeedsPool)) => {
                    let _ = jobs.park_waiting(&project, &harness, None);
                }
                Some((
                    _,
                    pick @ (AccountPick::NoUsableAccount | AccountPick::Unreached { .. }),
                )) => {
                    let _ = jobs.park_waiting(&project, &harness, Some(pick.label()));
                }
                Some((_, pick @ AccountPick::Paused { until_ms })) => {
                    let until = UNIX_EPOCH
                        + Duration::from_millis(u64::try_from(until_ms).unwrap_or_default());
                    let detail =
                        format!("rate_limited until {}: {}", utc_date(until), pick.label());
                    let _ = jobs.park_waiting(&project, &harness, Some(&detail));
                }
                Some((
                    root,
                    AccountPick::Account {
                        connection,
                        account,
                    },
                )) => {
                    let _ = jobs.resume_waiting(&project, &harness);
                    runs.extend(runner.spawn_ready(
                        project,
                        &harness,
                        root,
                        Some((connection, *account)),
                    ));
                }
            }
        }
        runs.retain(|run| !run.is_finished());
        #[cfg(unix)]
        status.tick(runner, resolve);
        std::thread::sleep(tick);
    }
    for run in runs {
        let _ = run.join();
    }
    #[cfg(unix)]
    status.join();
}

/// What a project's pool gives one roadmap status check: its folder and an account to run
/// on, the time the earliest paused account is free again, or nothing.
#[cfg(unix)]
#[derive(Debug)]
enum Picked {
    Account(PathBuf, Box<ResearchAccount>),
    Paused(i64),
    Nothing,
}

/// Picks what a status check runs on from what each harness resolved to, in harness order:
/// the first account, else the earliest end of a pause. Input: each harness's resolution.
/// Output: the pick.
#[cfg(unix)]
fn picked(resolved: impl Iterator<Item = Option<(PathBuf, AccountPick)>>) -> Picked {
    let mut paused: Option<i64> = None;
    for (root, pick) in resolved.flatten() {
        match pick {
            AccountPick::Account { account, .. } => return Picked::Account(root, account),
            AccountPick::Paused { until_ms } => {
                paused = Some(paused.map_or(until_ms, |earliest| earliest.min(until_ms)));
            }
            AccountPick::NeedsPool
            | AccountPick::NoUsableAccount
            | AccountPick::Unreached { .. } => {}
        }
    }
    paused.map_or(Picked::Nothing, Picked::Paused)
}

/// Picks again in a started upkeep run's own thread, never on the service tick, with each
/// loopback provider probed so a member whose provider refuses connects is passed over.
/// Inputs: the resolver, the project and the harnesses the tick asked, in order. Output: the
/// folder and the first reachable account, or none, when the run keeps the tick's pick and
/// its own reach check gives the owner the down provider's line.
#[cfg(unix)]
fn reachable<F>(
    resolve: &F,
    project: &ProjectId,
    harnesses: &[ResearchHarness],
) -> Option<(PathBuf, Box<ResearchAccount>)>
where
    F: Fn(&str, &str, Reach) -> Option<(PathBuf, AccountPick)>,
{
    let resolved = harnesses
        .iter()
        .map(|harness| resolve(project.as_str(), harness.id(), Reach::Reachable));
    match picked(resolved) {
        Picked::Account(root, account) => Some((root, account)),
        Picked::Paused(_) | Picked::Nothing => None,
    }
}

/// Which store a started run belongs to: the roadmap status check or the Overview tabs'
/// writing run. Both are claimed by their store's `tick` and run on the same accounts.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Upkeep {
    Roadmap,
    Overview,
}

/// The roadmap status and Overview tabs upkeep of the research loop: its own engine
/// connection, the runs going now, what finished status checks and Overview runs left for
/// later removal and the loop passes counted, so each store is asked for due runs every
/// `STATUS_EVERY_TICKS` passes.
#[cfg(unix)]
struct StatusUpkeep {
    db: Option<rusqlite::Connection>,
    checks: Vec<JoinHandle<Option<crate::roadmap_status::run::RunLeft>>>,
    left: Vec<crate::roadmap_status::run::RunLeft>,
    overview_runs: crate::project_overview::Started,
    passes: u32,
    failed: bool,
    overview_failed: bool,
}

#[cfg(unix)]
impl StatusUpkeep {
    /// Opens the upkeep on the engine database, closes the runs an engine that stopped
    /// left open and removes the status run folders an engine before 2.4.1 left in the
    /// research state beside the database. Input: the database path. Output: the upkeep,
    /// idle for good when the database cannot be opened.
    fn open(database: &Path) -> Self {
        if let Some(runtime) = database.parent() {
            let swept = crate::roadmap_status::run::sweep_unhashed(&runtime.join("research"));
            if swept > 0 {
                eprintln!("roadmap_status_old_runs_removed count={swept}");
            }
        }
        let db = database
            .parent()
            .and_then(|root| crate::questions::open(root).ok());
        if let Some(db) = &db
            && let Err(error) = crate::roadmap_status::close_orphans(db)
        {
            eprintln!(
                "roadmap_status_close_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
        }
        if let Some(db) = &db
            && let Err(error) = crate::project_overview::close_orphans(db)
        {
            eprintln!(
                "project_overview_close_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
        }
        Self {
            db,
            checks: Vec::new(),
            left: Vec::new(),
            overview_runs: crate::project_overview::Started::default(),
            passes: 0,
            failed: false,
            overview_failed: false,
        }
    }

    /// Takes what every finished check left in the research state and removes each run's
    /// folder and profile once the usage upkeep has had time to count the run; past
    /// `LEFT_MAX` the oldest goes at once. What still waits when the engine stops stays, is
    /// counted after the next start and goes with Delete everything.
    fn reap(&mut self) {
        let (ended, running): (Vec<_>, Vec<_>) = std::mem::take(&mut self.checks)
            .into_iter()
            .partition(|check| check.is_finished());
        self.checks = running;
        self.left.extend(
            ended
                .into_iter()
                .filter_map(|check| check.join().ok().flatten()),
        );
        let over = self.left.len().saturating_sub(LEFT_MAX);
        let mut waiting = Vec::new();
        for (index, left) in std::mem::take(&mut self.left).into_iter().enumerate() {
            if index < over || left.due() {
                left.remove();
            } else {
                waiting.push(left);
            }
        }
        self.left = waiting;
        assert!(self.left.len() <= LEFT_MAX);
    }

    /// Counts one loop pass and, on every `STATUS_EVERY_TICKS`th with room for more checks,
    /// starts the due ones. A store failure is logged once by its plain code. Inputs: the
    /// runner and the account resolver.
    fn tick<F>(&mut self, runner: &Arc<ResearchRunner>, resolve: &Arc<F>)
    where
        F: Fn(&str, &str, Reach) -> Option<(PathBuf, AccountPick)> + Send + Sync + 'static,
    {
        self.reap();
        self.passes = self.passes.wrapping_add(1);
        let Some(db) = &self.db else {
            return;
        };
        if !self.passes.is_multiple_of(STATUS_EVERY_TICKS) || self.checks.len() >= CHECKS_MAX {
            return;
        }
        let mut starts = StatusStarts {
            runner,
            resolve,
            db,
            checks: &mut self.checks,
            upkeep: Upkeep::Roadmap,
            settling: runner.reproving() || crate::storage::account_check_running(db),
        };
        let now = crate::team::now_ms();
        if let Err(error) = crate::roadmap_status::tick(db, &mut starts, now)
            && !self.failed
        {
            self.failed = true;
            eprintln!(
                "roadmap_status_tick_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
        }
        starts.upkeep = Upkeep::Overview;
        let steady = std::time::Instant::now();
        let ticked =
            crate::project_overview::tick(db, &mut starts, &mut self.overview_runs, steady, now);
        if let Err(error) = ticked
            && !self.overview_failed
        {
            self.overview_failed = true;
            eprintln!(
                "project_overview_tick_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
        }
        assert!(
            self.checks.len()
                <= CHECKS_MAX
                    + crate::roadmap_status::STARTS_PER_TICK_MAX
                    + crate::project_overview::STARTS_PER_TICK_MAX
        );
    }

    /// Waits for every running check; the runner's shutdown stops their agents first.
    fn join(self) {
        for check in self.checks {
            let _ = check.join();
        }
    }
}

/// Starts the runs a store hands out, for the store `upkeep` names: on the first account a
/// harness resolves for the project, later when every fitting account is paused, and when
/// the pool has none, soon again with no line while the engine still re-proves earlier
/// sign-ins or an account check of this engine run holds its profile (`settling`), else
/// later again under one plain line that stays until a run gets an account.
#[cfg(unix)]
struct StatusStarts<'a, F> {
    runner: &'a Arc<ResearchRunner>,
    resolve: &'a Arc<F>,
    db: &'a rusqlite::Connection,
    checks: &'a mut Vec<JoinHandle<Option<crate::roadmap_status::run::RunLeft>>>,
    upkeep: Upkeep,
    settling: bool,
}

#[cfg(unix)]
impl<F> crate::roadmap_status::Runs for StatusStarts<'_, F>
where
    F: Fn(&str, &str, Reach) -> Option<(PathBuf, AccountPick)> + Send + Sync + 'static,
{
    fn start(&mut self, project: &ProjectId, claim: crate::roadmap_status::Claim) {
        let harnesses: Vec<ResearchHarness> = STATUS_HARNESSES
            .into_iter()
            .filter(|harness| self.runner.can_run(*harness))
            .collect();
        let resolved = harnesses
            .iter()
            .map(|harness| (self.resolve)(project.as_str(), harness.id(), Reach::Unprobed));
        let now = crate::team::now_ms();
        let stored = match picked(resolved) {
            Picked::Account(root, account) => {
                let found = match self.upkeep {
                    Upkeep::Roadmap => {
                        crate::roadmap_status::account_found(self.db, project, claim)
                    }
                    Upkeep::Overview => {
                        crate::project_overview::account_found(self.db, project, claim)
                    }
                };
                if let Err(error) = found {
                    eprintln!(
                        "upkeep_line_kept store={:?} cause={}",
                        self.upkeep,
                        crate::protocol::plain_code(&error.0)
                    );
                }
                let (runner, project, upkeep) = (self.runner.clone(), project.clone(), self.upkeep);
                let resolve = self.resolve.clone();
                self.checks.push(std::thread::spawn(move || {
                    let (root, account) =
                        reachable(&*resolve, &project, &harnesses).unwrap_or((root, account));
                    match upkeep {
                        Upkeep::Roadmap => crate::roadmap_status::run::check(
                            &runner, &project, claim, &root, &account,
                        ),
                        Upkeep::Overview => crate::project_overview::agent::write(
                            &runner, &project, claim, &root, &account,
                        ),
                    }
                }));
                Ok(true)
            }
            Picked::Paused(until_ms) => match self.upkeep {
                Upkeep::Roadmap => {
                    crate::roadmap_status::requeue(self.db, project, claim, until_ms.max(now))
                }
                Upkeep::Overview => {
                    crate::project_overview::requeue(self.db, project, claim, until_ms.max(now))
                }
            },
            Picked::Nothing if self.settling => {
                let due_ms = now.saturating_add(SETTLING_RETRY_MS);
                match self.upkeep {
                    Upkeep::Roadmap => {
                        crate::roadmap_status::requeue(self.db, project, claim, due_ms)
                    }
                    Upkeep::Overview => {
                        crate::project_overview::requeue(self.db, project, claim, due_ms)
                    }
                }
            }
            Picked::Nothing => match self.upkeep {
                Upkeep::Roadmap => {
                    let due_ms = now.saturating_add(crate::roadmap_status::RETRY_MS);
                    let words = crate::roadmap_status::NO_ACCOUNT;
                    crate::roadmap_status::wait_for_account(self.db, project, claim, words, due_ms)
                }
                Upkeep::Overview => {
                    let due_ms = now.saturating_add(crate::project_overview::agent::RETRY_MS);
                    let words = crate::project_overview::NO_ACCOUNT;
                    crate::project_overview::wait_for_account(
                        self.db, project, claim, words, due_ms,
                    )
                }
            },
        };
        if let Err(error) = stored {
            eprintln!(
                "upkeep_start_failed store={:?} cause={}",
                self.upkeep,
                crate::protocol::plain_code(&error.0)
            );
        }
    }
}

/// Starts research for a running engine: jobs claimed from the engine
/// database's `research_jobs` table that project creation fills, agent state
/// in `<runtime>/research`, one folder per project in `<runtime>/projects`,
/// pool assignment read live from the engine's settings files, with a
/// terminal host every agent in its own pane, a look for that host again at each
/// agent's start while it has none, and with the engine's agent register each
/// running job named as a holder of the account it signs in with.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
pub fn start_for_engine(
    runtime: &Path,
    global: &Path,
    project: &Path,
    codex: Option<&Path>,
    claude: Option<&Path>,
    terminal: Option<Arc<dyn crate::terminal::Host>>,
    probe: Option<crate::team::runtime::TerminalProbe>,
    agents: Option<Arc<crate::agents::Agents>>,
    lookup: Option<crate::paths::HarnessLookup>,
    reproving: Option<Arc<AtomicBool>>,
) -> Result<ResearchService, String> {
    let runtime = runtime
        .canonicalize()
        .map_err(|error| format!("research runtime: {error}"))?;
    let state = runtime.join("research");
    let projects = runtime.join("projects");
    for directory in [&state, &projects] {
        match std::fs::DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("research folder: {error}")),
        }
        if !std::fs::symlink_metadata(directory)
            .map_err(|error| format!("research folder: {error}"))?
            .file_type()
            .is_dir()
        {
            return Err("research folder is not a directory".into());
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
    .map_err(|error| error.to_string())?
    .with_settings(global, project);
    if let Some(lookup) = lookup {
        runner = runner.with_harness_lookup(lookup);
    }
    if let Some(claude) = claude {
        runner = runner
            .with_claude(claude)
            .map_err(|error| error.to_string())?;
    }
    if let Some(host) = terminal {
        runner = runner
            .with_terminal(host)
            .map_err(|error| error.to_string())?;
    }
    if let Some(probe) = probe {
        runner = runner.with_terminal_probe(probe);
    }
    if let Some(agents) = agents {
        runner = runner.with_agents(agents);
    }
    if let Some(reproving) = reproving {
        runner = runner.with_reproof(reproving);
    }
    let resolver = SettingsResolver::new(global, project, &projects, &database)?;
    Ok(ResearchService::start(
        Arc::new(runner),
        move |id: &str, harness: &str, reach: Reach| resolver.resolve_with(id, harness, reach),
        ENGINE_TICK,
    ))
}

/// Refuses engine research where its 0700 state folders and private database cannot be
/// made or checked. Takes the runtime, settings files, harness paths, terminal host, its
/// probe and agent register, all unused. Returns `research_runtime_unavailable`.
#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
pub fn start_for_engine(
    _runtime: &Path,
    _global: &Path,
    _project: &Path,
    _codex: Option<&Path>,
    _claude: Option<&Path>,
    _terminal: Option<Arc<dyn crate::terminal::Host>>,
    _probe: Option<crate::team::runtime::TerminalProbe>,
    _agents: Option<Arc<crate::agents::Agents>>,
    _lookup: Option<crate::paths::HarnessLookup>,
    _reproving: Option<Arc<AtomicBool>>,
) -> Result<ResearchService, String> {
    Err("research_runtime_unavailable".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_finished_status_runs_folder_stays_until_usage_had_time_to_count_it() {
        use crate::roadmap_status::run::{RUN_KEPT, RunLeft};
        use std::time::Instant;
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().canonicalize().unwrap();
        std::fs::create_dir(state.join("profiles")).unwrap();
        let made = |name: &str, ended: Instant| {
            let folder = state.join("agents").join(name);
            std::fs::create_dir_all(folder.join("home")).unwrap();
            let profile = state.join("profiles").join(format!("{name}.sb"));
            std::fs::write(&profile, "(version 1)").unwrap();
            (
                folder.clone(),
                profile.clone(),
                RunLeft::at(folder, profile, ended),
            )
        };
        let now = Instant::now();
        let long_ago = now.checked_sub(RUN_KEPT).unwrap();
        let (new_folder, new_profile, new_left) = made("roadmap-status-demo-2", now);
        let (old_folder, old_profile, old_left) = made("roadmap-status-demo-1", long_ago);
        assert!(!new_left.due() && old_left.due());
        let mut upkeep = StatusUpkeep {
            db: None,
            checks: vec![
                std::thread::spawn(move || Some(old_left)),
                std::thread::spawn(move || Some(new_left)),
                std::thread::spawn(|| None),
            ],
            left: Vec::new(),
            overview_runs: crate::project_overview::Started::default(),
            passes: 0,
            failed: false,
            overview_failed: false,
        };
        for _ in 0..500 {
            if upkeep.checks.iter().all(|check| check.is_finished()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        upkeep.reap();
        assert!(upkeep.checks.is_empty());
        assert_eq!(upkeep.left.len(), 1, "only the run that just ended waits");
        assert!(new_folder.join("home").is_dir() && new_profile.is_file());
        assert!(std::fs::symlink_metadata(&old_folder).is_err());
        assert!(std::fs::symlink_metadata(&old_profile).is_err());

        let crowd: Vec<_> = (0..=LEFT_MAX)
            .map(|index| made(&format!("roadmap-status-crowd-{index}"), now))
            .collect();
        upkeep.left = Vec::new();
        let mut folders = Vec::new();
        for (folder, _, left) in crowd {
            folders.push(folder);
            upkeep.left.push(left);
        }
        upkeep.reap();
        assert_eq!(upkeep.left.len(), LEFT_MAX);
        assert!(
            std::fs::symlink_metadata(&folders[0]).is_err(),
            "the oldest went"
        );
        assert!(folders[1..].iter().all(|folder| folder.is_dir()));
    }

    fn account() -> AccountPick {
        AccountPick::Account {
            connection: crate::protocol::CredentialRef::new("pool-account").unwrap(),
            account: Box::new(ResearchAccount::Endpoint {
                base_url: "http://127.0.0.1:9".into(),
                model: "local".into(),
                key: None,
            }),
        }
    }

    #[test]
    fn a_status_check_runs_on_the_first_account_any_harness_resolves() {
        let root = PathBuf::from("/projects/alpha");
        let resolved = vec![
            None,
            Some((root.clone(), AccountPick::Paused { until_ms: 900 })),
            Some((root.clone(), account())),
        ];
        match picked(resolved.into_iter()) {
            Picked::Account(folder, account) => {
                let ResearchAccount::Endpoint { model, .. } = *account else {
                    panic!("expected the endpoint account, got {account:?}");
                };
                assert_eq!((folder, model.as_str()), (root, "local"));
            }
            other => panic!("expected an account, got {other:?}"),
        }
    }

    #[test]
    fn a_status_check_waits_for_the_earliest_pause_and_has_nothing_without_an_account() {
        let root = PathBuf::from("/projects/alpha");
        let paused = vec![
            Some((root.clone(), AccountPick::Paused { until_ms: 900 })),
            Some((root.clone(), AccountPick::NoUsableAccount)),
            Some((root.clone(), AccountPick::Paused { until_ms: 400 })),
        ];
        assert!(matches!(picked(paused.into_iter()), Picked::Paused(400)));
        let none = vec![
            None,
            Some((root.clone(), AccountPick::NeedsPool)),
            Some((root, AccountPick::NoUsableAccount)),
        ];
        assert!(matches!(picked(none.into_iter()), Picked::Nothing));
    }

    /// An engine folder with a private engine database, a runner with no harness, so every
    /// run a status store hands out finds no account, and a connection with the roadmap
    /// status and Overview tables made. Input: the engine's re-proof flag, if any. Output:
    /// the folder guard, the runner and the connection.
    fn engine_without_accounts(
        reproving: Option<Arc<AtomicBool>>,
    ) -> (tempfile::TempDir, Arc<ResearchRunner>, rusqlite::Connection) {
        let private = || std::fs::Permissions::from_mode(0o700);
        let temp = tempfile::Builder::new()
            .prefix("ca-upkeep-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(temp.path(), private()).unwrap();
        let engine = temp.path().canonicalize().unwrap().join("engine");
        let state = engine.join("research");
        for folder in [&engine, &state] {
            std::fs::create_dir(folder).unwrap();
            std::fs::set_permissions(folder, private()).unwrap();
        }
        let db = crate::questions::open(&engine).unwrap();
        db.execute_batch("PRAGMA user_version=1").unwrap();
        let database = engine.join("engine.db");
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
        crate::roadmap_status::initialize(&db).unwrap();
        crate::project_overview::initialize(&db).unwrap();
        let mut runner =
            ResearchRunner::without_codex(&database, &state, ResearchLimits::default()).unwrap();
        if let Some(reproving) = reproving {
            runner = runner.with_reproof(reproving);
        }
        assert!(
            STATUS_HARNESSES
                .into_iter()
                .all(|harness| !runner.can_run(harness))
        );
        (temp, Arc::new(runner), db)
    }

    /// The stored state of one project's status run: whether it waits to run, when it is
    /// due, whether it runs and the line it shows. Inputs: the connection, the table and the
    /// column holding the line. Output: the four values.
    fn waiting_row(
        db: &rusqlite::Connection,
        table: &str,
        line: &str,
    ) -> (bool, i64, Option<i64>, Option<String>) {
        db.query_row(
            &format!("SELECT stale,due_ms,running_ms,{line} FROM {table} WHERE project='alpha'"),
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
    }

    #[test]
    fn an_overview_run_with_no_account_keeps_waiting_for_one_under_one_plain_line() {
        let (_temp, runner, db) = engine_without_accounts(None);
        let project = ProjectId::new("alpha").unwrap();
        crate::project_overview::refresh(&db, &project).unwrap();
        let resolve =
            Arc::new(|_: &str, _: &str, _: Reach| -> Option<(PathBuf, AccountPick)> { None });
        let mut checks = Vec::new();
        let mut starts = StatusStarts {
            runner: &runner,
            resolve: &resolve,
            db: &db,
            checks: &mut checks,
            upkeep: Upkeep::Overview,
            settling: false,
        };
        let now = crate::team::now_ms();
        let mut noted = crate::project_overview::Started::default();
        let steady = std::time::Instant::now();
        let started =
            crate::project_overview::tick(&db, &mut starts, &mut noted, steady, now).unwrap();
        assert_eq!(started, 1);
        assert!(checks.is_empty(), "no agent starts without an account");
        let (stale, due_ms, running, line) = waiting_row(&db, "project_overview", "failure");
        assert_eq!(line.as_deref(), Some(crate::project_overview::NO_ACCOUNT));
        assert_eq!(running, None);
        assert!(
            stale,
            "the run waits for an account instead of ending for good"
        );
        assert!(due_ms > now, "the next try is a while away");
    }

    #[test]
    fn a_roadmap_check_with_no_account_keeps_waiting_for_one_under_one_plain_line() {
        let (_temp, runner, db) = engine_without_accounts(None);
        let project = ProjectId::new("alpha").unwrap();
        crate::roadmap_status::refresh(&db, &project).unwrap();
        let resolve =
            Arc::new(|_: &str, _: &str, _: Reach| -> Option<(PathBuf, AccountPick)> { None });
        let mut checks = Vec::new();
        let mut starts = StatusStarts {
            runner: &runner,
            resolve: &resolve,
            db: &db,
            checks: &mut checks,
            upkeep: Upkeep::Roadmap,
            settling: false,
        };
        let now = crate::team::now_ms();
        let started = crate::roadmap_status::tick(&db, &mut starts, now).unwrap();
        assert_eq!(started, 1);
        assert!(checks.is_empty(), "no agent starts without an account");
        let (stale, due_ms, running, line) = waiting_row(&db, "roadmap_status", "problem");
        assert_eq!(line.as_deref(), Some(crate::roadmap_status::NO_ACCOUNT));
        assert_eq!(running, None);
        assert!(
            stale,
            "the check waits for an account instead of ending for good"
        );
        assert!(due_ms > now, "the next try is a while away");
    }

    #[test]
    fn a_status_run_with_no_account_waits_without_a_line_while_the_engine_re_proves_sign_ins() {
        let flag = Arc::new(AtomicBool::new(true));
        let (_temp, runner, db) = engine_without_accounts(Some(flag.clone()));
        assert!(runner.reproving());
        let project = ProjectId::new("alpha").unwrap();
        crate::project_overview::refresh(&db, &project).unwrap();
        crate::roadmap_status::refresh(&db, &project).unwrap();
        let resolve =
            Arc::new(|_: &str, _: &str, _: Reach| -> Option<(PathBuf, AccountPick)> { None });
        let mut checks = Vec::new();
        let mut starts = StatusStarts {
            runner: &runner,
            resolve: &resolve,
            db: &db,
            checks: &mut checks,
            upkeep: Upkeep::Roadmap,
            settling: runner.reproving(),
        };
        let now = crate::team::now_ms();
        assert_eq!(
            crate::roadmap_status::tick(&db, &mut starts, now).unwrap(),
            1
        );
        starts.upkeep = Upkeep::Overview;
        let mut noted = crate::project_overview::Started::default();
        let steady = std::time::Instant::now();
        let started =
            crate::project_overview::tick(&db, &mut starts, &mut noted, steady, now).unwrap();
        assert_eq!(started, 1);
        assert!(checks.is_empty(), "no agent starts without an account");
        for (table, line) in [
            ("roadmap_status", "problem"),
            ("project_overview", "failure"),
        ] {
            let (stale, due_ms, running, shown) = waiting_row(&db, table, line);
            assert_eq!(
                shown, None,
                "{table} shows no line while sign-ins are re-proved"
            );
            assert_eq!(running, None);
            assert!(stale && due_ms > now, "{table} tries again soon");
            assert!(due_ms <= now.saturating_add(SETTLING_RETRY_MS) + 1000);
        }
        flag.store(false, Ordering::SeqCst);
        assert!(!runner.reproving(), "the wait ends with the re-proof");
    }

    fn fence_of_an_account_check(db: &rusqlite::Connection, phase: &str) {
        db.execute_batch(
            "CREATE TABLE owner_meta(singleton INTEGER PRIMARY KEY, schema_version INTEGER,
               runtime_state TEXT, db_instance TEXT, engine_epoch TEXT);
             INSERT INTO owner_meta VALUES(1,1,'active','instance','epoch-now');
             CREATE TABLE native_auth_operations(id TEXT PRIMARY KEY, phase TEXT);
             CREATE TABLE native_auth_fences(profile_key TEXT PRIMARY KEY, operation_id TEXT,
               db_instance TEXT, engine_epoch TEXT);
             INSERT INTO native_auth_fences VALUES('profile','check','instance','epoch-now');",
        )
        .unwrap();
        db.execute(
            "INSERT INTO native_auth_operations VALUES('check',?1)",
            [phase],
        )
        .unwrap();
    }

    #[test]
    fn a_status_run_with_no_account_waits_without_a_line_while_an_account_check_of_this_engine_runs()
     {
        let (temp, runner, db) = engine_without_accounts(None);
        fence_of_an_account_check(&db, "checking_account");
        let project = ProjectId::new("alpha").unwrap();
        let resolve =
            Arc::new(|_: &str, _: &str, _: Reach| -> Option<(PathBuf, AccountPick)> { None });
        let engine = temp.path().canonicalize().unwrap().join("engine");
        let mut upkeep = StatusUpkeep::open(&engine.join("engine.db"));
        let pass = |upkeep: &mut StatusUpkeep| {
            for _ in 0..STATUS_EVERY_TICKS {
                upkeep.tick(&runner, &resolve);
            }
        };
        let now = crate::team::now_ms();
        crate::project_overview::refresh(&db, &project).unwrap();
        crate::roadmap_status::refresh(&db, &project).unwrap();
        pass(&mut upkeep);
        for (table, line) in [
            ("roadmap_status", "problem"),
            ("project_overview", "failure"),
        ] {
            let (stale, due_ms, running, shown) = waiting_row(&db, table, line);
            assert_eq!(
                shown, None,
                "{table} shows no line while an account is checked"
            );
            assert!(
                stale && running.is_none() && due_ms > now,
                "{table} tries again"
            );
        }

        db.execute(
            "UPDATE native_auth_fences SET engine_epoch='epoch-before'",
            [],
        )
        .unwrap();
        crate::project_overview::refresh(&db, &project).unwrap();
        crate::roadmap_status::refresh(&db, &project).unwrap();
        pass(&mut upkeep);
        let (_, _, _, overview) = waiting_row(&db, "project_overview", "failure");
        let (_, _, _, roadmap) = waiting_row(&db, "roadmap_status", "problem");
        assert_eq!(
            overview.as_deref(),
            Some(crate::project_overview::NO_ACCOUNT)
        );
        assert_eq!(roadmap.as_deref(), Some(crate::roadmap_status::NO_ACCOUNT));
    }

    #[test]
    fn a_status_run_with_no_account_shows_its_line_when_the_account_check_of_this_engine_was_left_uncertain()
     {
        let (temp, runner, db) = engine_without_accounts(None);
        fence_of_an_account_check(&db, "uncertain");
        let project = ProjectId::new("alpha").unwrap();
        let resolve =
            Arc::new(|_: &str, _: &str, _: Reach| -> Option<(PathBuf, AccountPick)> { None });
        let engine = temp.path().canonicalize().unwrap().join("engine");
        let mut upkeep = StatusUpkeep::open(&engine.join("engine.db"));
        crate::project_overview::refresh(&db, &project).unwrap();
        crate::roadmap_status::refresh(&db, &project).unwrap();
        for _ in 0..STATUS_EVERY_TICKS {
            upkeep.tick(&runner, &resolve);
        }
        let (_, _, _, overview) = waiting_row(&db, "project_overview", "failure");
        let (_, _, _, roadmap) = waiting_row(&db, "roadmap_status", "problem");
        assert_eq!(
            overview.as_deref(),
            Some(crate::project_overview::NO_ACCOUNT),
            "a fence kept only for the next engine start is no running check"
        );
        assert_eq!(roadmap.as_deref(), Some(crate::roadmap_status::NO_ACCOUNT));
    }

    /// An engine whose pool for project alpha holds one keyless loopback endpoint member per
    /// port, in order, with a runner that can run Codex and a hidden-folders table that cannot
    /// be read, so a status run that gets past its reach check stops at the hidden-folders
    /// check before any agent starts. Input: the ports. Output: the folder guard, the runner,
    /// the connection and the resolver over the pool.
    #[cfg(target_os = "macos")]
    fn engine_with_loopback_pool(
        ports: &[u16],
    ) -> (
        tempfile::TempDir,
        Arc<ResearchRunner>,
        rusqlite::Connection,
        SettingsResolver,
    ) {
        let private = || std::fs::Permissions::from_mode(0o700);
        let temp = tempfile::Builder::new()
            .prefix("ca-upkeep-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(temp.path(), private()).unwrap();
        let base = temp.path().canonicalize().unwrap();
        let engine = base.join("engine");
        let state = engine.join("research");
        let projects = base.join("projects");
        for folder in [&engine, &state, &projects] {
            std::fs::create_dir(folder).unwrap();
            std::fs::set_permissions(folder, private()).unwrap();
        }
        std::fs::create_dir_all(
            projects
                .join("alpha")
                .join(crate::projects_home::CRAZYAGENT_DIRECTORY),
        )
        .unwrap();
        let db = crate::questions::open(&engine).unwrap();
        db.execute_batch("PRAGMA user_version=1; CREATE TABLE project_hidden_paths(unreadable)")
            .unwrap();
        let database = engine.join("engine.db");
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
        crate::roadmap_status::initialize(&db).unwrap();
        crate::project_overview::initialize(&db).unwrap();
        let codex = engine.join("codex");
        std::fs::write(&codex, "").unwrap();
        let runner =
            ResearchRunner::new(&database, &state, &codex, ResearchLimits::default()).unwrap();
        assert!(runner.can_run(ResearchHarness::Codex));
        let accounts: Vec<String> = (0..ports.len())
            .map(|n| format!("\"account-{n}\""))
            .collect();
        let members: Vec<String> = (0..ports.len())
            .map(|n| format!("\"member-{n}\""))
            .collect();
        let mut text = format!(
            "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [{}]\n",
            accounts.join(", ")
        );
        for (n, port) in ports.iter().enumerate() {
            text.push_str(&format!("\n[[model_access.connections]]\nid = \"member-{n}\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"account-{n}\"\nendpoint = {{ kind = \"http\", url = \"http://127.0.0.1:{port}/v1\" }}\nmodel = {{ id = \"local-model\", author = \"openai-compatible\" }}\nauth = {{ method = \"none\" }}\n"));
        }
        text.push_str(&format!("\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [{}]\nprojects = [\"alpha\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n", members.join(", ")));
        let global = base.join("global.toml");
        let project = base.join("project.toml");
        std::fs::write(&global, text).unwrap();
        std::fs::write(&project, "").unwrap();
        let resolver = SettingsResolver::new(&global, &project, &projects, &database).unwrap();
        (temp, Arc::new(runner), db, resolver)
    }

    /// Starts one roadmap check and one overview run for alpha through the research loop's
    /// starts, on the resolve the engine gives the loop, waits for both runs and reads the
    /// line each stored. Inputs: the runner, the connection and the resolver. Output: the
    /// roadmap check's problem and the overview run's failure.
    #[cfg(target_os = "macos")]
    fn upkeep_lines(
        runner: &Arc<ResearchRunner>,
        db: &rusqlite::Connection,
        resolver: &SettingsResolver,
    ) -> [Option<String>; 2] {
        let resolver = resolver.clone();
        let resolve = Arc::new(move |id: &str, harness: &str, reach: Reach| {
            resolver.resolve_with(id, harness, reach)
        });
        let resolve = &resolve;
        let project = ProjectId::new("alpha").unwrap();
        crate::roadmap_status::refresh(db, &project).unwrap();
        crate::project_overview::refresh(db, &project).unwrap();
        let mut checks = Vec::new();
        let mut starts = StatusStarts {
            runner,
            resolve,
            db,
            checks: &mut checks,
            upkeep: Upkeep::Roadmap,
            settling: false,
        };
        let now = crate::team::now_ms();
        assert_eq!(
            crate::roadmap_status::tick(db, &mut starts, now).unwrap(),
            1
        );
        starts.upkeep = Upkeep::Overview;
        let mut noted = crate::project_overview::Started::default();
        let steady = std::time::Instant::now();
        let started =
            crate::project_overview::tick(db, &mut starts, &mut noted, steady, now).unwrap();
        assert_eq!(started, 1);
        assert_eq!(checks.len(), 2, "both runs start on the tick's pick");
        for check in checks {
            assert!(check.join().unwrap().is_none(), "no agent ran");
        }
        [
            waiting_row(db, "roadmap_status", "problem").3,
            waiting_row(db, "project_overview", "failure").3,
        ]
    }

    /// A roadmap check and an overview run whose pool lists a member with a stopped loopback
    /// provider first run on the next member (DEC-091 addendum): the tick starts them with
    /// no connect, each run picks again in its own thread and gets past its reach check, here
    /// to the hidden-folders check, so no reach line is stored.
    #[cfg(target_os = "macos")]
    #[test]
    fn status_runs_pick_again_in_their_thread_and_take_the_next_member_when_the_first_one_is_down()
    {
        let (_held, down) = crate::spawn_gate::refusing_tcp_port().unwrap();
        let up = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
        let up_port = up.local_addr().unwrap().port();
        let (_temp, runner, db, resolver) = engine_with_loopback_pool(&[down, up_port]);
        assert_eq!(
            upkeep_lines(&runner, &db, &resolver),
            [
                Some(crate::roadmap_status::run::HIDDEN_UNREADABLE.to_owned()),
                Some(crate::project_overview::agent::HIDDEN_UNREADABLE.to_owned()),
            ]
        );
    }

    /// A roadmap check and an overview run whose pool members all have stopped loopback
    /// providers store the first one's line, the line they stored before failover.
    #[cfg(target_os = "macos")]
    #[test]
    fn status_runs_with_every_members_provider_down_store_the_first_ones_line() {
        let (_first, first) = crate::spawn_gate::refusing_tcp_port().unwrap();
        let (_second, second) = crate::spawn_gate::refusing_tcp_port().unwrap();
        let (_temp, runner, db, resolver) = engine_with_loopback_pool(&[first, second]);
        let line = format!(
            "Can't reach the model provider at 127.0.0.1:{first}. Start it or pick another account in this pool, then try again."
        );
        assert_eq!(
            upkeep_lines(&runner, &db, &resolver),
            [Some(line.clone()), Some(line)]
        );
    }
    /// The status upkeep's start removes every status run folder an engine before 2.4.1
    /// left, named without the run-id hash, with its profile, whatever project it held: no
    /// run of this engine has such a name, and Delete everything no longer matches it.
    /// Status runs named with the hash, research job runs and other folders stay
    /// (RV-DS241-45 r2, rv-secreview F1).
    #[test]
    fn the_status_upkeep_start_removes_status_runs_older_engines_left_and_nothing_else() {
        let temp = tempfile::tempdir().unwrap();
        let engine = temp.path().canonicalize().unwrap();
        let state = engine.join("research");
        std::fs::create_dir_all(state.join("profiles")).unwrap();
        let demo = crate::protocol::ProjectId::new("demo").unwrap();
        let hashed = format!("{}-1700000000000", crate::roadmap_status::run_stem(&demo));
        let made = |name: &str| {
            std::fs::create_dir_all(state.join("agents").join(name).join("home")).unwrap();
            let profile = state.join("profiles").join(format!("{name}.sb"));
            std::fs::write(profile, "(version 1)").unwrap();
        };
        let old = [
            "roadmap-status-demo-1700000000000",
            "roadmap-status-v1_2-1700000000001",
            "roadmap-status-team_one-5",
        ];
        let kept = [hashed.as_str(), "job-1-1", "job-2-1-3"];
        for name in old.iter().chain(&kept) {
            made(name);
        }
        let _upkeep = StatusUpkeep::open(&engine.join("engine.db"));
        for name in old {
            let folder = state.join("agents").join(name);
            let profile = state.join("profiles").join(format!("{name}.sb"));
            assert!(std::fs::symlink_metadata(folder).is_err(), "{name}");
            assert!(std::fs::symlink_metadata(profile).is_err(), "{name}");
        }
        for name in kept {
            assert!(state.join("agents").join(name).join("home").is_dir(), "{name}");
            let profile = state.join("profiles").join(format!("{name}.sb"));
            assert!(profile.is_file(), "{name}");
        }
    }
}
