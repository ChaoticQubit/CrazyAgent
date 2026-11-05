#[cfg(unix)]
use super::binding::{self, Ending};
#[cfg(unix)]
use super::integrity::{self, Subject};
use super::record::{self, Recording};
use super::runner::{self, RunLimits};
use super::snapshot::{self, RunKey, Snapshot};
use super::{Actor, AdmittedCheck, AdmittedProfile, CheckRefusal, NetworkMode, Trigger, settings};
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::landing::{self, ChangeSetId, ChangeState};
use crate::protocol::ProjectId;
use crate::tracker::Host;
#[cfg(unix)]
use rusqlite::OptionalExtension;
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use trackwright_core::runs::{IntegrityRef, Verdict};

/// The most jobs one project holds queued or running at once.
pub const QUEUED_MAX: usize = 64;
/// The most bytes of a batch id: `b`, then 16 lowercase hex digits.
pub const BATCH_BYTES: usize = 17;
/// The most jobs `jobs` lists for one change set, newest first.
pub const JOBS_LISTED_MAX: usize = 256;
/// How often a running job looks for a cancel, a discard or a moved head.
pub const WATCH_EVERY: Duration = Duration::from_millis(250);

const HOUR_MS: i64 = 3_600_000;
const PICKED_MAX: i64 = 256;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS check_jobs (
    id INTEGER PRIMARY KEY,
    batch TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 1),
    project TEXT NOT NULL,
    change_set TEXT NOT NULL,
    head TEXT NOT NULL,
    check_id TEXT NOT NULL,
    trigger TEXT NOT NULL CHECK (trigger IN ('turn_end', 'before_accept', 'on_request')),
    actor TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'recorded', 'cancelled')),
    runner TEXT,
    run INTEGER,
    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    reason TEXT,
    queued_ms INTEGER NOT NULL,
    started_ms INTEGER,
    ended_ms INTEGER,
    UNIQUE (batch, position),
    UNIQUE (batch, check_id),
    CHECK ((state = 'recorded') = (run IS NOT NULL)),
    CHECK ((state = 'running') <= (runner IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS check_jobs_change_set ON check_jobs(change_set, queued_ms);
CREATE INDEX IF NOT EXISTS check_jobs_state ON check_jobs(state, id);
CREATE INDEX IF NOT EXISTS check_jobs_requests ON check_jobs(project, trigger, queued_ms);";

const JOB_COLUMNS: &str =
    "batch, change_set, check_id, trigger, actor, state, run, reason, queued_ms";

/// One trigger's batch of check runs on one change set, also the trackwright `batch` label
/// of each run it records: `b` and 16 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BatchId(String);

/// Where one job of a batch stands: waiting for a free run slot, running in its snapshot,
/// recorded as a trackwright run, or cancelled before it was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Recorded,
    Cancelled,
}

/// One check of a batch: its batch, change set and check id, the trigger and who asked,
/// where it stands, the `tk_runs` row it recorded (None until recorded), the code it was
/// cancelled with (None unless cancelled) and when it was queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub batch: BatchId,
    pub change_set: ChangeSetId,
    pub check: String,
    pub trigger: Trigger,
    pub actor: Actor,
    pub state: JobState,
    pub run: Option<i64>,
    pub reason: Option<String>,
    pub queued_ms: i64,
}

/// A queued job this process took to run: its row, batch slot, project, change set, the
/// head it was queued at and the admitted check and profile it runs under.
struct Claimed {
    id: i64,
    batch: BatchId,
    position: i64,
    project: ProjectId,
    change_set: ChangeSetId,
    head: String,
    check: AdmittedCheck,
    profile: AdmittedProfile,
}

impl BatchId {
    /// Checks a batch id. Input: the text. Output: the id, or `checks_batch_unknown` for
    /// anything but `b` and 16 lowercase hex digits.
    pub fn new(value: impl Into<String>) -> Result<Self, CheckRefusal> {
        let value = value.into();
        let digits = value.strip_prefix('b').unwrap_or("");
        let hex = digits
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if value.len() != BATCH_BYTES || digits.len() != BATCH_BYTES - 1 || !hex {
            return Err(failure("checks_batch_unknown"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn fresh() -> Result<Self, CheckRefusal> {
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes).map_err(unavailable)?;
        let digits: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        Self::new(format!("b{digits}"))
    }
}

impl JobState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Recorded => "recorded",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Queued, Self::Running, Self::Recorded, Self::Cancelled]
            .into_iter()
            .find(|state| state.as_str() == text)
    }
}

fn unavailable<E>(_: E) -> CheckRefusal {
    failure("checks_unavailable")
}

fn trigger_text(trigger: Trigger) -> &'static str {
    match trigger {
        Trigger::TurnEnd => "turn_end",
        Trigger::BeforeAccept => "before_accept",
        Trigger::OnRequest => "on_request",
    }
}

fn trigger_of(text: &str) -> Option<Trigger> {
    [Trigger::TurnEnd, Trigger::BeforeAccept, Trigger::OnRequest]
        .into_iter()
        .find(|trigger| trigger_text(*trigger) == text)
}

fn actor_text(actor: Actor) -> String {
    match actor {
        Actor::Owner => "owner".into(),
        Actor::Master(id) => format!("master:{id}"),
        Actor::Agent(id) => format!("agent:{id}"),
    }
}

fn actor_of(text: &str) -> Option<Actor> {
    if text == "owner" {
        return Some(Actor::Owner);
    }
    let (kind, id) = text.split_once(':')?;
    let id = id.parse().ok()?;
    match kind {
        "master" => Some(Actor::Master(id)),
        "agent" => Some(Actor::Agent(id)),
        _ => None,
    }
}

fn now_ms() -> i64 {
    crate::team::now_ms()
}

fn immediate(db: &Connection) -> Result<Transaction<'_>, CheckRefusal> {
    Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)
}

/// The id this engine process marks the jobs it runs with; a running job another process
/// marked was left by an engine that stopped.
fn runner_mark() -> Result<&'static str, CheckRefusal> {
    static MARK: OnceLock<String> = OnceLock::new();
    if let Some(mark) = MARK.get() {
        return Ok(mark);
    }
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(unavailable)?;
    let fresh: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(MARK.get_or_init(|| fresh))
}

/// Creates the queue's tables in `engine.db`, forward only and idempotent. Input: the
/// engine database. Output: Ok, or `checks_unavailable`.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(SCHEMA).map_err(unavailable)?;
    record::initialize(connection)
}

/// Queues one run of each check of the change set's current profile that `trigger` names,
/// holdouts included, as one batch: every such check when `only` is empty, else just the ones
/// it names, where an agent or master may name only checks whose `when` holds the trigger and
/// the owner any check. Each agent's or master's request counts against its own
/// `agent_requests` per hour in the project, and a change set whose head already has a queued
/// or running job of a check gets no second one. Inputs: the engine connection, the change set,
/// the trigger, the actor the engine set and the check ids. Output: the batch, or
/// `checks_change_set_unknown`, `checks_change_set_discarded`, `checks_change_set_accepted`,
/// `checks_no_profile`, `checks_check_unknown`, `checks_not_on_request`,
/// `checks_nothing_to_run`, `checks_requests_off`, `checks_rate_limited`, `checks_queue_full`
/// or `checks_unavailable`.
pub fn submit(
    db: &Connection,
    change_set: &ChangeSetId,
    trigger: Trigger,
    actor: Actor,
    only: &[String],
) -> Result<BatchId, CheckRefusal> {
    let tx = immediate(db)?;
    let (project, view) = landing::change_set(&tx, change_set)?
        .ok_or_else(|| failure("checks_change_set_unknown"))?;
    match view.state {
        ChangeState::Discarded => return Err(failure("checks_change_set_discarded")),
        ChangeState::Accepted => return Err(failure("checks_change_set_accepted")),
        ChangeState::Pending | ChangeState::Conflict | ChangeState::Updating => {}
    }
    let profile =
        super::current_profile(&tx, &project)?.ok_or_else(|| failure("checks_no_profile"))?;
    let checks = profile.checks();
    let chosen: Vec<&AdmittedCheck> = if only.is_empty() {
        checks
            .iter()
            .filter(|check| check.spec.when.contains(&trigger))
            .collect()
    } else {
        let named: BTreeSet<&str> = only.iter().map(String::as_str).collect();
        if named
            .iter()
            .any(|id| !checks.iter().any(|check| check.spec.id == *id))
        {
            return Err(failure("checks_check_unknown"));
        }
        let chosen: Vec<&AdmittedCheck> = checks
            .iter()
            .filter(|check| named.contains(check.spec.id.as_str()))
            .collect();
        if actor != Actor::Owner
            && chosen
                .iter()
                .any(|check| !check.spec.when.contains(&trigger))
        {
            return Err(failure("checks_not_on_request"));
        }
        chosen
    };
    assert!(
        actor == Actor::Owner
            || chosen
                .iter()
                .all(|check| check.spec.when.contains(&trigger))
    );
    let now = now_ms();
    if trigger == Trigger::OnRequest && actor != Actor::Owner {
        let allowed = settings::effective(&tx, &project)?.agent_requests;
        if allowed == 0 {
            return Err(failure("checks_requests_off"));
        }
        let asked: i64 = tx
            .query_row(
                "SELECT COUNT(DISTINCT batch) FROM check_jobs
                  WHERE project = ?1 AND trigger = 'on_request' AND actor = ?2
                    AND queued_ms > ?3",
                params![project.as_str(), actor_text(actor), now - HOUR_MS],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if asked >= i64::from(allowed) {
            return Err(failure("checks_rate_limited"));
        }
    }
    let mut pending = tx
        .prepare(
            "SELECT 1 FROM check_jobs WHERE change_set = ?1 AND head = ?2 AND check_id = ?3
                AND state IN ('queued', 'running') LIMIT 1",
        )
        .map_err(unavailable)?;
    let mut fresh = Vec::new();
    for check in chosen {
        let waiting = pending
            .exists(params![change_set.as_str(), view.head, check.spec.id])
            .map_err(unavailable)?;
        if !waiting {
            fresh.push(check);
        }
    }
    drop(pending);
    if fresh.is_empty() {
        return Err(failure("checks_nothing_to_run"));
    }
    let held: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM check_jobs WHERE project = ?1 AND state IN ('queued', 'running')",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    let held = usize::try_from(held).map_err(unavailable)?;
    if held + fresh.len() > QUEUED_MAX {
        return Err(failure("checks_queue_full"));
    }
    let batch = BatchId::fresh()?;
    for (index, check) in fresh.iter().enumerate() {
        let added = tx
            .execute(
                "INSERT INTO check_jobs(batch, position, project, change_set, head, check_id,
                     trigger, actor, state, queued_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'queued', ?9)",
                params![
                    batch.as_str(),
                    i64::try_from(index + 1).map_err(unavailable)?,
                    project.as_str(),
                    change_set.as_str(),
                    view.head,
                    check.spec.id,
                    trigger_text(trigger),
                    actor_text(actor),
                    now
                ],
            )
            .map_err(unavailable)?;
        assert_eq!(added, 1);
    }
    tx.commit().map_err(unavailable)?;
    Ok(batch)
}

/// Cancels a change set's queued jobs and stops its running ones, whose runs are then
/// recorded as interrupted `cancelled`; the owner's explicit cancel calls it, and a
/// running job also stops by itself once its change set is discarded or moves. Inputs: the
/// engine connection and the change set. Output: how many jobs were queued or running, or
/// `checks_unavailable`.
pub fn cancel(db: &Connection, change_set: &ChangeSetId) -> Result<u32, CheckRefusal> {
    let tx = immediate(db)?;
    let queued = tx
        .execute(
            "UPDATE check_jobs SET state = 'cancelled', reason = 'cancelled', ended_ms = ?2
              WHERE change_set = ?1 AND state = 'queued'",
            params![change_set.as_str(), now_ms()],
        )
        .map_err(unavailable)?;
    let running = tx
        .execute(
            "UPDATE check_jobs SET cancel_requested = 1
              WHERE change_set = ?1 AND state = 'running'",
            [change_set.as_str()],
        )
        .map_err(unavailable)?;
    tx.commit().map_err(unavailable)?;
    u32::try_from(queued + running).map_err(unavailable)
}

/// A change set's jobs, newest batch first and in profile order inside a batch, at most
/// `JOBS_LISTED_MAX`. Inputs: the engine connection and the change set. Output: the jobs,
/// or `checks_unavailable`.
pub fn jobs(db: &Connection, change_set: &ChangeSetId) -> Result<Vec<Job>, CheckRefusal> {
    let sql = format!(
        "SELECT {JOB_COLUMNS} FROM check_jobs WHERE change_set = ?1
          ORDER BY queued_ms DESC, batch, position LIMIT ?2"
    );
    let limit = i64::try_from(JOBS_LISTED_MAX).expect("the bound fits in i64");
    let mut statement = db.prepare(&sql).map_err(unavailable)?;
    let jobs = statement
        .query_map(params![change_set.as_str(), limit], job_row)
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    assert!(jobs.len() <= JOBS_LISTED_MAX);
    jobs.into_iter().collect()
}

/// One `check_jobs` row as stored, before its text columns are checked.
struct StoredJob {
    batch: String,
    change_set: String,
    check: String,
    trigger: String,
    actor: String,
    state: String,
    run: Option<i64>,
    reason: Option<String>,
    queued_ms: i64,
}

impl StoredJob {
    /// Checks the stored text columns. Output: the job, or `checks_unavailable` for a
    /// column `submit` never writes.
    fn read(self) -> Result<Job, CheckRefusal> {
        Ok(Job {
            batch: BatchId::new(self.batch).map_err(unavailable)?,
            change_set: ChangeSetId::new(self.change_set).map_err(unavailable)?,
            check: self.check,
            trigger: trigger_of(&self.trigger).ok_or_else(|| unavailable(()))?,
            actor: actor_of(&self.actor).ok_or_else(|| unavailable(()))?,
            state: JobState::parse(&self.state).ok_or_else(|| unavailable(()))?,
            run: self.run,
            reason: self.reason,
            queued_ms: self.queued_ms,
        })
    }
}

fn job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Job, CheckRefusal>> {
    let stored = StoredJob {
        batch: row.get(0)?,
        change_set: row.get(1)?,
        check: row.get(2)?,
        trigger: row.get(3)?,
        actor: row.get(4)?,
        state: row.get(5)?,
        run: row.get(6)?,
        reason: row.get(7)?,
        queued_ms: row.get(8)?,
    };
    Ok(stored.read())
}

fn job(db: &Connection, id: i64) -> Result<Job, CheckRefusal> {
    let sql = format!("SELECT {JOB_COLUMNS} FROM check_jobs WHERE id = ?1");
    db.query_row(&sql, [id], job_row).map_err(unavailable)?
}

fn end(db: &Connection, id: i64, reason: &str) -> Result<Job, CheckRefusal> {
    let ended = db
        .execute(
            "UPDATE check_jobs SET state = 'cancelled', reason = ?2, ended_ms = ?3
              WHERE id = ?1 AND state IN ('queued', 'running')",
            params![id, reason, now_ms()],
        )
        .map_err(unavailable)?;
    assert!(ended <= 1);
    let stored = job(db, id)?;
    assert!(matches!(
        stored.state,
        JobState::Cancelled | JobState::Recorded
    ));
    Ok(stored)
}

/// Runs the oldest queued job, while fewer than the project's `parallel` jobs run, to its
/// record: the snapshot of the change set's head, the confined run under the project's
/// trust ceiling (`off` for an untrusted project or an unreadable level, `remotes` for a
/// reviewed one), the integrity assessment, then the run, its results and its source,
/// policy and integrity in one transaction, and the snapshot removed or kept per
/// `keep_failed_snapshots`. A job whose change set moved on or was discarded before it ran
/// is cancelled with `checks_change_set_moved` or `checks_change_set_discarded`; one that
/// moves or is discarded while it runs is recorded as interrupted `cancelled`. Running jobs
/// a stopped engine left are cancelled with `checks_engine_stopped`. Inputs: the engine
/// connection and the engine runtime holding `checks/`. Output: the job as it ended, None
/// when nothing could start, or `checks_unavailable`.
pub fn run_next(db: &Connection, runtime: &Path) -> Result<Option<Job>, CheckRefusal> {
    let claimed = match claim(db)? {
        Picked::Nothing => return Ok(None),
        Picked::Ended(id) => return job(db, id).map(Some),
        Picked::Claimed(claimed) => claimed,
    };
    run_claimed(db, runtime, *claimed).map(Some)
}

/// One queued `check_jobs` row a pick looks at.
struct Queued {
    id: i64,
    batch: String,
    position: i64,
    project: String,
    change_set: String,
    head: String,
    check_id: String,
}

/// Queues a change set's `turn_end` checks after a turn added edits to it and starts the
/// project's `parallel` runners in the background, each on its own connection, running
/// jobs until none can start. A project with no profile or no `turn_end` check queues
/// nothing; the turn end never fails because of its checks. Inputs: the engine connection
/// on `<runtime>/engine.db` and the change set. Output: the batch, or None when nothing
/// was queued.
pub fn after_turn(db: &Connection, change_set: &ChangeSetId) -> Option<BatchId> {
    let batch = submit(db, change_set, Trigger::TurnEnd, Actor::Owner, &[]).ok()?;
    start(db, change_set);
    Some(batch)
}

/// Starts the project's `parallel` check runners in the background, each on its own
/// connection to the same `engine.db`, running queued jobs until none can start. Inputs:
/// the engine connection on `<runtime>/engine.db` and the change set whose project sets
/// `parallel`. Output: how many runners started, 0 when the database has no absolute
/// folder or no thread could start.
pub fn start(db: &Connection, change_set: &ChangeSetId) -> u8 {
    let Some(runtime) = db
        .path()
        .map(Path::new)
        .and_then(Path::parent)
        .filter(|runtime| runtime.is_absolute())
        .map(Path::to_path_buf)
    else {
        return 0;
    };
    let parallel = landing::change_set(db, change_set)
        .ok()
        .flatten()
        .and_then(|(project, _)| settings::effective(db, &project).ok())
        .map_or(1, |settings| settings.parallel.max(1));
    let mut started = 0;
    for _ in 0..parallel {
        let runtime = runtime.clone();
        let spawned = std::thread::Builder::new()
            .name("checks-runner".into())
            .spawn(move || {
                let Ok(db) = landing::open(&runtime) else {
                    return;
                };
                while let Ok(Some(_)) = run_next(&db, &runtime) {}
            });
        if spawned.is_err() {
            break;
        }
        started += 1;
    }
    assert!(started <= parallel);
    started
}

/// What one pick of the queue found: nothing to start, a job it cancelled at once, or a
/// job now running in this process.
enum Picked {
    Nothing,
    Ended(i64),
    Claimed(Box<Claimed>),
}

fn claim(db: &Connection) -> Result<Picked, CheckRefusal> {
    let mark = runner_mark()?;
    let tx = immediate(db)?;
    let now = now_ms();
    tx.execute(
        "UPDATE check_jobs SET state = 'cancelled', reason = 'checks_engine_stopped',
             ended_ms = ?2
          WHERE state = 'running' AND runner <> ?1",
        params![mark, now],
    )
    .map_err(unavailable)?;
    let queued: Vec<Queued> = {
        let mut statement = tx
            .prepare(
                "SELECT id, batch, position, project, change_set, head, check_id FROM check_jobs
                  WHERE state = 'queued' ORDER BY id LIMIT ?1",
            )
            .map_err(unavailable)?;
        statement
            .query_map([PICKED_MAX], |row| {
                Ok(Queued {
                    id: row.get(0)?,
                    batch: row.get(1)?,
                    position: row.get(2)?,
                    project: row.get(3)?,
                    change_set: row.get(4)?,
                    head: row.get(5)?,
                    check_id: row.get(6)?,
                })
            })
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    let mut full = BTreeSet::new();
    for queued in queued {
        let Queued {
            id,
            batch,
            position,
            project,
            change_set,
            head,
            check_id,
        } = queued;
        if full.contains(&project) {
            continue;
        }
        let project_id = ProjectId::new(project.clone()).map_err(unavailable)?;
        let parallel = settings::effective(&tx, &project_id)?.parallel;
        let running: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM check_jobs WHERE project = ?1 AND state = 'running'",
                [project.as_str()],
                |row| row.get(0),
            )
            .map_err(unavailable)?;
        if running >= i64::from(parallel) {
            full.insert(project);
            continue;
        }
        let change_set = ChangeSetId::new(change_set).map_err(unavailable)?;
        let stale = match landing::change_set(&tx, &change_set)? {
            None => Some("checks_change_set_discarded"),
            Some((_, view)) => match view.state {
                ChangeState::Discarded => Some("checks_change_set_discarded"),
                ChangeState::Accepted => Some("checks_change_set_accepted"),
                _ if view.head != head => Some("checks_change_set_moved"),
                _ => None,
            },
        };
        let profile = super::current_profile(&tx, &project_id)?;
        let check = profile.as_ref().and_then(|profile| {
            profile
                .checks()
                .into_iter()
                .find(|check| check.spec.id == check_id)
        });
        let missing = match (&profile, &check) {
            (None, _) => Some("checks_no_profile"),
            (Some(_), None) => Some("checks_check_unknown"),
            (Some(_), Some(_)) => None,
        };
        if let Some(reason) = stale.or(missing) {
            end(&tx, id, reason)?;
            tx.commit().map_err(unavailable)?;
            return Ok(Picked::Ended(id));
        }
        let started = tx
            .execute(
                "UPDATE check_jobs SET state = 'running', runner = ?2, started_ms = ?3
                  WHERE id = ?1 AND state = 'queued'",
                params![id, mark, now],
            )
            .map_err(unavailable)?;
        assert_eq!(started, 1);
        tx.commit().map_err(unavailable)?;
        let (Some(check), Some(profile)) = (check, profile) else {
            unreachable!("a job starts only with its check in the current profile");
        };
        return Ok(Picked::Claimed(Box::new(Claimed {
            id,
            batch: BatchId::new(batch)?,
            position,
            project: project_id,
            change_set,
            head,
            check,
            profile,
        })));
    }
    tx.commit().map_err(unavailable)?;
    Ok(Picked::Nothing)
}

fn run_claimed(db: &Connection, runtime: &Path, claimed: Claimed) -> Result<Job, CheckRefusal> {
    let settings = settings::effective(db, &claimed.project)?;
    let holdout = match claimed.check.holdout {
        Some(_) => super::holdout_files(db, &claimed.project, &claimed.check),
        None => Ok(BTreeMap::new()),
    };
    let key = RunKey::new(format!("{}-{}", claimed.batch.as_str(), claimed.position));
    let prepared = holdout.and_then(|files| {
        let snapshot = snapshot::prepare(db, &runtime.join("checks"), &claimed.change_set, &key?)?;
        Ok((files, snapshot))
    });
    let (files, snapshot) = match prepared {
        Ok(prepared) => prepared,
        Err(refusal) => return end(db, claimed.id, &refusal.0),
    };
    let limits = RunLimits {
        timeout: Duration::from_secs(u64::from(claimed.check.spec.timeout_s)),
        output_cap: runner::OUTPUT_CAP,
        report_cap: runner::REPORT_CAP,
        network_ceiling: network_ceiling(db, &claimed.project),
        remotes: Vec::new(),
    };
    let cancel = AtomicBool::new(false);
    let done = AtomicBool::new(false);
    let execution = std::thread::scope(|scope| {
        scope.spawn(|| watch(runtime, &claimed, &cancel, &done));
        let execution = runner::run(&snapshot, &claimed.check, &files, &limits, &cancel);
        done.store(true, Ordering::SeqCst);
        execution
    });
    drop(files);
    let integrity = assessed(db, &claimed, &settings, &execution);
    let recorded = record_run(db, &claimed, &snapshot, &execution, integrity.as_ref());
    let verdict = match &recorded {
        Ok(verdict) => *verdict,
        Err(_) => None,
    };
    let _ = if verdict == Some(Verdict::Passed) {
        snapshot.remove()
    } else {
        snapshot.keep(settings.keep_failed_snapshots)
    };
    match recorded {
        Ok(_) => job(db, claimed.id),
        Err(refusal) => end(db, claimed.id, &refusal.0),
    }
}

/// The widest network any check of a project gets: the project's trust level's
/// `check_network`, so an untrusted project's checks run with network off whatever its
/// profile says (threat model N1), and `off` when the level cannot be read. Inputs: the
/// engine connection and the project. Output: the ceiling.
fn network_ceiling(db: &Connection, project: &ProjectId) -> NetworkMode {
    let Ok(trust) = crate::trust::level(db, project.as_str()) else {
        return NetworkMode::Off;
    };
    let ceiling = crate::trust::Enforcement::for_level(trust).check_network;
    assert!(trust == crate::trust::Trust::Reviewed || ceiling == NetworkMode::Off);
    ceiling
}

fn record_run(
    db: &Connection,
    claimed: &Claimed,
    snapshot: &Snapshot,
    execution: &runner::CheckExecution,
    integrity: Option<&IntegrityRef>,
) -> Result<Option<Verdict>, CheckRefusal> {
    let tx = immediate(db)?;
    let recording = Recording {
        change_set: &claimed.change_set,
        batch: &claimed.batch,
        snapshot,
        check: &claimed.check,
        execution,
        integrity,
    };
    let view = record::store(&tx, &Host, &recording, now_ms())?;
    let run = record::row_of(&tx, &view.id)?;
    let recorded = tx
        .execute(
            "UPDATE check_jobs SET state = 'recorded', run = ?2, ended_ms = ?3
              WHERE id = ?1 AND state = 'running'",
            params![claimed.id, run, now_ms()],
        )
        .map_err(unavailable)?;
    if recorded != 1 {
        return Err(failure("checks_unavailable"));
    }
    tx.commit().map_err(unavailable)?;
    Ok(view.verdict)
}

/// The integrity block of a run that ended with results to compare, assessed before the
/// recording transaction; None for a stopped or never-started run, or when the
/// assessment failed (the change set moved, or git could not be read), which agents then
/// see as outcome only.
#[cfg(unix)]
fn assessed(
    db: &Connection,
    claimed: &Claimed,
    settings: &settings::ChecksSettings,
    execution: &runner::CheckExecution,
) -> Option<IntegrityRef> {
    let bound = binding::bind(&claimed.check, execution);
    if matches!(bound.ending, Ending::Stopped(_) | Ending::NotStarted(_)) {
        return None;
    }
    let tracker_project: i64 = db
        .query_row(
            "SELECT p.id FROM tracker_projects t JOIN tk_projects p ON p.key = t.key
              WHERE t.project = ?1",
            [claimed.project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .ok()??;
    let subject = Subject {
        change_set: &claimed.change_set,
        head: &claimed.head,
        tracker_project,
        check: &claimed.check,
        protected_paths: &claimed.profile.profile.protected_paths,
        settings,
    };
    let block = integrity::assess(db, &subject, &bound.reported(execution)).ok()?;
    serde_json::to_value(&block)
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
}

/// Assesses nothing where the engine's change set store is missing. Takes the store, the
/// job, the settings and the execution, all unused. Returns None.
#[cfg(not(unix))]
fn assessed(
    db: &Connection,
    claimed: &Claimed,
    settings: &settings::ChecksSettings,
    execution: &runner::CheckExecution,
) -> Option<IntegrityRef> {
    let _ = (db, claimed, settings, execution);
    None
}

/// Stops a running job's check once the owner cancels it, or its change set is discarded,
/// accepted or moves past the head it was queued at: polls every `WATCH_EVERY` on its own
/// connection until the run is done. A watcher that cannot open the store stops the run.
fn watch(runtime: &Path, claimed: &Claimed, cancel: &AtomicBool, done: &AtomicBool) {
    let db = landing::open(runtime);
    while !done.load(Ordering::SeqCst) {
        std::thread::sleep(WATCH_EVERY);
        let stop = match &db {
            Err(_) => true,
            Ok(db) => stopped(db, claimed).unwrap_or(false),
        };
        if stop {
            cancel.store(true, Ordering::SeqCst);
            return;
        }
    }
}

fn stopped(db: &Connection, claimed: &Claimed) -> Result<bool, CheckRefusal> {
    let requested: bool = db
        .query_row(
            "SELECT cancel_requested FROM check_jobs WHERE id = ?1",
            [claimed.id],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if requested {
        return Ok(true);
    }
    Ok(match landing::change_set(db, &claimed.change_set)? {
        None => true,
        Some((_, view)) => {
            matches!(view.state, ChangeState::Discarded | ChangeState::Accepted)
                || view.head != claimed.head
        }
    })
}
