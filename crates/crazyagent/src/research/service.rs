use super::accounts::AccountPick;
#[cfg(unix)]
use super::accounts::SettingsResolver;
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
    /// Starts the loop. `resolve` maps a project id and a job harness to the
    /// project folder and the account picked for that harness from its pool,
    /// or `None` for a project the engine does not know yet.
    pub fn start<F>(runner: Arc<ResearchRunner>, resolve: F, tick: Duration) -> Self
    where
        F: Fn(&str, &str) -> Option<(PathBuf, AccountPick)> + Send + 'static,
    {
        assert!(!tick.is_zero());
        let stop = Arc::new(AtomicBool::new(false));
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

fn serve<F>(runner: &Arc<ResearchRunner>, resolve: &F, tick: Duration, stop: &AtomicBool)
where
    F: Fn(&str, &str) -> Option<(PathBuf, AccountPick)>,
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
            match resolve(&project, &harness) {
                None => {}
                Some((root, _)) if ResearchHarness::of(&harness).is_none() => {
                    let _ = jobs.resume_waiting(&project, &harness);
                    runs.extend(runner.spawn_ready(project, &harness, root, None));
                }
                Some((_, AccountPick::NeedsPool)) => {
                    let _ = jobs.park_waiting(&project, &harness, None);
                }
                Some((_, pick @ AccountPick::NoUsableAccount)) => {
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
            AccountPick::NeedsPool | AccountPick::NoUsableAccount => {}
        }
    }
    paused.map_or(Picked::Nothing, Picked::Paused)
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
/// connection, the runs going now, what finished status checks left for later removal and
/// the loop passes counted, so each store is asked for due runs every `STATUS_EVERY_TICKS`
/// passes.
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
    /// Opens the upkeep on the engine database and closes the runs an engine that stopped
    /// left open. Input: the database path. Output: the upkeep, idle for good when the
    /// database cannot be opened.
    fn open(database: &Path) -> Self {
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
    fn tick<F>(&mut self, runner: &Arc<ResearchRunner>, resolve: &F)
    where
        F: Fn(&str, &str) -> Option<(PathBuf, AccountPick)>,
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
    resolve: &'a F,
    db: &'a rusqlite::Connection,
    checks: &'a mut Vec<JoinHandle<Option<crate::roadmap_status::run::RunLeft>>>,
    upkeep: Upkeep,
    settling: bool,
}

#[cfg(unix)]
impl<F> crate::roadmap_status::Runs for StatusStarts<'_, F>
where
    F: Fn(&str, &str) -> Option<(PathBuf, AccountPick)>,
{
    fn start(&mut self, project: &ProjectId, claim: crate::roadmap_status::Claim) {
        let resolved = STATUS_HARNESSES
            .into_iter()
            .filter(|harness| self.runner.can_run(*harness))
            .map(|harness| (self.resolve)(project.as_str(), harness.id()));
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
                self.checks.push(std::thread::spawn(move || match upkeep {
                    Upkeep::Roadmap => {
                        crate::roadmap_status::run::check(&runner, &project, claim, &root, &account)
                    }
                    Upkeep::Overview => {
                        crate::project_overview::agent::write(
                            &runner, &project, claim, &root, &account,
                        );
                        None
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
        move |id: &str, harness: &str| resolver.resolve(id, harness),
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
        let resolve = |_: &str, _: &str| -> Option<(PathBuf, AccountPick)> { None };
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
        let resolve = |_: &str, _: &str| -> Option<(PathBuf, AccountPick)> { None };
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
        let resolve = |_: &str, _: &str| -> Option<(PathBuf, AccountPick)> { None };
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
        let resolve = |_: &str, _: &str| -> Option<(PathBuf, AccountPick)> { None };
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
        let resolve = |_: &str, _: &str| -> Option<(PathBuf, AccountPick)> { None };
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
}
