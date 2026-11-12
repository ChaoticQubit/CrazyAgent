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
/// harness resolves for the project, later when every fitting account is paused, and with
/// a plain failure when the pool has none.
#[cfg(unix)]
struct StatusStarts<'a, F> {
    runner: &'a Arc<ResearchRunner>,
    resolve: &'a F,
    db: &'a rusqlite::Connection,
    checks: &'a mut Vec<JoinHandle<Option<crate::roadmap_status::run::RunLeft>>>,
    upkeep: Upkeep,
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
            Picked::Nothing => match self.upkeep {
                Upkeep::Roadmap => {
                    let failed = Err(crate::roadmap_status::NO_ACCOUNT);
                    crate::roadmap_status::finish(self.db, project, claim, failed, now)
                }
                Upkeep::Overview => {
                    let words = crate::project_overview::NO_ACCOUNT;
                    crate::project_overview::fail(self.db, project, claim, words, now)
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
/// terminal host every agent in its own pane, and with the engine's agent
/// register each running job named as a holder of the account it signs in with.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
pub fn start_for_engine(
    runtime: &Path,
    global: &Path,
    project: &Path,
    codex: Option<&Path>,
    claude: Option<&Path>,
    terminal: Option<Arc<dyn crate::terminal::Host>>,
    agents: Option<Arc<crate::agents::Agents>>,
    lookup: Option<crate::paths::HarnessLookup>,
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
    if let Some(agents) = agents {
        runner = runner.with_agents(agents);
    }
    let resolver = SettingsResolver::new(global, project, &projects, &database)?;
    Ok(ResearchService::start(
        Arc::new(runner),
        move |id: &str, harness: &str| resolver.resolve(id, harness),
        ENGINE_TICK,
    ))
}

/// Refuses engine research where its 0700 state folders and private database cannot be
/// made or checked. Takes the runtime, settings files, harness paths, terminal host and
/// agent register, all unused. Returns `research_runtime_unavailable`.
#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
pub fn start_for_engine(
    _runtime: &Path,
    _global: &Path,
    _project: &Path,
    _codex: Option<&Path>,
    _claude: Option<&Path>,
    _terminal: Option<Arc<dyn crate::terminal::Host>>,
    _agents: Option<Arc<crate::agents::Agents>>,
    _lookup: Option<crate::paths::HarnessLookup>,
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
}
