use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};
use std::fmt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PROJECT_BYTES_MAX: usize = 128;
const KIND_BYTES_MAX: usize = 64;
const HARNESS_BYTES_MAX: usize = 64;
const CLAIMER_BYTES_MAX: usize = 128;
const ERROR_BYTES_MAX: usize = 512;
const REPORT_PATH_BYTES_MAX: usize = 512;
const RUNNING_MAX: u32 = 64;
const ATTEMPTS_MAX: u32 = 10;

const COLUMNS: &str = "id, project, kind, harness, tools, status, attempts, claimer, report_path, error, cancel_requested, output_bytes, created_ms, started_ms, heartbeat_ms, finished_ms";

/// Where a research job stands, with the plain label the UI shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResearchStatus {
    Queued,
    NeedsPool,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl ResearchStatus {
    /// The plain-language status for the main flow.
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::NeedsPool => "Needs a pool",
            Self::Running => "Running",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::NeedsPool => "needs_pool",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(code: &str) -> Option<Self> {
        [
            Self::Queued,
            Self::NeedsPool,
            Self::Running,
            Self::Done,
            Self::Failed,
            Self::Cancelled,
        ]
        .into_iter()
        .find(|status| status.code() == code)
    }
}

/// One durable research job: a project's toggle kind run by one harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResearchJob {
    pub id: i64,
    pub project: String,
    pub kind: String,
    pub harness: String,
    pub tools: bool,
    pub status: ResearchStatus,
    pub attempts: u32,
    pub claimer: Option<String>,
    pub report_path: Option<String>,
    pub error: Option<String>,
    pub cancel_requested: bool,
    pub output_bytes: u64,
    pub created_ms: u64,
    pub started_ms: Option<u64>,
    pub heartbeat_ms: Option<u64>,
    pub finished_ms: Option<u64>,
}

/// A refused or failed job-store operation.
#[derive(Debug)]
pub enum JobError {
    Invalid(&'static str),
    NotHeld,
    Storage(rusqlite::Error),
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "research job refused: {reason}"),
            Self::NotHeld => write!(f, "research job is not held by this runner"),
            Self::Storage(error) => write!(f, "research job storage: {error}"),
        }
    }
}

impl std::error::Error for JobError {}

impl From<rusqlite::Error> for JobError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Storage(value)
    }
}

/// The minimal durable research queue the runner claims from. It is the seam
/// to the project-setup job table: one SQLite table, atomic single-statement
/// claims, and every transition guarded by the claimer that holds the job.
pub struct ResearchJobs {
    connection: Connection,
}

impl ResearchJobs {
    /// Opens or creates the queue database at `path`, refusing a symlink.
    /// The table matches the engine's own `research_jobs`, so `path` may be
    /// the engine database that project creation queues jobs in.
    pub fn open(path: &Path) -> Result<Self, JobError> {
        let parent = path
            .parent()
            .ok_or(JobError::Invalid("database path"))?
            .canonicalize()
            .map_err(|_| JobError::Invalid("database folder"))?;
        let name = path.file_name().ok_or(JobError::Invalid("database path"))?;
        let connection = Connection::open_with_flags(
            parent.join(name),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS research_jobs (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 project TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 harness TEXT NOT NULL,
                 tools INTEGER NOT NULL,
                 status TEXT NOT NULL,
                 attempts INTEGER NOT NULL DEFAULT 0,
                 claimer TEXT,
                 report_path TEXT,
                 error TEXT,
                 cancel_requested INTEGER NOT NULL DEFAULT 0,
                 output_bytes INTEGER NOT NULL DEFAULT 0,
                 created_ms INTEGER NOT NULL,
                 started_ms INTEGER,
                 heartbeat_ms INTEGER,
                 finished_ms INTEGER
             );
             CREATE INDEX IF NOT EXISTS research_jobs_project_status
                 ON research_jobs(project, status, id);",
        )?;
        Ok(Self { connection })
    }

    /// Queues one job and returns its id.
    pub fn enqueue(
        &self,
        project: &str,
        kind: &str,
        harness: &str,
        tools: bool,
    ) -> Result<i64, JobError> {
        bounded(project, PROJECT_BYTES_MAX)?;
        bounded(harness, HARNESS_BYTES_MAX)?;
        if !valid_kind(kind) {
            return Err(JobError::Invalid("kind must be 1..64 of a-z, 0-9, _ or -"));
        }
        self.connection.execute(
            "INSERT INTO research_jobs (project, kind, harness, tools, status, created_ms)
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5)",
            params![project, kind, harness, tools, now_ms()],
        )?;
        let id = self.connection.last_insert_rowid();
        assert!(id > 0);
        Ok(id)
    }

    /// Atomically moves the oldest queued job of `project` to running for
    /// `claimer`, unless `running_max` jobs of that project already run.
    pub fn claim(
        &self,
        project: &str,
        claimer: &str,
        running_max: u32,
    ) -> Result<Option<ResearchJob>, JobError> {
        self.claim_where(project, None, claimer, running_max)
    }

    /// Like [`ResearchJobs::claim`], but only takes a job for `harness`; the
    /// running limit still counts every harness of the project.
    pub fn claim_harness(
        &self,
        project: &str,
        harness: &str,
        claimer: &str,
        running_max: u32,
    ) -> Result<Option<ResearchJob>, JobError> {
        bounded(harness, HARNESS_BYTES_MAX)?;
        self.claim_where(project, Some(harness), claimer, running_max)
    }

    fn claim_where(
        &self,
        project: &str,
        harness: Option<&str>,
        claimer: &str,
        running_max: u32,
    ) -> Result<Option<ResearchJob>, JobError> {
        bounded(claimer, CLAIMER_BYTES_MAX)?;
        if !(1..=RUNNING_MAX).contains(&running_max) {
            return Err(JobError::Invalid("running limit must be 1..64"));
        }
        let now = now_ms();
        let job = self
            .connection
            .query_row(
                &format!(
                    "UPDATE research_jobs
                     SET status = 'running', claimer = ?2, attempts = attempts + 1,
                         started_ms = ?3, heartbeat_ms = ?3, finished_ms = NULL,
                         error = NULL, output_bytes = 0
                     WHERE id = (SELECT id FROM research_jobs
                                 WHERE project = ?1 AND status = 'queued'
                                   AND (?5 IS NULL OR harness = ?5)
                                 ORDER BY id LIMIT 1)
                       AND (SELECT count(*) FROM research_jobs
                            WHERE project = ?1 AND status = 'running') < ?4
                     RETURNING {COLUMNS}"
                ),
                params![project, claimer, now, running_max, harness],
                job_from_row,
            )
            .optional()?;
        if let Some(job) = &job {
            assert_eq!(job.status, ResearchStatus::Running);
            assert_eq!(job.claimer.as_deref(), Some(claimer));
            assert!(harness.is_none_or(|harness| job.harness == harness));
            assert!(job.attempts >= 1);
        }
        Ok(job)
    }

    /// Records liveness and output size for the claim `held`, the job a claim
    /// returned; returns whether cancel was requested. A claim is fenced by its
    /// claimer and attempt, so a run whose claim stale recovery took back is no
    /// longer held once the job is claimed again, even by the same runner.
    pub fn heartbeat(&self, held: &ResearchJob, output_bytes: u64) -> Result<bool, JobError> {
        let claimer = held.claimer.as_deref().ok_or(JobError::NotHeld)?;
        let cancel = self
            .connection
            .query_row(
                "UPDATE research_jobs SET heartbeat_ms = ?4, output_bytes = ?5
                 WHERE id = ?1 AND claimer = ?2 AND attempts = ?3 AND status = 'running'
                 RETURNING cancel_requested",
                params![
                    held.id,
                    claimer,
                    held.attempts,
                    now_ms(),
                    output_bytes.min(i64::MAX as u64) as i64
                ],
                |row| row.get::<_, bool>(0),
            )
            .optional()?;
        cancel.ok_or(JobError::NotHeld)
    }

    /// Marks the job of the claim `held` done with its report path inside the
    /// project folder.
    pub fn finish(&self, held: &ResearchJob, report_path: &str) -> Result<(), JobError> {
        bounded(report_path, REPORT_PATH_BYTES_MAX)?;
        self.transition(
            held,
            "status = 'done', report_path = ?4, finished_ms = ?5, claimer = NULL",
            &[&report_path, &now_ms()],
        )
    }

    /// Records a failed attempt of the claim `held`: a job whose cancel was
    /// requested ends cancelled, otherwise it requeues while attempts remain under
    /// `attempts_max`, otherwise it is marked failed. Returns the new status.
    pub fn fail(
        &self,
        held: &ResearchJob,
        error: &str,
        attempts_max: u32,
    ) -> Result<ResearchStatus, JobError> {
        if !(1..=ATTEMPTS_MAX).contains(&attempts_max) {
            return Err(JobError::Invalid("attempt limit must be 1..10"));
        }
        let error: String = error.chars().take(ERROR_BYTES_MAX / 4).collect();
        let claimer = held.claimer.as_deref().ok_or(JobError::NotHeld)?;
        let status = self
            .connection
            .query_row(
                "UPDATE research_jobs
                 SET status = CASE WHEN cancel_requested = 1 THEN 'cancelled'
                                   WHEN attempts < ?5 THEN 'queued' ELSE 'failed' END,
                     error = ?4, finished_ms = ?6, claimer = NULL
                 WHERE id = ?1 AND claimer = ?2 AND attempts = ?3 AND status = 'running'
                 RETURNING status",
                params![
                    held.id,
                    claimer,
                    held.attempts,
                    error,
                    attempts_max,
                    now_ms()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(JobError::NotHeld)?;
        let status = ResearchStatus::parse(&status).ok_or(JobError::Invalid("status"))?;
        assert!(matches!(
            status,
            ResearchStatus::Queued | ResearchStatus::Failed | ResearchStatus::Cancelled
        ));
        Ok(status)
    }

    /// Parks the job of the claim `held` until its project has a pool, without
    /// using an attempt; a job whose cancel was requested ends cancelled instead.
    /// Returns the new status.
    pub fn needs_pool(&self, held: &ResearchJob) -> Result<ResearchStatus, JobError> {
        let claimer = held.claimer.as_deref().ok_or(JobError::NotHeld)?;
        let status = self
            .connection
            .query_row(
                "UPDATE research_jobs
                 SET status = CASE WHEN cancel_requested = 1 THEN 'cancelled' ELSE 'needs_pool' END,
                     finished_ms = CASE WHEN cancel_requested = 1 THEN ?4 ELSE finished_ms END,
                     attempts = attempts - 1, claimer = NULL, started_ms = NULL, heartbeat_ms = NULL
                 WHERE id = ?1 AND claimer = ?2 AND attempts = ?3 AND status = 'running'
                 RETURNING status",
                params![held.id, claimer, held.attempts, now_ms()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(JobError::NotHeld)?;
        let status = ResearchStatus::parse(&status).ok_or(JobError::Invalid("status"))?;
        assert!(matches!(
            status,
            ResearchStatus::NeedsPool | ResearchStatus::Cancelled
        ));
        Ok(status)
    }

    /// Requeues every job of `project` for `harness` that waited for a pool.
    pub fn resume_waiting(&self, project: &str, harness: &str) -> Result<usize, JobError> {
        Ok(self.connection.execute(
            "UPDATE research_jobs SET status = 'queued', error = NULL
             WHERE project = ?1 AND harness = ?2 AND status = 'needs_pool'",
            params![project, harness],
        )?)
    }

    /// Parks every queued or waiting job of `project` for `harness` until its
    /// pool can run that harness, recording `detail` as the plain reason when
    /// there is one.
    pub fn park_waiting(
        &self,
        project: &str,
        harness: &str,
        detail: Option<&str>,
    ) -> Result<usize, JobError> {
        if let Some(detail) = detail {
            bounded(detail, ERROR_BYTES_MAX)?;
        }
        Ok(self.connection.execute(
            "UPDATE research_jobs SET status = 'needs_pool', error = ?3
             WHERE project = ?1 AND harness = ?2 AND status IN ('queued', 'needs_pool')
               AND (status = 'queued' OR error IS NOT ?3)",
            params![project, harness, detail],
        )?)
    }

    /// Returns the job of the claim `held` to the queue without spending its
    /// attempt, for a run the engine itself interrupted.
    pub fn release(&self, held: &ResearchJob) -> Result<(), JobError> {
        self.transition(
            held,
            "status = CASE WHEN cancel_requested = 1 THEN 'cancelled' ELSE 'queued' END, attempts = attempts - 1, claimer = NULL",
            &[],
        )
    }

    /// Returns the job of the claim `held` to the queue without spending its attempt after
    /// its account hit a rate or usage limit, and pauses that pool account in the pause
    /// master and team turns share before the job is free, so the next pick fails over to
    /// another account; a claim lost meanwhile keeps the pause. Takes the claim and the
    /// account's connection. Returns the new status and when the pause
    /// ends, in ms since the epoch.
    pub fn rate_limited(
        &self,
        held: &ResearchJob,
        account: &crate::protocol::CredentialRef,
    ) -> Result<(ResearchStatus, i64), JobError> {
        let until = crate::team::pause_account(&self.connection, account)?;
        self.release(held)?;
        let status = self
            .get(held.id)?
            .map_or(ResearchStatus::Queued, |job| job.status);
        Ok((status, until))
    }

    /// Each project and harness pair with queued or waiting research, in
    /// order, at most 1000.
    pub fn work(&self) -> Result<Vec<(String, String)>, JobError> {
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT project, harness FROM research_jobs
             WHERE status IN ('queued', 'needs_pool') ORDER BY project, harness LIMIT 1000",
        )?;
        let work = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(work)
    }

    /// Requests cancellation: a waiting job is cancelled at once, a running job
    /// is flagged for its runner. Returns false when nothing was left to cancel.
    pub fn request_cancel(&self, id: i64) -> Result<bool, JobError> {
        let now = now_ms();
        let changed = self.connection.execute(
            "UPDATE research_jobs
             SET status = CASE WHEN status = 'running' THEN 'running' ELSE 'cancelled' END,
                 cancel_requested = 1,
                 finished_ms = CASE WHEN status = 'running' THEN finished_ms ELSE ?2 END
             WHERE id = ?1 AND status IN ('queued', 'needs_pool', 'running')
               AND cancel_requested = 0",
            params![id, now],
        )?;
        assert!(changed <= 1);
        Ok(changed == 1)
    }

    /// Marks the cancel-flagged job of the claim `held` cancelled after its
    /// process stopped.
    pub fn cancelled(&self, held: &ResearchJob) -> Result<(), JobError> {
        self.transition(
            held,
            "status = 'cancelled', finished_ms = ?4, claimer = NULL",
            &[&now_ms()],
        )
    }

    /// Requeues running jobs whose heartbeat is older than `stale_ms`, so work
    /// held by a runner that died is not lost.
    pub fn recover_stale(&self, stale_ms: u64) -> Result<usize, JobError> {
        let cutoff = now_ms().saturating_sub(stale_ms.min(i64::MAX as u64) as i64);
        Ok(self.connection.execute(
            "UPDATE research_jobs
             SET status = CASE WHEN cancel_requested = 1 THEN 'cancelled' ELSE 'queued' END,
                 claimer = NULL
             WHERE status = 'running' AND heartbeat_ms <= ?1",
            params![cutoff],
        )?)
    }

    /// Reads what every agent launched for `project` receives, its approved
    /// skills, MCP servers, brief and memories, from the engine database this
    /// queue lives in. Returns the context, or a plain launch code such as
    /// `plugin_project_unknown` for a project the engine does not know.
    pub fn project_context(
        &self,
        project: &crate::protocol::ProjectId,
    ) -> Result<crate::launch_context::AgentContext, crate::harnesses::launch::LaunchError> {
        crate::launch_context::for_project(&self.connection, project)
    }

    /// Proves a managed connection is signed in for the running engine, with
    /// the check every master and team turn uses, on the engine database this
    /// queue lives in. Returns the engine host and the sign-in, or a plain code
    /// such as `native_profile_in_use` or `master_authentication_stale`.
    pub(crate) fn managed_sign_in(
        &self,
        connection: &crate::config::ModelConnection,
    ) -> Result<
        (
            crate::engine::owner::HostStamp,
            crate::engine::owner::native_auth::NativeAuthFact,
        ),
        String,
    > {
        super::accounts::managed_sign_in(&self.connection, connection)
    }

    /// One job by id.
    pub fn get(&self, id: i64) -> Result<Option<ResearchJob>, JobError> {
        Ok(self
            .connection
            .query_row(
                &format!("SELECT {COLUMNS} FROM research_jobs WHERE id = ?1"),
                params![id],
                job_from_row,
            )
            .optional()?)
    }

    /// Every job of `project`, oldest first, for status polling.
    pub fn list(&self, project: &str) -> Result<Vec<ResearchJob>, JobError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {COLUMNS} FROM research_jobs WHERE project = ?1 ORDER BY id LIMIT 1000"
        ))?;
        let jobs = statement
            .query_map(params![project], job_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(jobs)
    }

    fn transition(
        &self,
        held: &ResearchJob,
        assignments: &str,
        values: &[&dyn rusqlite::ToSql],
    ) -> Result<(), JobError> {
        assert!(values.len() <= 2);
        let claimer = held.claimer.as_deref().ok_or(JobError::NotHeld)?;
        let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&held.id, &claimer, &held.attempts];
        bound.extend_from_slice(values);
        let changed = self.connection.execute(
            &format!(
                "UPDATE research_jobs SET {assignments}
                 WHERE id = ?1 AND claimer = ?2 AND attempts = ?3 AND status = 'running'"
            ),
            bound.as_slice(),
        )?;
        assert!(changed <= 1);
        if changed == 0 {
            return Err(JobError::NotHeld);
        }
        Ok(())
    }
}

/// Whether `kind` is safe as a report file stem: 1..64 of `a-z`, `0-9`, `_`, `-`.
pub fn valid_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= KIND_BYTES_MAX
        && kind
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
}

fn bounded(value: &str, max: usize) -> Result<(), JobError> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(JobError::Invalid(
            "field must be 1..bounded noncontrol text",
        ));
    }
    Ok(())
}

fn now_ms() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(elapsed).unwrap_or(i64::MAX)
}

fn job_from_row(row: &Row<'_>) -> rusqlite::Result<ResearchJob> {
    let status: String = row.get(5)?;
    let optional_ms = |index: usize| -> rusqlite::Result<Option<u64>> {
        Ok(row
            .get::<_, Option<i64>>(index)?
            .map(|value| value.max(0) as u64))
    };
    Ok(ResearchJob {
        id: row.get(0)?,
        project: row.get(1)?,
        kind: row.get(2)?,
        harness: row.get(3)?,
        tools: row.get(4)?,
        status: ResearchStatus::parse(&status).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                "unknown research status".into(),
            )
        })?,
        attempts: row.get(6)?,
        claimer: row.get(7)?,
        report_path: row.get(8)?,
        error: row.get(9)?,
        cancel_requested: row.get(10)?,
        output_bytes: row.get::<_, i64>(11)?.max(0) as u64,
        created_ms: row.get::<_, i64>(12)?.max(0) as u64,
        started_ms: optional_ms(13)?,
        heartbeat_ms: optional_ms(14)?,
        finished_ms: optional_ms(15)?,
    })
}
