use super::{Actor, CheckRefusal, Trigger};
use crate::engine::EngineError;
use crate::landing::ChangeSetId;
use rusqlite::Connection;
use std::path::Path;

/// The most jobs one project holds queued or running at once.
pub const QUEUED_MAX: usize = 64;
/// The most bytes of a batch id: `b`, then 16 lowercase hex digits.
pub const BATCH_BYTES: usize = 17;

/// One trigger's batch of check runs on one change set, also the trackwright `batch` label
/// of each run it records: `b` and 16 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BatchId(String);

/// Where one job of a batch stands: waiting for a free run slot, running in its snapshot,
/// recorded as a trackwright run, or cancelled before it was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Recorded,
    Cancelled,
}

/// One check of a batch: its batch, change set and check id, the trigger and who asked,
/// where it stands, the `tk_runs` row it recorded (None until recorded) and when it was
/// queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub batch: BatchId,
    pub change_set: ChangeSetId,
    pub check: String,
    pub trigger: Trigger,
    pub actor: Actor,
    pub state: JobState,
    pub run: Option<i64>,
    pub queued_ms: i64,
}

impl BatchId {
    /// Checks a batch id. Input: the text. Output: the id, or `checks_batch_unknown` for
    /// anything but `b` and 16 lowercase hex digits.
    pub fn new(value: impl Into<String>) -> Result<Self, CheckRefusal> {
        let value = value.into();
        let _ = value;
        todo!()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Creates the queue's tables in `engine.db`, forward only and idempotent. Input: the
/// engine database. Output: Ok, or `checks_unavailable`.
pub fn initialize(connection: &Connection) -> Result<(), EngineError> {
    let _ = connection;
    todo!()
}

/// Queues one run of each check of the change set's current profile that `trigger` names,
/// holdouts included, as one batch: every such check when `only` is empty, else just the
/// ones it names. An agent's or master's request counts against the project's
/// `agent_requests` per hour, and a change set whose head already has a queued or running
/// job of a check gets no second one. Inputs: the engine connection, the change set, the
/// trigger, the actor the engine set and the check ids. Output: the batch, or
/// `checks_change_set_unknown`, `checks_change_set_discarded`, `checks_no_profile`,
/// `checks_check_unknown`, `checks_nothing_to_run`, `checks_requests_off`,
/// `checks_rate_limited`, `checks_queue_full` or `checks_unavailable`.
pub fn submit(
    db: &Connection,
    change_set: &ChangeSetId,
    trigger: Trigger,
    actor: Actor,
    only: &[String],
) -> Result<BatchId, CheckRefusal> {
    let _ = (db, change_set, trigger, actor, only);
    todo!()
}

/// Cancels a change set's queued jobs and stops its running ones, whose runs are then
/// recorded as interrupted `cancelled`; discarding a change set calls it. Inputs: the
/// engine connection and the change set. Output: how many jobs were queued or running, or
/// `checks_unavailable`.
pub fn cancel(db: &Connection, change_set: &ChangeSetId) -> Result<u32, CheckRefusal> {
    let _ = (db, change_set);
    todo!()
}

/// A change set's jobs, newest batch first and in profile order inside a batch. Inputs:
/// the engine connection and the change set. Output: the jobs, or `checks_unavailable`.
pub fn jobs(db: &Connection, change_set: &ChangeSetId) -> Result<Vec<Job>, CheckRefusal> {
    let _ = (db, change_set);
    todo!()
}

/// Runs the oldest queued job, while fewer than the project's `parallel` jobs run, to its
/// record: the snapshot of the change set's head, the confined run under the network
/// ceiling `remotes`, the integrity assessment, then the run, its results and its source,
/// policy and integrity in one transaction, and the snapshot removed or kept per
/// `keep_failed_snapshots`. A job whose change set moved on or was discarded is recorded as
/// interrupted. Inputs: the engine connection and the engine runtime holding `checks/`.
/// Output: the job as it ended, None when nothing could start, or `checks_unavailable`.
pub fn run_next(db: &Connection, runtime: &Path) -> Result<Option<Job>, CheckRefusal> {
    let _ = (db, runtime);
    todo!()
}
