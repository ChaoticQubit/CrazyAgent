use super::binding::{self, Ending};
use super::queue::BatchId;
use super::runner::CheckExecution;
use super::snapshot::Snapshot;
use super::{AdmittedCheck, CheckRefusal, ProfileId};
use crate::engine::owner::failure;
use crate::landing::{self, ChangeSetId};
use crate::tracker::{self, Host};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use trackwright_core::coverage;
use trackwright_core::model::Actor as CoreActor;
use trackwright_core::runs::{
    self, ENGINE_ACTOR, IntegrityRef, PolicyRef, RunCancel, RunEnd, RunFinish, RunRecord, RunStart,
    RunState, RunView, SourceRef, TestId, Toolchain, ToolchainFamily, Verdict,
};
use trackwright_core::store::StoreError;
use trackwright_core::tests_model::RESULT_ROWS_PER_RECORD_MAX;

/// The digest a run records for a sandbox or environment it never built: SHA-256 of no bytes.
pub const EMPTY_DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS check_runs (
    run INTEGER PRIMARY KEY NOT NULL,
    short TEXT NOT NULL UNIQUE,
    change_set TEXT NOT NULL,
    check_id TEXT NOT NULL,
    batch TEXT NOT NULL,
    commit_id TEXT NOT NULL,
    tree TEXT NOT NULL,
    profile TEXT NOT NULL,
    profile_approved_ms INTEGER NOT NULL,
    protected_json TEXT NOT NULL,
    holdout INTEGER NOT NULL CHECK (holdout IN (0, 1))
);
CREATE INDEX IF NOT EXISTS check_runs_change_set ON check_runs(change_set, check_id, run);";

const LATEST: &str = "SELECT c.check_id, c.run, c.short, c.batch, r.state, r.verdict, c.commit_id,
        c.tree, c.profile, c.profile_approved_ms, c.protected_json, c.holdout, r.finished_ms
   FROM check_runs c JOIN tk_runs r ON r.id = c.run
  WHERE c.change_set = ?1 AND r.state <> 'running'
    AND c.run = (SELECT MAX(o.run) FROM check_runs o JOIN tk_runs s ON s.id = o.run
                  WHERE o.change_set = c.change_set AND o.check_id = c.check_id
                    AND s.state <> 'running')
  ORDER BY c.check_id";

const ROW_OF: &str = "SELECT r.id FROM tk_runs r JOIN tk_projects p ON p.id = r.project
  WHERE p.key = ?1 AND r.number = ?2";

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

fn unavailable<E>(_: E) -> CheckRefusal {
    failure("checks_unavailable")
}

fn core(error: StoreError) -> CheckRefusal {
    match error {
        StoreError::Refused(refusal) => failure(refusal.code),
        StoreError::Sqlite(_) | StoreError::Corrupt(_) => failure("checks_unavailable"),
    }
}

/// Creates the binding of recorded runs to their change sets in `engine.db`, forward only
/// and idempotent. Input: the engine database. Output: Ok, or `checks_unavailable`.
pub(crate) fn initialize(connection: &Connection) -> Result<(), CheckRefusal> {
    connection.execute_batch(SCHEMA).map_err(unavailable)
}

/// Records one check execution as one trackwright run in the caller's transaction, as the
/// engine actor `engine:checks`: the run with its batch, source (repository label, commit,
/// tree, base, change set) and policy (profile and approval, check, kind, argv, toolchain
/// family and version, sandbox and environment digests, network, timeout), its results
/// parsed from the report or the exit status, its end and verdict by the core's rules
/// (interrupted for a timeout or cancel), the integrity evidence and the change set's
/// binding to the run. The run ends through `coverage::finish` or `coverage::cancel`,
/// which re-evaluate the tickets whose test checks it covers through the host in the same
/// transaction, so a rolled-back transaction leaves no run and no ticket change. Results
/// link to cases by stable name, so a holdout test the owner keeps as a protected case
/// links to it; a holdout run's results keep no message and no artifact whatever case they
/// link to, so no reader's view depends on that link to withhold them. Inputs: the
/// transaction, the host, the recording and the time. Output: the run as stored, or
/// `checks_record_invalid`, `project_not_attached` or a core refusal's code.
pub fn store(
    tx: &Transaction<'_>,
    host: &Host,
    recording: &Recording<'_>,
    now_ms: i64,
) -> Result<RunView, CheckRefusal> {
    let Recording {
        change_set,
        batch,
        snapshot,
        check,
        execution,
        integrity,
    } = *recording;
    if execution.holdout != check.holdout.is_some() || snapshot.commit.is_empty() {
        return Err(failure("checks_record_invalid"));
    }
    let (project, _) =
        landing::change_set(tx, change_set)?.ok_or_else(|| failure("checks_record_invalid"))?;
    let key = tracker::key_of(tx, &project)?.ok_or_else(|| failure("project_not_attached"))?;
    let bound = binding::bind(check, execution);
    assert!(
        !execution.holdout
            || bound
                .results
                .iter()
                .all(|result| result.message.is_none() && result.artifact.is_none()),
        "a holdout run keeps no result message or artifact"
    );
    let engine = CoreActor::new(ENGINE_ACTOR).expect("the engine actor is valid");
    let family = binding::family(check.spec.toolchain);
    let digest = |value: &str| {
        if value.is_empty() {
            EMPTY_DIGEST.to_owned()
        } else {
            value.to_owned()
        }
    };
    let policy = PolicyRef {
        profile: check.profile.as_str().to_owned(),
        profile_approved_ms: check.approved_ms,
        check: check.spec.id.clone(),
        kind: binding::case_kind(check.spec.kind),
        argv: check.spec.argv.clone(),
        toolchain: Toolchain {
            family,
            version: if family == ToolchainFamily::None {
                String::new()
            } else {
                execution.toolchain_version.clone()
            },
        },
        sandbox: digest(&execution.sandbox_digest),
        network: binding::network(execution.network),
        timeout_s: check.spec.timeout_s,
        environment: digest(&execution.environment_digest),
    };
    let source = SourceRef::Git {
        repository: project.as_str().to_owned(),
        commit: snapshot.commit.clone(),
        tree: snapshot.tree.clone(),
        base: Some(snapshot.base.clone()),
        change_set: Some(change_set.as_str().to_owned()),
    };
    let start = RunStart {
        project: key,
        title: check.spec.id.clone(),
        plan: None,
        batch: Some(batch.as_str().to_owned()),
        source,
        policy,
    };
    let mut view = runs::start(tx, &engine, &start, now_ms).map_err(core)?;
    for chunk in bound.results.chunks(RESULT_ROWS_PER_RECORD_MAX) {
        let record = RunRecord {
            id: view.id.clone(),
            version: view.version,
            results: chunk.to_vec(),
        };
        view = runs::record(tx, &engine, &record, now_ms).map_err(core)?;
    }
    let finish = |end, exit_code, reason| RunFinish {
        id: view.id.clone(),
        version: view.version,
        end,
        exit_code,
        reason,
        integrity: integrity.cloned(),
    };
    let ended = match bound.ending {
        Ending::Stopped(reason) => {
            let cancel = RunCancel {
                id: view.id.clone(),
                version: view.version,
                reason,
            };
            coverage::cancel(tx, host, &engine, &cancel, now_ms)
        }
        Ending::NotStarted(code) => {
            let mut input = finish(RunEnd::NotStarted, None, Some(code));
            input.integrity = None;
            coverage::finish(tx, host, &engine, &input, now_ms)
        }
        Ending::ReportUnreadable(reason) => {
            let input = finish(RunEnd::ReportUnreadable, execution.exit, Some(reason));
            coverage::finish(tx, host, &engine, &input, now_ms)
        }
        Ending::BuildFailed => {
            let input = finish(RunEnd::BuildFailed, execution.exit, None);
            coverage::finish(tx, host, &engine, &input, now_ms)
        }
        Ending::Completed => {
            let input = finish(RunEnd::Completed, execution.exit, None);
            coverage::finish(tx, host, &engine, &input, now_ms)
        }
    }
    .map_err(core)?
    .run;
    assert_ne!(ended.state, RunState::Running);
    assert_eq!(ended.id, view.id);
    let run = row_of(tx, &ended.id)?;
    let protected: Vec<String> = integrity
        .map(|block| block.protected_touched.clone())
        .unwrap_or_default();
    let protected = serde_json::to_string(&protected).map_err(unavailable)?;
    let added = tx
        .execute(
            "INSERT INTO check_runs(run, short, change_set, check_id, batch, commit_id, tree,
                 profile, profile_approved_ms, protected_json, holdout)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                run,
                String::from(ended.id.clone()),
                change_set.as_str(),
                check.spec.id,
                batch.as_str(),
                snapshot.commit,
                snapshot.tree,
                check.profile.as_str(),
                check.approved_ms,
                protected,
                execution.holdout,
            ],
        )
        .map_err(unavailable)?;
    assert_eq!(added, 1);
    Ok(ended)
}

/// The `tk_runs` row behind a run's short id. Inputs: the transaction and the id. Output:
/// the row, or `checks_unavailable`.
pub(crate) fn row_of(db: &Connection, id: &TestId) -> Result<i64, CheckRefusal> {
    db.query_row(ROW_OF, params![id.key().as_str(), id.number()], |row| {
        row.get(0)
    })
    .optional()
    .map_err(unavailable)?
    .ok_or_else(|| failure("checks_unavailable"))
}

/// The newest finished or interrupted run of each check recorded on a change set, any head,
/// sorted by check id; the gate compares each one's tree and profile with the reviewed head
/// and the current profile. Inputs: the engine connection or a transaction on it, and the
/// change set. Output: the runs, or `checks_unavailable`.
pub fn latest(db: &Connection, change_set: &ChangeSetId) -> Result<Vec<CheckRun>, CheckRefusal> {
    let mut statement = db.prepare(LATEST).map_err(unavailable)?;
    let rows = statement
        .query_map([change_set.as_str()], |row| {
            Ok(Stored {
                check: row.get(0)?,
                run: row.get(1)?,
                short: row.get(2)?,
                batch: row.get(3)?,
                state: row.get(4)?,
                verdict: row.get(5)?,
                commit: row.get(6)?,
                tree: row.get(7)?,
                profile: row.get(8)?,
                profile_approved_ms: row.get(9)?,
                protected: row.get(10)?,
                holdout: row.get(11)?,
                finished_ms: row.get(12)?,
            })
        })
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    rows.into_iter().map(Stored::read).collect()
}

/// One `latest` row as stored, before its text columns are checked.
struct Stored {
    check: String,
    run: i64,
    short: String,
    batch: String,
    state: String,
    verdict: Option<String>,
    commit: String,
    tree: String,
    profile: String,
    profile_approved_ms: i64,
    protected: String,
    holdout: bool,
    finished_ms: Option<i64>,
}

impl Stored {
    /// Checks the stored text columns. Output: the run, or `checks_unavailable` for a
    /// column no recorder writes.
    fn read(self) -> Result<CheckRun, CheckRefusal> {
        let verdict = match self.verdict {
            Some(text) => Some(Verdict::parse(&text).ok_or_else(|| unavailable(()))?),
            None => None,
        };
        Ok(CheckRun {
            check: self.check,
            run: self.run,
            id: TestId::parse(&self.short).map_err(unavailable)?,
            batch: BatchId::new(self.batch).map_err(unavailable)?,
            state: RunState::parse(&self.state).ok_or_else(|| unavailable(()))?,
            verdict,
            commit: self.commit,
            tree: self.tree,
            profile: ProfileId::new(self.profile).map_err(unavailable)?,
            profile_approved_ms: self.profile_approved_ms,
            protected_touched: serde_json::from_str(&self.protected).map_err(unavailable)?,
            holdout: self.holdout,
            finished_ms: self.finished_ms,
        })
    }
}
