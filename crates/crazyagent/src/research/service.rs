use super::accounts::{AccountPick, SettingsResolver};
use super::jobs::{JobError, ResearchJobs, ResearchStatus};
use super::runner::{
    ResearchHarness, ResearchLimits, ResearchRun, ResearchRunner, RunError, kind_label,
};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STALE_MS: u64 = 180_000;
const RUNS_MAX: usize = 256;
const ENGINE_TICK: Duration = Duration::from_secs(1);

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
/// harness can use, resumes them once one is assigned, and starts queued jobs
/// up to the per-project limit.
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
    while !stop.load(Ordering::SeqCst) {
        let _ = jobs.recover_stale(STALE_MS);
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
                Some((root, AccountPick::Account { account, .. })) => {
                    let _ = jobs.resume_waiting(&project, &harness);
                    runs.extend(runner.spawn_ready(project, &harness, root, Some(*account)));
                }
            }
        }
        runs.retain(|run| !run.is_finished());
        std::thread::sleep(tick);
    }
    for run in runs {
        let _ = run.join();
    }
}

/// Starts research for a running engine: jobs claimed from the engine
/// database's `research_jobs` table that project creation fills, agent state
/// in `<runtime>/research`, one folder per project in `<runtime>/projects`,
/// pool assignment read live from the engine's settings files and, with a
/// terminal host, every agent in its own pane.
pub fn start_for_engine(
    runtime: &Path,
    global: &Path,
    project: &Path,
    codex: &Path,
    claude: Option<&Path>,
    terminal: Option<Arc<dyn crate::terminal::Host>>,
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
    let mut runner = ResearchRunner::new(&database, &state, codex, ResearchLimits::default())
        .map_err(|error| error.to_string())?;
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
    let resolver = SettingsResolver::new(global, project, &projects, &database)?;
    Ok(ResearchService::start(
        Arc::new(runner),
        move |id: &str, harness: &str| resolver.resolve(id, harness),
        ENGINE_TICK,
    ))
}
