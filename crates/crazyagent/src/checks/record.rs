use super::queue::BatchId;
use super::runner::CheckExecution;
use super::snapshot::Snapshot;
use super::{AdmittedCheck, CheckRefusal, ProfileId};
use crate::landing::ChangeSetId;
use crate::tracker::Host;
use rusqlite::{Connection, Transaction};
use trackwright_core::runs::{IntegrityRef, RunState, RunView, TestId, Verdict};

/// What one check run is recorded from: the change set and the batch it ran in, the
/// snapshot it ran on (its commit, tree and base), the admitted check with its profile and
/// approval time, how it went, and the integrity evidence assessed before the transaction
/// (None when it could not be assessed, which agents then see as outcome only).
#[derive(Debug, Clone, Copy)]
pub struct Recording<'a> {
    pub change_set: &'a ChangeSetId,
    pub batch: &'a BatchId,
    pub snapshot: &'a Snapshot,
    pub check: &'a AdmittedCheck,
    pub execution: &'a CheckExecution,
    pub integrity: Option<&'a IntegrityRef>,
}

/// The newest recorded run of one check on a change set, as the gate reads it: the check
/// id, the `tk_runs` row and short id, the batch, the run's state and verdict, the commit
/// and tree it covered, the profile and approval it ran under, the protected paths the
/// change set touched, whether it was a holdout run and when it finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRun {
    pub check: String,
    pub run: i64,
    pub id: TestId,
    pub batch: BatchId,
    pub state: RunState,
    pub verdict: Option<Verdict>,
    pub commit: String,
    pub tree: String,
    pub profile: ProfileId,
    pub profile_approved_ms: i64,
    pub protected_touched: Vec<String>,
    pub holdout: bool,
    pub finished_ms: Option<i64>,
}

/// Records one check execution as one trackwright run in the caller's transaction, as the
/// engine actor `engine:checks`: the run with its batch, source (repository label, commit,
/// tree, base, change set) and policy (profile and approval, check, kind, argv, toolchain
/// family and version, sandbox and environment digests, network, timeout), its results
/// parsed from the report or the exit status, its end and verdict by the core's rules
/// (interrupted for a timeout or cancel), the integrity evidence, the change set's
/// binding to the run, then `notify` so linked tickets re-evaluate in the same
/// transaction. A rolled-back transaction leaves no run and no ticket change. Inputs: the
/// transaction, the host, the recording and the time. Output: the run as stored, or
/// `checks_record_invalid`, `project_not_attached` or a core refusal's code.
pub fn store(
    tx: &Transaction<'_>,
    host: &Host,
    recording: &Recording<'_>,
    now_ms: i64,
) -> Result<RunView, CheckRefusal> {
    let _ = (tx, host, recording, now_ms);
    todo!()
}

/// The newest finished or interrupted run of each check recorded on a change set, any head,
/// sorted by check id; the gate compares each one's tree and profile with the reviewed head
/// and the current profile. Inputs: the engine connection or a transaction on it, and the
/// change set. Output: the runs, or `checks_unavailable`.
pub fn latest(db: &Connection, change_set: &ChangeSetId) -> Result<Vec<CheckRun>, CheckRefusal> {
    let _ = (db, change_set);
    todo!()
}
