mod history;
mod shape;

pub use crate::tests_model::{
    ResultOutcome, RunRetention, RunState, RunWriters, TestId, TestObject, Verdict,
};
pub use history::{StoredRun, replay, stored};
pub use shape::{
    CHECK_ID_BYTES_MAX, INTEGRITY_PATH_BYTES_MAX, IntegrityRef, Inventory, NetworkMode, NewSkip,
    PolicyRef, RunEnd, SKIP_MARKER_BYTES_MAX, SOURCE_JSON_BYTES_MAX, SourceRef, StopReason,
    TIMEOUT_S_MAX, Toolchain, ToolchainFamily, is_commit,
};

use crate::host::{Authority, Sight};
use crate::model::{Actor, ProjectKey, Refusal, codes as core_codes};
use crate::store::{self, StoreError};
use crate::tests_model::{
    CaseKind, LIST_ROWS_MAX, LIST_TITLE_BYTES_MAX, OWNER_ACTOR, RESULT_ARTIFACT_BYTES_MAX,
    RESULT_MESSAGE_BYTES_MAX, RESULT_NAME_BYTES_MAX, RESULT_ROWS_PER_RECORD_MAX,
    RESULTS_PER_RUN_MAX, RUN_REASON_BYTES_MAX, RUN_RETAINED_DAYS_MAX, RUN_SHOW_PAGE_ROWS,
    RUN_TITLE_BYTES_MAX, RUNS_RETAINED_PER_PROJECT_MAX,
};
use crate::tests_store;
use rusqlite::{OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// The refusal codes of runs, declared once in `model::codes`: the test model's run and plan
/// codes plus the ones runs add.
pub mod codes {
    pub use crate::model::codes::{
        PLAN_UNKNOWN, RESULT_NAME_REPEATED, RESULTS_EMPTY, RESULTS_TOO_MANY, RUN_BATCH_INVALID,
        RUN_CLOSED, RUN_FINISH_INVALID, RUN_INTEGRITY_INVALID, RUN_POLICY_INVALID,
        RUN_RESULTS_FULL, RUN_SOURCE_INVALID, RUN_UNKNOWN, RUN_WRITER_REFUSED, RUNS_FULL,
        TEST_ID_INVALID,
    };
}

pub const RUN_START: &str = "run.start";
pub const RUN_RECORD: &str = "run.record";
pub const RUN_FINISH: &str = "run.finish";
pub const RUN_CANCEL: &str = "run.cancel";
pub const RUN_PRUNE: &str = "run.prune";

/// Every event op runs write and replay. The ticket replay skips these.
pub const OPS: &[&str] = &[RUN_START, RUN_RECORD, RUN_FINISH, RUN_CANCEL, RUN_PRUNE];

pub const RESULT_MESSAGE_INPUT_BYTES_MAX: usize = 1024 * 1024;
pub const PRUNE_PER_START_MAX: usize = 64;
pub const PAGE_BYTES_MAX: usize = 6 * 1024;
pub const UNLINKED_BYTES_MAX: usize = 2 * 1024;
pub const ELIDED_MARK: &str = "[elided] ";
pub const ENGINE_ACTOR: &str = "engine:checks";

const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
const POLICY_RUN_WRITERS: &str = "run_writers";
const POLICY_RUN_RETENTION: &str = "run_retention";

const _: () = assert!(ELIDED_MARK.len() < RESULT_MESSAGE_BYTES_MAX);
const _: () = assert!(RESULT_MESSAGE_BYTES_MAX < RESULT_MESSAGE_INPUT_BYTES_MAX);
const _: () = assert!(RESULT_ROWS_PER_RECORD_MAX <= RESULTS_PER_RUN_MAX);
const _: () = assert!(PRUNE_PER_START_MAX < RUNS_RETAINED_PER_PROJECT_MAX);

const PROJECT_ID: &str = "SELECT id FROM tk_projects WHERE key = ?1";
const PLAN_ID: &str = "SELECT id FROM tk_plans WHERE project = ?1 AND number = ?2";
const POLICY_VALUE: &str = "SELECT value_json FROM tk_policy WHERE project = ?1 AND key = ?2";
const CASE_NUMBER: &str = "SELECT number FROM tk_cases WHERE id = ?1";
const RUN_NEXT_NUMBER: &str = "SELECT coalesce(max(number), 0) + 1 FROM tk_runs WHERE project = ?1";
const RUN_COUNT: &str = "SELECT count(*) FROM tk_runs WHERE project = ?1";
const RUN_INSERT: &str = "INSERT INTO tk_runs(project, number, plan_id, batch, title, state,
    verdict, reason, source_json, policy_json, integrity_json, started_ms, finished_ms,
    recorded_by, total, passed, failed, skipped, errored, version)
    VALUES (?1, ?2, ?3, ?4, ?5, 'running', NULL, NULL, ?6, ?7, NULL, ?8, NULL, ?9,
    0, 0, 0, 0, 0, 1)";
const RUN_SELECT: &str = "SELECT r.id, r.number, p.number, r.batch, r.title, r.state,
    r.verdict, r.reason, r.source_json, r.policy_json, r.integrity_json, r.started_ms,
    r.finished_ms, r.recorded_by, r.total, r.passed, r.failed, r.skipped, r.errored, r.version
    FROM tk_runs r LEFT JOIN tk_plans p ON p.id = r.plan_id
    WHERE r.project = ?1 AND r.number = ?2";
const RUN_TOTALS_UPDATE: &str = "UPDATE tk_runs SET total = ?2, passed = ?3, failed = ?4,
    skipped = ?5, errored = ?6, version = version + 1
    WHERE id = ?1 AND version = ?7 AND state = 'running'";
const RUN_END_UPDATE: &str = "UPDATE tk_runs SET state = ?2, verdict = ?3, reason = ?4,
    integrity_json = ?5, finished_ms = ?6, version = version + 1
    WHERE id = ?1 AND version = ?7 AND state = 'running'";
const RESULT_NAME_EXISTS: &str = "SELECT 1 FROM tk_results WHERE run_id = ?1 AND name = ?2";
const RESULT_INSERT: &str = "INSERT INTO tk_results(run_id, position, case_id, name, outcome,
    duration_ms, message, artifact) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";
const RESULTS_FROM: &str = "SELECT res.position, res.name, res.outcome, c.number,
    res.duration_ms, res.message, res.artifact
    FROM tk_results res LEFT JOIN tk_cases c ON c.id = res.case_id
    WHERE res.run_id = ?1 AND res.position > ?2 ORDER BY res.position LIMIT ?3";
const UNLINKED_NAMES: &str = "SELECT name FROM tk_results
    WHERE run_id = ?1 AND case_id IS NULL ORDER BY position LIMIT ?2";
const UNLINKED_COUNT: &str =
    "SELECT count(*) FROM tk_results WHERE run_id = ?1 AND case_id IS NULL";
const RUN_LIST: &str = "SELECT r.number, r.title, r.state, r.verdict, r.started_ms,
    r.finished_ms, r.total, r.passed, r.failed, r.skipped, r.errored
    FROM tk_runs r
    WHERE r.project = ?1 AND (?2 IS NULL OR r.plan_id = ?2) AND (?3 IS NULL OR r.batch = ?3)
    AND (?4 IS NULL OR r.verdict = ?4)
    AND (?5 IS NULL OR json_extract(r.source_json, '$.commit') = ?5)
    AND r.number < ?6 ORDER BY r.number DESC LIMIT ?7";
const PRUNE_CANDIDATES: &str = "SELECT r.id, r.number FROM tk_runs r
    WHERE r.project = ?1 AND r.state <> 'running' AND r.number < ?2
    AND (?4 IS NULL OR r.started_ms < ?4)
    AND NOT EXISTS (SELECT 1 FROM tk_evidence e WHERE e.ref = ?3 || r.number)
    AND NOT EXISTS (SELECT 1 FROM tk_links l WHERE l.target = ?3 || r.number)
    AND NOT EXISTS (SELECT 1 FROM tk_results mine
        WHERE mine.run_id = r.id AND mine.case_id IS NOT NULL
        AND NOT EXISTS (SELECT 1 FROM tk_results later
            JOIN tk_runs newer ON newer.id = later.run_id
            WHERE later.case_id = mine.case_id AND newer.number > r.number))
    ORDER BY r.number LIMIT ?5";
const PRUNE_RESULTS: &str = "DELETE FROM tk_results WHERE run_id = ?1";
const PRUNE_RUN: &str = "DELETE FROM tk_runs WHERE id = ?1";
const EVENT_INSERT: &str = "INSERT INTO tk_events(ticket, project, at_ms, actor, op,
    before_json, after_json) VALUES (NULL, ?1, ?2, ?3, ?4, ?5, ?6)";

/// Every SQL statement runs prepare, so a test proves each names only core tables. Output:
/// the statements.
pub fn statements() -> &'static [&'static str] {
    &[
        PROJECT_ID,
        PLAN_ID,
        POLICY_VALUE,
        CASE_NUMBER,
        RUN_NEXT_NUMBER,
        RUN_COUNT,
        RUN_INSERT,
        RUN_SELECT,
        RUN_TOTALS_UPDATE,
        RUN_END_UPDATE,
        RESULT_NAME_EXISTS,
        RESULT_INSERT,
        RESULTS_FROM,
        UNLINKED_NAMES,
        UNLINKED_COUNT,
        RUN_LIST,
        PRUNE_CANDIDATES,
        PRUNE_RESULTS,
        PRUNE_RUN,
        EVENT_INSERT,
        history::RUNS_OF_PROJECT,
        history::EVENTS_OF_RUNS,
    ]
}

impl Verdict {
    /// Tells whether the verdict proves anything. Output: true for `passed` alone, so a
    /// zero-test, build-failed, unknown or unavailable run never satisfies a check.
    pub fn satisfies(self) -> bool {
        self == Verdict::Passed
    }
}

/// Starts a run. Fields: the project, a title, the plan it runs, the batch label grouping
/// one trigger's runs, and the source and policy it covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunStart {
    pub project: ProjectKey,
    pub title: String,
    pub plan: Option<TestId>,
    pub batch: Option<String>,
    pub source: SourceRef,
    pub policy: PolicyRef,
}

/// One result to record. Fields: the runner's stable name, the outcome, the duration, a
/// failure message (tail kept past 2 KiB) and an artifact reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewResult {
    pub name: String,
    pub outcome: ResultOutcome,
    pub duration_ms: Option<u32>,
    pub message: Option<String>,
    pub artifact: Option<String>,
}

/// Appends at most 500 results to a running run. Fields: the run, the version read and
/// the results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub id: TestId,
    pub version: i64,
    pub results: Vec<NewResult>,
}

/// Finishes a run. Fields: the run, the version read, how it ended, the process exit code
/// if one ran, the reason it could not start (only with `not_started`), and the host's
/// integrity evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunFinish {
    pub id: TestId,
    pub version: i64,
    pub end: RunEnd,
    pub exit_code: Option<i32>,
    pub reason: Option<String>,
    pub integrity: Option<IntegrityRef>,
}

/// Interrupts a running run. Fields: the run, the version read and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCancel {
    pub id: TestId,
    pub version: i64,
    pub reason: StopReason,
}

/// Shows a run with one page of results. Fields: the run and the last position already
/// read (the previous page's cursor).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunShow {
    pub id: TestId,
    pub after: Option<u32>,
}

/// Lists a project's runs newest first. Fields: the project, optional plan, batch,
/// verdict and source commit filters, and the run number to list below (the previous
/// page's cursor).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunList {
    pub project: ProjectKey,
    pub plan: Option<TestId>,
    pub batch: Option<String>,
    pub verdict: Option<Verdict>,
    pub commit: Option<String>,
    pub before: Option<u32>,
}

/// A run's result counts. `total` counts every result, `not_run` ones included; `passed`
/// and `failed` are the executed ones.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub errored: u32,
}

impl Totals {
    /// The results that ran a test body to an answer. Output: passed plus failed.
    pub fn executed(&self) -> u32 {
        let executed = self.passed + self.failed;
        assert!(executed <= self.total);
        executed
    }

    fn counted(&self, outcomes: impl Iterator<Item = ResultOutcome>) -> Result<Totals, Refusal> {
        let mut next = *self;
        for outcome in outcomes {
            next.total += 1;
            match outcome {
                ResultOutcome::Passed => next.passed += 1,
                ResultOutcome::Failed => next.failed += 1,
                ResultOutcome::Skipped => next.skipped += 1,
                ResultOutcome::Errored => next.errored += 1,
                ResultOutcome::NotRun => {}
            }
        }
        if next.total as usize > RESULTS_PER_RUN_MAX {
            return Err(Refusal::new(
                codes::RUN_RESULTS_FULL,
                format!("a run holds at most {RESULTS_PER_RUN_MAX} results"),
            ));
        }
        assert!(next.passed + next.failed + next.skipped + next.errored <= next.total);
        assert!(next.total >= self.total);
        Ok(next)
    }
}

/// A run as callers see it: its id, plan, batch, title, state, verdict and reason, the
/// source, policy and integrity it is bound to, when it started and finished, who
/// recorded it, its counts and its version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunView {
    pub id: TestId,
    pub plan: Option<TestId>,
    pub batch: Option<String>,
    pub title: String,
    pub state: RunState,
    pub verdict: Option<Verdict>,
    pub reason: Option<String>,
    pub source: SourceRef,
    pub policy: PolicyRef,
    pub integrity: Option<IntegrityRef>,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    pub recorded_by: Actor,
    pub totals: Totals,
    pub version: i64,
}

/// One stored result: its position in the run (from 1), the stable name, outcome, the
/// case it maps to (None when unlinked), duration, message and artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultView {
    pub position: u32,
    pub name: String,
    pub outcome: ResultOutcome,
    pub case: Option<TestId>,
    pub duration_ms: Option<u32>,
    pub message: Option<String>,
    pub artifact: Option<String>,
}

/// One page of `run_show`: the run, its results after the cursor, the first unlinked
/// result names (at most 50 and 2 KiB) with their total, and the cursor for the next page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunPage {
    pub run: RunView,
    pub results: Vec<ResultView>,
    pub unlinked: Vec<String>,
    pub unlinked_total: u32,
    pub cursor: Option<u32>,
}

/// One row of `run_list`: id, title (cut past 80 bytes, `elided` set), state, verdict,
/// times and counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRow {
    pub id: TestId,
    pub title: String,
    pub elided: bool,
    pub state: RunState,
    pub verdict: Option<Verdict>,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    pub totals: Totals,
}

/// One page of `run_list`: the rows and the cursor (the last row's number) when more
/// remain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunListPage {
    pub rows: Vec<RunRow>,
    pub cursor: Option<u32>,
}

/// A run as an actor with `Outcome` sight reads it: its id, state, verdict, times, counts
/// and version, with no title, source, policy, integrity or result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOutcome {
    pub id: TestId,
    pub state: RunState,
    pub verdict: Option<Verdict>,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
    pub totals: Totals,
    pub version: i64,
}

impl RunOutcome {
    fn of(run: &RunView) -> RunOutcome {
        RunOutcome {
            id: run.id.clone(),
            state: run.state,
            verdict: run.verdict,
            started_ms: run.started_ms,
            finished_ms: run.finished_ms,
            totals: run.totals,
            version: run.version,
        }
    }
}

/// One run as `run_show` gives it to an actor: the page under `Full` sight, or the outcome
/// alone under `Outcome` sight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "sight", rename_all = "snake_case")]
pub enum Sighted {
    Full(Box<RunPage>),
    Outcome(RunOutcome),
}

#[derive(Debug, Serialize, Deserialize)]
struct RecordEvent {
    id: TestId,
    version: i64,
    totals: Totals,
    results: Vec<ResultView>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PruneEvent {
    id: TestId,
}

struct Loaded {
    row_id: i64,
    project_id: i64,
    view: RunView,
}

struct Ending {
    state: RunState,
    verdict: Option<Verdict>,
    reason: Option<String>,
    integrity: Option<String>,
    op: &'static str,
}

/// Starts a running run with its source and policy bound, writes its event, then prunes
/// per `run_retention`, all in one savepoint. Inputs: the transaction, the writer, the
/// start and the time. Output: the run, or an error that wrote nothing.
pub fn start(
    tx: &Transaction<'_>,
    actor: &Actor,
    input: &RunStart,
    now_ms: i64,
) -> Result<RunView, StoreError> {
    store::write(tx, || {
        let project_id = project_id(tx, &input.project)?;
        writer_may(tx, project_id, actor, input.policy.kind)?;
        shape::run_text("run title", &input.title, RUN_TITLE_BYTES_MAX, false)
            .map_err(StoreError::Refused)?;
        batch_fits(input.batch.as_deref())?;
        let plan_id = match &input.plan {
            Some(plan) => Some(plan_id(tx, project_id, &input.project, plan)?),
            None => None,
        };
        input.source.check().map_err(StoreError::Refused)?;
        input.policy.check().map_err(StoreError::Refused)?;
        let next: i64 = tx.query_row(RUN_NEXT_NUMBER, [project_id], |row| row.get(0))?;
        let number = u32::try_from(next)
            .map_err(|_| refused(codes::RUNS_FULL, "the project has used every run number"))?;
        let id = run_id(&input.project, number)?;
        tx.prepare(RUN_INSERT)?.execute((
            project_id,
            number,
            plan_id,
            &input.batch,
            &input.title,
            json(&input.source),
            json(&input.policy),
            now_ms,
            actor.as_str(),
        ))?;
        let run = load(tx, &id)?;
        assert_eq!(run.view.state, RunState::Running);
        assert_eq!(run.view.version, 1);
        assert_eq!(run.view.totals, Totals::default());
        event(
            tx,
            project_id,
            actor,
            RUN_START,
            None,
            Some(json(&run.view)),
            now_ms,
        )?;
        prune(tx, &input.project, project_id, number, now_ms)?;
        Ok(run.view)
    })
}

/// Appends results to a running run. Each name maps to the project's case with that
/// stable name, or stays unlinked. The batch is refused whole: empty, over 500 rows, a
/// repeated or bad name, a bad message or artifact, or past 20,000 results per run. A
/// refused or failed record writes nothing and the run stays running at its version; the
/// host then finishes it `report_unreadable` with the refusal as its reason. Inputs: the
/// transaction, the writer, the record and the time. Output: the run.
pub fn record(
    tx: &Transaction<'_>,
    actor: &Actor,
    input: &RunRecord,
    now_ms: i64,
) -> Result<RunView, StoreError> {
    store::write(tx, || {
        let run = writable(tx, actor, &input.id, input.version)?;
        if input.results.is_empty() {
            return Err(refused(
                codes::RESULTS_EMPTY,
                "a record holds at least one result",
            ));
        }
        if input.results.len() > RESULT_ROWS_PER_RECORD_MAX {
            return Err(refused(
                codes::RESULTS_TOO_MANY,
                format!("a record holds at most {RESULT_ROWS_PER_RECORD_MAX} results"),
            ));
        }
        let rows = prepared(tx, &run, &input.results)?;
        let totals = run
            .view
            .totals
            .counted(rows.iter().map(|(_, row)| row.outcome))
            .map_err(StoreError::Refused)?;
        let mut insert = tx.prepare(RESULT_INSERT)?;
        for (case_id, row) in &rows {
            insert.execute((
                run.row_id,
                row.position,
                case_id,
                &row.name,
                row.outcome.as_str(),
                row.duration_ms,
                &row.message,
                &row.artifact,
            ))?;
        }
        let changed = tx.prepare(RUN_TOTALS_UPDATE)?.execute((
            run.row_id,
            totals.total,
            totals.passed,
            totals.failed,
            totals.skipped,
            totals.errored,
            input.version,
        ))?;
        assert_eq!(changed, 1);
        let after = load(tx, &input.id)?.view;
        assert_eq!(after.totals, totals);
        assert_eq!(after.version, input.version + 1);
        let written = RecordEvent {
            id: after.id.clone(),
            version: after.version,
            totals,
            results: rows.into_iter().map(|(_, row)| row).collect(),
        };
        event(
            tx,
            run.project_id,
            actor,
            RUN_RECORD,
            None,
            Some(json(&written)),
            now_ms,
        )?;
        Ok(after)
    })
}

/// Finishes a running run and stores its verdict by the core's locked rules, with the
/// host's integrity evidence. `not_started` needs a one-line reason and `report_unreadable`
/// may carry one; no other end takes a reason. It leaves tickets as they were: the
/// `run_finish` op and engines call `coverage::finish`. Inputs: the transaction, the writer,
/// the finish and the time. Output: the finished run, or an error that wrote nothing.
pub fn finish(
    tx: &Transaction<'_>,
    actor: &Actor,
    input: &RunFinish,
    now_ms: i64,
) -> Result<RunView, StoreError> {
    store::write(tx, || {
        let run = writable(tx, actor, &input.id, input.version)?;
        let totals = run.view.totals;
        ending_fits(input, &totals).map_err(StoreError::Refused)?;
        assert!(
            input.reason.is_none()
                || matches!(input.end, RunEnd::NotStarted | RunEnd::ReportUnreadable)
        );
        if let Some(integrity) = &input.integrity {
            integrity.check().map_err(StoreError::Refused)?;
        }
        let ending = Ending {
            state: RunState::Finished,
            verdict: Some(verdict(input.end, input.exit_code, &totals)),
            reason: input.reason.clone(),
            integrity: input.integrity.as_ref().map(json),
            op: RUN_FINISH,
        };
        end(tx, actor, &run, ending, now_ms)
    })
}

/// Interrupts a running run: no verdict, the reason `cancelled` or `timed_out`. It leaves
/// tickets as they were; the `run_cancel` op and engines call `coverage::cancel`, which
/// re-evaluates them. Inputs: the transaction, the writer, the cancel and the time. Output:
/// the interrupted run, or an error that wrote nothing.
pub fn cancel(
    tx: &Transaction<'_>,
    actor: &Actor,
    input: &RunCancel,
    now_ms: i64,
) -> Result<RunView, StoreError> {
    store::write(tx, || {
        let run = writable(tx, actor, &input.id, input.version)?;
        let ending = Ending {
            state: RunState::Interrupted,
            verdict: None,
            reason: Some(input.reason.as_str().to_owned()),
            integrity: None,
            op: RUN_CANCEL,
        };
        end(tx, actor, &run, ending, now_ms)
    })
}

/// Shows one run and a page of its results, filled to at most 50 rows and 6 KiB. Inputs:
/// the transaction and the request. Output: the page, or `run_unknown`.
pub fn show(tx: &Transaction<'_>, input: &RunShow) -> Result<RunPage, StoreError> {
    let run = load(tx, &input.id)?;
    let after = input.after.unwrap_or(0);
    let fetched = results_from(tx, &run, after, RUN_SHOW_PAGE_ROWS + 1)?;
    let (results, more) = fill(fetched, RUN_SHOW_PAGE_ROWS, PAGE_BYTES_MAX);
    let cursor = if more {
        Some(
            results
                .last()
                .expect("a filled page keeps one row")
                .position,
        )
    } else {
        None
    };
    let mut names = tx.prepare(UNLINKED_NAMES)?;
    let fetched: Vec<String> = names
        .query_map((run.row_id, sql_count(RUN_SHOW_PAGE_ROWS)?), |row| {
            row.get(0)
        })?
        .collect::<Result<_, _>>()?;
    let (unlinked, _) = fill(fetched, RUN_SHOW_PAGE_ROWS, UNLINKED_BYTES_MAX);
    let unlinked_total: u32 = tx.query_row(UNLINKED_COUNT, [run.row_id], |row| row.get(0))?;
    assert!(unlinked.len() <= unlinked_total as usize);
    Ok(RunPage {
        run: run.view,
        results,
        unlinked,
        unlinked_total,
        cursor,
    })
}

/// Lists a project's runs newest first, filtered, filled to at most 50 rows and 6 KiB.
/// Inputs: the transaction and the request. Output: the page.
pub fn list(tx: &Transaction<'_>, input: &RunList) -> Result<RunListPage, StoreError> {
    let project_id = project_id(tx, &input.project)?;
    let plan_id = match &input.plan {
        Some(plan) => Some(plan_id(tx, project_id, &input.project, plan)?),
        None => None,
    };
    batch_fits(input.batch.as_deref())?;
    if let Some(commit) = &input.commit
        && !is_commit(commit)
    {
        return Err(refused(
            codes::RUN_SOURCE_INVALID,
            "a commit filter is 40 lowercase hex digits",
        ));
    }
    let before = input.before.map_or(i64::MAX, i64::from);
    let verdict = input.verdict.map(Verdict::as_str);
    let mut statement = tx.prepare(RUN_LIST)?;
    let fetched: Vec<RunRow> = statement
        .query_map(
            (
                project_id,
                plan_id,
                &input.batch,
                verdict,
                &input.commit,
                before,
                sql_count(LIST_ROWS_MAX + 1)?,
            ),
            |row| list_row(&input.project, row),
        )?
        .collect::<Result<_, _>>()?;
    let (rows, more) = fill(fetched, LIST_ROWS_MAX, PAGE_BYTES_MAX);
    let cursor = if more {
        Some(
            rows.last()
                .expect("a filled page keeps one row")
                .id
                .number(),
        )
    } else {
        None
    };
    Ok(RunListPage { rows, cursor })
}

/// Shows one run as the actor may read it under the host's `run_sight`: the page of
/// `show`, or only its outcome. Inputs: the transaction, the host, the actor and the
/// request. Output: the sighted run, `run_unknown`, or the host's refusal.
pub fn show_as(
    tx: &Transaction<'_>,
    host: &(impl Authority + ?Sized),
    actor: &Actor,
    input: &RunShow,
) -> Result<Sighted, StoreError> {
    match host.run_sight(tx, input.id.key(), actor, &input.id)? {
        Sight::Full => show(tx, input).map(|page| Sighted::Full(Box::new(page))),
        Sight::Outcome => Ok(Sighted::Outcome(RunOutcome::of(&load(tx, &input.id)?.view))),
    }
}

/// Lists a project's runs as `list` does, with the title of each run the actor reads as
/// an outcome only left empty. Inputs: the transaction, the host, the actor and the
/// request. Output: the page, or the host's refusal.
pub fn list_as(
    tx: &Transaction<'_>,
    host: &(impl Authority + ?Sized),
    actor: &Actor,
    input: &RunList,
) -> Result<RunListPage, StoreError> {
    let mut page = list(tx, input)?;
    for row in &mut page.rows {
        if host.run_sight(tx, &input.project, actor, &row.id)? == Sight::Outcome {
            row.title.clear();
            row.elided = false;
        }
    }
    Ok(page)
}

/// Computes a finished run's verdict, the locked rules shared by every host. A completed
/// run: a nonzero exit with no results is `build_failed`; an exit status that contradicts
/// the results is `unknown`; any failed or errored result is `failed`; zero executed is
/// `no_tests`; else `passed`. A check whose report is only its exit status records one
/// result named after the check, so exit 0 passes and a nonzero exit fails. Inputs: how it
/// ended, the exit code and the counts. Output: the verdict.
pub(crate) fn verdict(end: RunEnd, exit_code: Option<i32>, totals: &Totals) -> Verdict {
    let broken = totals.failed + totals.errored;
    let executed = totals.executed();
    let verdict = match end {
        RunEnd::NotStarted => Verdict::Unavailable,
        RunEnd::BuildFailed => Verdict::BuildFailed,
        RunEnd::ReportUnreadable => Verdict::Unknown,
        RunEnd::Completed => match exit_code {
            Some(code) if code != 0 && totals.total == 0 => Verdict::BuildFailed,
            Some(0) if broken > 0 => Verdict::Unknown,
            Some(code) if code != 0 && broken == 0 => Verdict::Unknown,
            _ if broken > 0 => Verdict::Failed,
            _ if executed == 0 => Verdict::NoTests,
            _ => Verdict::Passed,
        },
    };
    if verdict.satisfies() {
        assert_eq!(end, RunEnd::Completed);
        assert!(executed >= 1);
        assert_eq!(broken, 0);
        assert!(exit_code.is_none_or(|code| code == 0));
    }
    verdict
}

fn ending_fits(input: &RunFinish, totals: &Totals) -> Result<(), Refusal> {
    let invalid = |message: &str| Refusal::new(codes::RUN_FINISH_INVALID, message);
    match (input.end, &input.reason) {
        (RunEnd::NotStarted, None) => {
            return Err(invalid("a run that could not start says why in its reason"));
        }
        (RunEnd::NotStarted, Some(reason)) => {
            shape::run_text("run reason", reason, RUN_REASON_BYTES_MAX, false)?;
            if totals.total > 0 || input.exit_code.is_some() {
                return Err(invalid(
                    "a run that could not start has no results and no exit",
                ));
            }
        }
        (RunEnd::ReportUnreadable, Some(reason)) => {
            shape::run_text("run reason", reason, RUN_REASON_BYTES_MAX, false)?;
        }
        (_, Some(_)) => {
            return Err(invalid(
                "only a run that could not start or read its report takes a reason",
            ));
        }
        (RunEnd::BuildFailed, None) if totals.total > 0 => {
            return Err(invalid(
                "a build that failed before tests ran has no results",
            ));
        }
        (_, None) => {}
    }
    Ok(())
}

fn end(
    tx: &Transaction<'_>,
    actor: &Actor,
    run: &Loaded,
    ending: Ending,
    now_ms: i64,
) -> Result<RunView, StoreError> {
    assert_ne!(ending.state, RunState::Running);
    assert_eq!(ending.verdict.is_some(), ending.state == RunState::Finished);
    let changed = tx.prepare(RUN_END_UPDATE)?.execute((
        run.row_id,
        ending.state.as_str(),
        ending.verdict.map(Verdict::as_str),
        &ending.reason,
        &ending.integrity,
        now_ms,
        run.view.version,
    ))?;
    assert_eq!(changed, 1);
    let after = load(tx, &run.view.id)?.view;
    assert_eq!(after.state, ending.state);
    assert_eq!(after.version, run.view.version + 1);
    event(
        tx,
        run.project_id,
        actor,
        ending.op,
        None,
        Some(json(&after)),
        now_ms,
    )?;
    Ok(after)
}

fn writable(
    tx: &Transaction<'_>,
    actor: &Actor,
    id: &TestId,
    version: i64,
) -> Result<Loaded, StoreError> {
    let run = load(tx, id)?;
    writer_may(tx, run.project_id, actor, run.view.policy.kind)?;
    if run.view.version != version {
        return Err(refused(
            core_codes::STALE_VERSION,
            format!("the run is at version {}", run.view.version),
        ));
    }
    if run.view.state != RunState::Running {
        return Err(refused(
            codes::RUN_CLOSED,
            format!("the run is {} and takes no more writes", run.view.state),
        ));
    }
    Ok(run)
}

fn writer_may(
    tx: &Transaction<'_>,
    project_id: i64,
    actor: &Actor,
    kind: CaseKind,
) -> Result<(), StoreError> {
    let writers = policy_choice(tx, project_id, POLICY_RUN_WRITERS, RunWriters::parse)?
        .unwrap_or(RunWriters::Any);
    let manual = kind == CaseKind::Manual;
    let allowed = match writers {
        RunWriters::Any => true,
        RunWriters::EngineOnly => {
            (actor.as_str() == ENGINE_ACTOR && !manual) || (actor.as_str() == OWNER_ACTOR && manual)
        }
    };
    if !allowed {
        return Err(refused(
            codes::RUN_WRITER_REFUSED,
            "here only the engine writes automated runs and only the owner manual runs",
        ));
    }
    Ok(())
}

fn batch_fits(batch: Option<&str>) -> Result<(), StoreError> {
    if let Some(batch) = batch
        && !shape::batch_label(batch)
    {
        return Err(refused(
            codes::RUN_BATCH_INVALID,
            "a batch is 1 to 64 bytes of letters, digits and _ . - :",
        ));
    }
    Ok(())
}

fn prepared(
    tx: &Transaction<'_>,
    run: &Loaded,
    results: &[NewResult],
) -> Result<Vec<(Option<i64>, ResultView)>, StoreError> {
    let mut seen: HashSet<&str> = HashSet::with_capacity(results.len());
    let mut exists = tx.prepare(RESULT_NAME_EXISTS)?;
    let mut rows = Vec::with_capacity(results.len());
    for (offset, result) in results.iter().enumerate() {
        result_fits(result).map_err(StoreError::Refused)?;
        if !seen.insert(result.name.as_str()) || exists.exists((run.row_id, &result.name))? {
            return Err(refused(
                codes::RESULT_NAME_REPEATED,
                "a result name appears once per run",
            ));
        }
        let case_id = tests_store::case_id_for(tx, run.project_id, &result.name)?;
        let case = match case_id {
            Some(case_id) => Some(case_ref(tx, run.view.id.key(), case_id)?),
            None => None,
        };
        let offset = u32::try_from(offset).expect("a record holds at most 500 results");
        rows.push((
            case_id,
            ResultView {
                position: run.view.totals.total + offset + 1,
                name: result.name.clone(),
                outcome: result.outcome,
                case,
                duration_ms: result.duration_ms,
                message: result.message.as_deref().map(elided),
                artifact: result.artifact.clone(),
            },
        ));
    }
    assert_eq!(rows.len(), results.len());
    Ok(rows)
}

fn result_fits(result: &NewResult) -> Result<(), Refusal> {
    shape::run_text("result name", &result.name, RESULT_NAME_BYTES_MAX, false)?;
    if let Some(message) = &result.message {
        shape::run_text(
            "result message",
            message,
            RESULT_MESSAGE_INPUT_BYTES_MAX,
            true,
        )?;
    }
    if let Some(artifact) = &result.artifact {
        shape::run_text(
            "result artifact",
            artifact,
            RESULT_ARTIFACT_BYTES_MAX,
            false,
        )?;
    }
    Ok(())
}

/// Keeps a message's tail when it is over 2 KiB, cut on a character boundary and marked
/// with `ELIDED_MARK`. Input: the message. Output: the stored message.
fn elided(message: &str) -> String {
    if message.len() <= RESULT_MESSAGE_BYTES_MAX {
        return message.to_owned();
    }
    let room = RESULT_MESSAGE_BYTES_MAX - ELIDED_MARK.len();
    let mut cut = message.len() - room;
    while !message.is_char_boundary(cut) {
        cut += 1;
    }
    let kept = format!("{ELIDED_MARK}{}", &message[cut..]);
    assert!(kept.len() <= RESULT_MESSAGE_BYTES_MAX);
    assert!(message.ends_with(&kept[ELIDED_MARK.len()..]));
    kept
}

/// Prunes per `run_retention` right after a start: at most 64 runs, oldest first, never
/// a running run, the newest run (so numbers are never reused), a run that evidence or a
/// ticket link names, or a run holding the newest result of any case. Each pruned run
/// writes one `run.prune` event. Inputs: the transaction, the project, the new run's
/// number and the time. Output: Ok.
fn prune(
    tx: &Transaction<'_>,
    key: &ProjectKey,
    project_id: i64,
    newest: u32,
    now_ms: i64,
) -> Result<(), StoreError> {
    let retention = policy_choice(tx, project_id, POLICY_RUN_RETENTION, RunRetention::parse)?
        .unwrap_or(RunRetention::PruneCount);
    let age_ms = i64::from(RUN_RETAINED_DAYS_MAX) * DAY_MS;
    let (limit, cutoff_ms) = match retention {
        RunRetention::KeepAll => return Ok(()),
        RunRetention::PruneCount => {
            let count: i64 = tx.query_row(RUN_COUNT, [project_id], |row| row.get(0))?;
            let count = usize::try_from(count)
                .map_err(|_| StoreError::Corrupt("a run count is negative".into()))?;
            let over = count.saturating_sub(RUNS_RETAINED_PER_PROJECT_MAX);
            if over == 0 {
                return Ok(());
            }
            (over.min(PRUNE_PER_START_MAX), None)
        }
        RunRetention::PruneAge => (PRUNE_PER_START_MAX, Some(now_ms.saturating_sub(age_ms))),
    };
    let prefix = format!("{key}-R");
    let mut candidates = tx.prepare(PRUNE_CANDIDATES)?;
    let pruned: Vec<(i64, u32)> = candidates
        .query_map(
            (project_id, newest, &prefix, cutoff_ms, sql_count(limit)?),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .collect::<Result<_, _>>()?;
    assert!(pruned.len() <= limit);
    let retention_actor = Actor::host("retention");
    for (row_id, number) in pruned {
        assert!(number < newest);
        tx.prepare(PRUNE_RESULTS)?.execute([row_id])?;
        let removed = tx.prepare(PRUNE_RUN)?.execute([row_id])?;
        assert_eq!(removed, 1);
        let before = json(&PruneEvent {
            id: run_id(key, number)?,
        });
        event(
            tx,
            project_id,
            &retention_actor,
            RUN_PRUNE,
            Some(before),
            None,
            now_ms,
        )?;
    }
    Ok(())
}

/// Fills a page in order to at most `rows_max` rows and `bytes_max` bytes of compact JSON,
/// keeping at least one row. Inputs: the rows fetched, one more than a page when there are
/// more, and the bounds. Output: the rows kept and whether rows were left over.
pub(crate) fn fill<T: Serialize>(
    rows: Vec<T>,
    rows_max: usize,
    bytes_max: usize,
) -> (Vec<T>, bool) {
    assert!(rows_max >= 1);
    assert!(bytes_max >= 1);
    let fetched = rows.len();
    let mut kept = Vec::with_capacity(fetched.min(rows_max));
    let mut bytes = 0;
    for row in rows {
        let size = serde_json::to_vec(&row)
            .expect("a page row always serializes")
            .len()
            + 1;
        if kept.len() == rows_max || (!kept.is_empty() && bytes + size > bytes_max) {
            break;
        }
        bytes += size;
        kept.push(row);
    }
    assert!(kept.len() <= rows_max);
    assert!(fetched == 0 || !kept.is_empty());
    let more = kept.len() < fetched;
    (kept, more)
}

fn run_id(key: &ProjectKey, number: u32) -> Result<TestId, StoreError> {
    let id = TestId::new(key.clone(), TestObject::Run, number).map_err(StoreError::Refused)?;
    assert_eq!(id.object(), TestObject::Run);
    Ok(id)
}

fn project_id(tx: &Transaction<'_>, key: &ProjectKey) -> Result<i64, StoreError> {
    tx.query_row(PROJECT_ID, [key.as_str()], |row| row.get(0))
        .optional()?
        .ok_or_else(|| refused(core_codes::PROJECT_UNKNOWN, format!("no project {key}")))
}

fn plan_id(
    tx: &Transaction<'_>,
    project_id: i64,
    key: &ProjectKey,
    plan: &TestId,
) -> Result<i64, StoreError> {
    if plan.object() != TestObject::Plan {
        return Err(refused(
            codes::TEST_ID_INVALID,
            format!("{plan} is not a plan id"),
        ));
    }
    let unknown = || {
        refused(
            codes::PLAN_UNKNOWN,
            format!("no plan {plan} in project {key}"),
        )
    };
    if plan.key() != key {
        return Err(unknown());
    }
    tx.query_row(PLAN_ID, (project_id, plan.number()), |row| row.get(0))
        .optional()?
        .ok_or_else(unknown)
}

fn case_ref(tx: &Transaction<'_>, key: &ProjectKey, case_id: i64) -> Result<TestId, StoreError> {
    let number: u32 = tx.query_row(CASE_NUMBER, [case_id], |row| row.get(0))?;
    let case = TestId::new(key.clone(), TestObject::Case, number).map_err(StoreError::Refused)?;
    assert_eq!(case.object(), TestObject::Case);
    Ok(case)
}

fn policy_choice<T>(
    tx: &Transaction<'_>,
    project_id: i64,
    policy: &str,
    parse: fn(&str) -> Option<T>,
) -> Result<Option<T>, StoreError> {
    let stored: Option<String> = tx
        .query_row(POLICY_VALUE, (project_id, policy), |row| row.get(0))
        .optional()?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    let value: Option<String> = serde_json::from_str(&stored).ok();
    match value.as_deref().and_then(parse) {
        Some(choice) => Ok(Some(choice)),
        None => Err(StoreError::Corrupt(format!(
            "the {policy} policy holds a value the core does not know"
        ))),
    }
}

fn load(tx: &Transaction<'_>, id: &TestId) -> Result<Loaded, StoreError> {
    if id.object() != TestObject::Run {
        return Err(refused(
            codes::TEST_ID_INVALID,
            format!("{id} is not a run id"),
        ));
    }
    let project_id = project_id(tx, id.key())?;
    let loaded = tx
        .query_row(RUN_SELECT, (project_id, id.number()), |row| {
            Ok((row.get::<_, i64>(0)?, stored_run(id.key(), row)))
        })
        .optional()?;
    let Some((row_id, view)) = loaded else {
        return Err(refused(codes::RUN_UNKNOWN, format!("no run {id}")));
    };
    let view = view?;
    assert_eq!(&view.id, id);
    Ok(Loaded {
        row_id,
        project_id,
        view,
    })
}

fn stored_run(key: &ProjectKey, row: &Row<'_>) -> Result<RunView, StoreError> {
    let corrupt = |what: &str| StoreError::Corrupt(format!("a stored run has a bad {what}"));
    let plan: Option<u32> = row.get(2)?;
    let state: String = row.get(5)?;
    let verdict: Option<String> = row.get(6)?;
    let integrity: Option<String> = row.get(10)?;
    let recorded_by: String = row.get(13)?;
    let view = RunView {
        id: run_id(key, row.get(1)?).map_err(|_| corrupt("number"))?,
        plan: match plan {
            Some(number) => Some(
                TestId::new(key.clone(), TestObject::Plan, number).map_err(|_| corrupt("plan"))?,
            ),
            None => None,
        },
        batch: row.get(3)?,
        title: row.get(4)?,
        state: RunState::parse(&state).ok_or_else(|| corrupt("state"))?,
        verdict: match verdict {
            Some(text) => Some(Verdict::parse(&text).ok_or_else(|| corrupt("verdict"))?),
            None => None,
        },
        reason: row.get(7)?,
        source: SourceRef::parse(&row.get::<_, String>(8)?).map_err(|_| corrupt("source"))?,
        policy: PolicyRef::parse(&row.get::<_, String>(9)?).map_err(|_| corrupt("policy"))?,
        integrity: match integrity {
            Some(text) => Some(IntegrityRef::parse(&text).map_err(|_| corrupt("integrity"))?),
            None => None,
        },
        started_ms: row.get(11)?,
        finished_ms: row.get(12)?,
        recorded_by: Actor::new(&recorded_by).map_err(|_| corrupt("writer"))?,
        totals: Totals {
            total: row.get(14)?,
            passed: row.get(15)?,
            failed: row.get(16)?,
            skipped: row.get(17)?,
            errored: row.get(18)?,
        },
        version: row.get(19)?,
    };
    if !state_fits(&view) {
        return Err(corrupt("state, verdict, end time or count"));
    }
    Ok(view)
}

fn state_fits(view: &RunView) -> bool {
    let ended = match view.state {
        RunState::Running => view.verdict.is_none() && view.finished_ms.is_none(),
        RunState::Finished => view.verdict.is_some() && view.finished_ms.is_some(),
        RunState::Interrupted => {
            view.verdict.is_none() && view.finished_ms.is_some() && view.reason.is_some()
        }
    };
    let totals = &view.totals;
    let counted = u64::from(totals.passed)
        + u64::from(totals.failed)
        + u64::from(totals.skipped)
        + u64::from(totals.errored)
        <= u64::from(totals.total);
    ended && counted
}

/// Binds a row count or bound as an SQLite integer, which `rusqlite` takes as `i64` and not
/// as `usize`. Input: the count. Output: the integer, or `store_failed` when it does not fit.
pub(crate) fn sql_count(count: usize) -> Result<i64, StoreError> {
    i64::try_from(count).map_err(|_| {
        store::refusal(
            core_codes::STORE_FAILED,
            format!("the count {count} does not fit an SQLite integer"),
        )
    })
}

fn results_from(
    tx: &Transaction<'_>,
    run: &Loaded,
    after: u32,
    limit: usize,
) -> Result<Vec<ResultView>, StoreError> {
    let key = run.view.id.key();
    let mut statement = tx.prepare(RESULTS_FROM)?;
    let rows = statement
        .query_map((run.row_id, after, sql_count(limit)?), |row| {
            Ok(stored_result(key, row))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let results: Vec<ResultView> = rows.into_iter().collect::<Result<_, _>>()?;
    assert!(results.len() <= limit);
    assert!(results.iter().all(|result| result.position > after));
    Ok(results)
}

fn stored_result(key: &ProjectKey, row: &Row<'_>) -> Result<ResultView, StoreError> {
    let corrupt = |what: &str| StoreError::Corrupt(format!("a stored result has a bad {what}"));
    let outcome: String = row.get(2)?;
    let case: Option<u32> = row.get(3)?;
    Ok(ResultView {
        position: row.get(0)?,
        name: row.get(1)?,
        outcome: ResultOutcome::parse(&outcome).ok_or_else(|| corrupt("outcome"))?,
        case: match case {
            Some(number) => Some(
                TestId::new(key.clone(), TestObject::Case, number).map_err(|_| corrupt("case"))?,
            ),
            None => None,
        },
        duration_ms: row.get(4)?,
        message: row.get(5)?,
        artifact: row.get(6)?,
    })
}

fn list_row(key: &ProjectKey, row: &Row<'_>) -> rusqlite::Result<RunRow> {
    let bad = |column: usize| {
        rusqlite::Error::InvalidColumnType(column, "run".to_owned(), rusqlite::types::Type::Text)
    };
    let number: u32 = row.get(0)?;
    let state: String = row.get(2)?;
    let verdict: Option<String> = row.get(3)?;
    let (title, elided) = cut(row.get(1)?, LIST_TITLE_BYTES_MAX);
    Ok(RunRow {
        id: TestId::new(key.clone(), TestObject::Run, number).map_err(|_| bad(0))?,
        title,
        elided,
        state: RunState::parse(&state).ok_or_else(|| bad(2))?,
        verdict: match verdict {
            Some(text) => Some(Verdict::parse(&text).ok_or_else(|| bad(3))?),
            None => None,
        },
        started_ms: row.get(4)?,
        finished_ms: row.get(5)?,
        totals: Totals {
            total: row.get(6)?,
            passed: row.get(7)?,
            failed: row.get(8)?,
            skipped: row.get(9)?,
            errored: row.get(10)?,
        },
    })
}

/// Cuts a text to at most `bytes_max` bytes on a character boundary. Inputs: the text and
/// the bound. Output: the text and whether it was cut.
pub(crate) fn cut(mut text: String, bytes_max: usize) -> (String, bool) {
    if text.len() <= bytes_max {
        return (text, false);
    }
    let mut end = bytes_max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    assert!(text.len() <= bytes_max);
    (text, true)
}

fn event(
    tx: &Transaction<'_>,
    project_id: i64,
    actor: &Actor,
    op: &'static str,
    before: Option<String>,
    after: Option<String>,
    now_ms: i64,
) -> Result<(), StoreError> {
    assert!(before.is_some() || after.is_some());
    assert!(OPS.contains(&op));
    let written = tx.prepare(EVENT_INSERT)?.execute((
        project_id,
        now_ms,
        actor.as_str(),
        op,
        before,
        after,
    ))?;
    assert_eq!(written, 1);
    Ok(())
}

fn json<T: Serialize>(value: &T) -> String {
    let text = serde_json::to_string(value).expect("a run shape always serializes");
    assert!(!text.is_empty());
    text
}

fn refused(code: &'static str, message: impl Into<String>) -> StoreError {
    StoreError::Refused(Refusal::new(code, message))
}
