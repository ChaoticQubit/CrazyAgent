use crate::criteria;
use crate::host::{Authority, CheckView, Evidence, EvidenceState, Host, Sight, TicketView};
use crate::model::{
    Actor, CHECKS_PER_CRITERION_MAX, CheckKind, CriterionState, ProjectKey, Refusal, ShortId,
    TICKETS_PER_PROJECT_MAX, codes,
};
use crate::policy;
use crate::runs::{
    self, PAGE_BYTES_MAX, ResultOutcome, RunCancel, RunFinish, RunState, RunView, SourceRef,
    TestId, TestObject, Verdict,
};
use crate::standalone::refusal_of;
use crate::store::{self, StoreError};
use crate::tests_model::{
    CriterionRef, LIST_ROWS_MAX, LIST_TITLE_BYTES_MAX, SkippedCounts, TestEvidence,
};
use crate::text::CRITERION_BYTES_MAX;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The most criteria one `coverage` page holds, before its byte budget.
pub const COVERAGE_PAGE_ROWS: usize = LIST_ROWS_MAX;
/// The most bytes of one `coverage` page's rows, measured as compact JSON.
pub const COVERAGE_PAGE_BYTES_MAX: usize = PAGE_BYTES_MAX;
/// The most cases one coverage row lists; `cases_total` still counts them all.
pub const COVERAGE_CASES_PER_ROW_MAX: usize = 10;
/// The most bytes of a case title in a coverage row, cut on a character boundary.
pub const COVERAGE_TITLE_BYTES_MAX: usize = LIST_TITLE_BYTES_MAX;
/// The most open tickets one finished run re-evaluates: every ticket of a project.
pub const RUN_TICKETS_MAX: usize = TICKETS_PER_PROJECT_MAX;

const COVERAGE_CASE_BYTES_MAX: usize = 2 * COVERAGE_TITLE_BYTES_MAX + 128;

const _: () = assert!(
    2 * CRITERION_BYTES_MAX + COVERAGE_CASES_PER_ROW_MAX * COVERAGE_CASE_BYTES_MAX
        < COVERAGE_PAGE_BYTES_MAX
);

/// What a `coverage` request covers: one ticket's criteria, or the criteria of every ticket
/// in one release version of the project. On the wire `{"ticket":"CA-12"}` or
/// `{"version":"1.4"}`; any other shape fails to parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CoverageOf {
    Ticket(ShortId),
    Version(String),
}

/// One `coverage` request. Fields: the project, what it covers, and the last criterion of
/// the previous page, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageQuery {
    pub project: ProjectKey,
    pub of: CoverageOf,
    pub after: Option<CriterionRef>,
}

/// A case's latest result in a finished run. Fields: the outcome, the run it came from and
/// that run's verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageResult {
    pub outcome: ResultOutcome,
    pub run: TestId,
    pub verdict: Verdict,
}

/// One case that covers a criterion: linked to it by `case_link`, named by one of its `test`
/// checks, or both. Fields: the case, its title (cut to 80 bytes), whether a `test` check of
/// the criterion names it (only those prove the criterion), and its latest result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageCase {
    pub case: TestId,
    pub title: String,
    pub proves: bool,
    pub latest: Option<CoverageResult>,
}

/// One criterion and the cases that cover it, by case number. Fields: the criterion, its
/// text and state, at most 10 cases, and how many cases cover it in all (0 when uncovered).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageRow {
    pub criterion: CriterionRef,
    pub text: String,
    pub state: CriterionState,
    pub cases: Vec<CoverageCase>,
    pub cases_total: u32,
}

/// One page of coverage, criteria by ticket number then position, filled to at most 50 rows
/// and 6 KiB. Fields: the rows, how many criteria the scope holds and how many of them no
/// case covers, and the cursor for the next page (None on the last).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoveragePage {
    pub rows: Vec<CoverageRow>,
    pub criteria_total: u32,
    pub uncovered_total: u32,
    pub cursor: Option<CriterionRef>,
}

/// What a finished or interrupted run left. Fields: the run and the open tickets it
/// re-evaluated, in creation order; a ticket it closed under `close = evidence` is among
/// them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finished {
    pub run: RunView,
    pub tickets: Vec<ShortId>,
}

const RUN_ROW: &str = "SELECT u.id, u.project FROM tk_runs u JOIN tk_projects p \
     ON p.id = u.project WHERE p.key = ?1 AND u.number = ?2";
const RUN_CASE_NUMBERS: &str = "SELECT DISTINCT c.number FROM tk_results r \
     JOIN tk_cases c ON c.id = r.case_id WHERE r.run_id = ?1";
const OPEN_TEST_CHECKS: &str = "SELECT t.id, k.ref FROM tk_checks k \
     JOIN tk_criteria c ON c.id = k.criterion JOIN tk_tickets t ON t.id = c.ticket \
     WHERE t.project = ?1 AND k.kind = 'test' AND t.status NOT IN ('done', 'cancelled') \
     ORDER BY t.id, k.id";
const CASE_BY_NUMBER: &str = "SELECT id, title FROM tk_cases WHERE project = ?1 AND number = ?2";
const CASE_LATEST: &str = "SELECT r.outcome, u.number, u.state, u.verdict FROM tk_results r \
     JOIN tk_runs u ON u.id = r.run_id WHERE r.case_id = ?1 \
     AND (u.state = 'finished' OR (?7 = 1 AND u.state = 'interrupted')) \
     AND (?2 = 0 OR r.outcome NOT IN ('skipped', 'not_run')) \
     AND (?3 IS NULL OR (json_extract(u.source_json, '$.kind') = ?3 \
     AND json_extract(u.source_json, ?4) = ?5)) \
     AND (?6 IS NULL OR u.plan_id = ?6) ORDER BY r.run_id DESC LIMIT 1";
const ACTIVE_PLAN: &str = "SELECT p.id FROM tk_plans p JOIN tk_tickets t \
     ON t.version_id = p.version_id WHERE t.id = ?1 AND p.status = 'active'";
const RELEASE_ID: &str = "SELECT id FROM tk_versions WHERE project = ?1 AND name = ?2";
const CRITERIA_COUNTS: &str = "SELECT count(*), coalesce(sum(\
     NOT EXISTS (SELECT 1 FROM tk_case_criteria l WHERE l.criterion_id = c.id) \
     AND NOT EXISTS (SELECT 1 FROM tk_checks k JOIN tk_cases s ON s.project = t.project \
     AND s.number = CAST(substr(k.ref, ?4) AS INTEGER) \
     WHERE k.criterion = c.id AND k.kind = 'test' AND k.ref = ?5 || '-T' || s.number)), 0) \
     FROM tk_criteria c JOIN tk_tickets t ON t.id = c.ticket \
     WHERE t.project = ?1 AND (?2 IS NULL OR t.id = ?2) AND (?3 IS NULL OR t.version_id = ?3)";
const CRITERIA_PAGE: &str = "SELECT c.id, t.number, c.position, c.text, c.state \
     FROM tk_criteria c JOIN tk_tickets t ON t.id = c.ticket \
     WHERE t.project = ?1 AND (?2 IS NULL OR t.id = ?2) AND (?3 IS NULL OR t.version_id = ?3) \
     AND (t.number, c.position) > (?4, ?5) ORDER BY t.number, c.position LIMIT ?6";
const LINKED_CASES: &str = "SELECT s.number, s.id, s.title FROM tk_case_criteria l \
     JOIN tk_cases s ON s.id = l.case_id WHERE l.criterion_id = ?1";
const TEST_REFS: &str = "SELECT ref FROM tk_checks WHERE criterion = ?1 AND kind = 'test' \
     ORDER BY id";

/// The runs a `test` check counts under `test_evidence`: every finished run, the runs of one
/// source (its JSON kind, the path of its key and the key), or the runs of one plan.
enum Counted {
    Any,
    Source {
        kind: &'static str,
        path: &'static str,
        value: String,
    },
    Plan(i64),
}

/// A case's newest counted result: its outcome, its run, and that run's verdict, None for
/// an interrupted run.
struct Newest {
    outcome: ResultOutcome,
    run: TestId,
    verdict: Option<Verdict>,
}

/// One criterion of a coverage page as stored: its row id, its ticket's number, its
/// position, text and stored state.
struct Found {
    id: i64,
    ticket: u32,
    position: u32,
    text: String,
    state: String,
}

/// Tells a host's `Evidence::check` whether a `test` check naming a case is satisfied: the
/// case's newest result in an ended run `test_evidence` counts (`skipped`, `not_run` dropped
/// under `skipped_counts = neutral`) must be `passed`, in a finished run whose verdict is
/// `passed` or `failed`. Inputs: the transaction, host, project and check. Output: the
/// state, None when the reference is no case id (the host keeps its rule), or a refusal.
pub fn test_check(
    tx: &Transaction<'_>,
    host: &(impl Evidence + ?Sized),
    project: &ProjectKey,
    check: &CheckView,
) -> Result<Option<EvidenceState>, Refusal> {
    assert_eq!(check.kind, CheckKind::Test);
    assert_eq!(
        check.ticket.key(),
        project,
        "a check belongs to its project"
    );
    let Ok(case) = TestId::parse_of(&check.reference, TestObject::Case) else {
        return Ok(None);
    };
    let state = case_state(tx, host, project, &check.ticket, &case).map_err(refusal_of)?;
    assert!(!state.detail.is_empty());
    assert!(state.detail.starts_with(&case.to_string()));
    Ok(Some(state))
}

/// Finishes a run through `runs::finish`, then re-evaluates in the same savepoint every
/// open ticket holding a `test` check on a case the run reported, so the last passing run
/// closes its ticket under `close = evidence`. The `run_finish` op and every engine use
/// this, not `runs::finish`. Inputs: the transaction, the host, the writer, the finish and
/// the time. Output: the run and the tickets re-evaluated, or an error that wrote nothing.
pub fn finish(
    tx: &Transaction<'_>,
    host: &impl Host,
    actor: &Actor,
    input: &RunFinish,
    now_ms: i64,
) -> Result<Finished, StoreError> {
    store::write(tx, || {
        let run = runs::finish(tx, actor, input, now_ms)?;
        assert_eq!(run.state, RunState::Finished);
        assert!(run.verdict.is_some());
        reevaluated(tx, host, run, now_ms)
    })
}

/// Interrupts a run through `runs::cancel`, then re-evaluates in the same savepoint every
/// open ticket holding a `test` check on a case the run reported, so a newer interrupted
/// result leaves those checks unsatisfied. The `run_cancel` op and every engine use this,
/// not `runs::cancel`. Inputs: the transaction, the host, the writer, the cancel and the
/// time. Output: the run and the tickets re-evaluated, or an error that wrote nothing.
pub fn cancel(
    tx: &Transaction<'_>,
    host: &impl Host,
    actor: &Actor,
    input: &RunCancel,
    now_ms: i64,
) -> Result<Finished, StoreError> {
    store::write(tx, || {
        let run = runs::cancel(tx, actor, input, now_ms)?;
        assert_eq!(run.state, RunState::Interrupted);
        assert!(run.verdict.is_none());
        reevaluated(tx, host, run, now_ms)
    })
}

/// Lists a ticket's or a release's criteria, each with the cases that cover it, by ticket
/// number then position, a page at a time. Inputs: the transaction and the query. Output:
/// the page, or `project_unknown`, `ticket_unknown`, `cross_project`, `release_unknown` or
/// `cursor_invalid`.
pub fn coverage(tx: &Transaction<'_>, query: &CoverageQuery) -> Result<CoveragePage, StoreError> {
    let key = &query.project;
    let project = store::project(tx, key)?;
    let (ticket, release) = scope(tx, project.id, key, &query.of)?;
    let (after_ticket, after_position) = match &query.after {
        None => (0, 0),
        Some(after) if after.ticket.key() == key => (after.ticket.number(), after.position),
        Some(_) => {
            return Err(store::refusal(
                codes::CURSOR_INVALID,
                "the cursor names a criterion of another project",
            ));
        }
    };
    let number_at = runs::sql_count(key.as_str().len() + "-T".len() + 1)?;
    let (criteria_total, uncovered_total): (u32, u32) = tx.prepare(CRITERIA_COUNTS)?.query_row(
        params![project.id, ticket, release, number_at, key.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert!(uncovered_total <= criteria_total);
    let fetched = tx
        .prepare(CRITERIA_PAGE)?
        .query_map(
            params![
                project.id,
                ticket,
                release,
                after_ticket,
                after_position,
                runs::sql_count(COVERAGE_PAGE_ROWS + 1)?
            ],
            |row| {
                Ok(Found {
                    id: row.get(0)?,
                    ticket: row.get(1)?,
                    position: row.get(2)?,
                    text: row.get(3)?,
                    state: row.get(4)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut rows = Vec::with_capacity(fetched.len());
    for found in fetched {
        rows.push(row(tx, key, project.id, found)?);
    }
    let (rows, more) = runs::fill(rows, COVERAGE_PAGE_ROWS, COVERAGE_PAGE_BYTES_MAX);
    let cursor = more
        .then(|| rows.last().map(|row| row.criterion.clone()))
        .flatten();
    if query.after.is_none() && cursor.is_none() {
        let uncovered = rows.iter().filter(|row| row.cases_total == 0).count();
        assert_eq!(rows.len(), criteria_total as usize);
        assert_eq!(uncovered, uncovered_total as usize);
    }
    Ok(CoveragePage {
        rows,
        criteria_total,
        uncovered_total,
        cursor,
    })
}

/// Lists coverage as `coverage` does, without the latest result of any case that came from
/// a run the actor reads as an outcome only. Inputs: the transaction, the host, the actor
/// and the query. Output: the page, or the host's refusal.
pub fn coverage_as(
    tx: &Transaction<'_>,
    host: &(impl Authority + ?Sized),
    actor: &Actor,
    query: &CoverageQuery,
) -> Result<CoveragePage, StoreError> {
    let mut page = coverage(tx, query)?;
    for case in page.rows.iter_mut().flat_map(|row| row.cases.iter_mut()) {
        if let Some(latest) = &case.latest
            && host.run_sight(tx, &query.project, actor, &latest.run)? == Sight::Outcome
        {
            case.latest = None;
        }
    }
    Ok(page)
}

fn case_state(
    tx: &Transaction<'_>,
    host: &(impl Evidence + ?Sized),
    project: &ProjectKey,
    ticket: &ShortId,
    case: &TestId,
) -> Result<EvidenceState, StoreError> {
    if case.key() != project {
        return Ok(unsatisfied(format!("{case} is a case of another project")));
    }
    let owner = store::project(tx, project)?;
    let Some((case_id, _)) = case_row(tx, owner.id, case.number())? else {
        return Ok(unsatisfied(format!("{case} names no case")));
    };
    let policy = policy::load(tx, project)?;
    let counted = match counted(tx, host, project, ticket, policy.test_evidence)? {
        Ok(counted) => counted,
        Err(why) => return Ok(unsatisfied(format!("{case} counts no run: {why}"))),
    };
    let neutral = policy.skipped_counts == SkippedCounts::Neutral;
    let Some(found) = newest(tx, project, case_id, neutral, &counted, true)? else {
        return Ok(unsatisfied(format!(
            "{case} has no result in a run that counts"
        )));
    };
    let Some(verdict) = found.verdict else {
        return Ok(unsatisfied(format!(
            "{case} {} in {}, which was interrupted",
            found.outcome, found.run
        )));
    };
    let satisfied = found.outcome == ResultOutcome::Passed
        && matches!(verdict, Verdict::Passed | Verdict::Failed);
    if verdict == Verdict::NoTests {
        assert_ne!(
            found.outcome,
            ResultOutcome::Passed,
            "a no_tests run executed nothing"
        );
    }
    Ok(EvidenceState {
        satisfied,
        detail: format!("{case} {} in {} ({verdict})", found.outcome, found.run),
    })
}

fn counted(
    tx: &Transaction<'_>,
    host: &(impl Evidence + ?Sized),
    project: &ProjectKey,
    ticket: &ShortId,
    option: TestEvidence,
) -> Result<Result<Counted, String>, StoreError> {
    match option {
        TestEvidence::Latest => Ok(Ok(Counted::Any)),
        TestEvidence::CurrentSource => {
            let view = TicketView::of(&store::ticket(tx, ticket)?);
            let source = host
                .current_source(tx, project, &view)
                .map_err(StoreError::Refused)?;
            let Some(source) = source else {
                return Ok(Err(format!(
                    "the host knows no current source for {ticket}"
                )));
            };
            source.check().map_err(StoreError::Refused)?;
            Ok(Ok(match source {
                SourceRef::Git { tree, .. } => Counted::Source {
                    kind: "git",
                    path: "$.tree",
                    value: tree,
                },
                SourceRef::Declared { reference } => Counted::Source {
                    kind: "declared",
                    path: "$.ref",
                    value: reference,
                },
            }))
        }
        TestEvidence::Plan => {
            let row = store::ticket(tx, ticket)?.id;
            let plan: Option<i64> = tx
                .prepare(ACTIVE_PLAN)?
                .query_row([row], |found| found.get(0))
                .optional()?;
            Ok(plan
                .map(Counted::Plan)
                .ok_or_else(|| format!("{ticket} has no release with an active plan")))
        }
    }
}

fn newest(
    tx: &Transaction<'_>,
    key: &ProjectKey,
    case_id: i64,
    neutral: bool,
    counted: &Counted,
    interrupted: bool,
) -> Result<Option<Newest>, StoreError> {
    let (kind, path, value, plan) = match counted {
        Counted::Any => (None, None, None, None),
        Counted::Source { kind, path, value } => {
            (Some(*kind), Some(*path), Some(value.as_str()), None)
        }
        Counted::Plan(id) => (None, None, None, Some(*id)),
    };
    let found: Option<(String, u32, String, Option<String>)> = tx
        .prepare(CASE_LATEST)?
        .query_row(
            params![case_id, neutral, kind, path, value, plan, interrupted],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((outcome, number, state, verdict)) = found else {
        return Ok(None);
    };
    let outcome = ResultOutcome::parse(&outcome)
        .ok_or_else(|| StoreError::Corrupt("a stored result outcome is unknown".into()))?;
    let verdict = match RunState::parse(&state) {
        Some(RunState::Finished) => Some(
            verdict
                .as_deref()
                .and_then(Verdict::parse)
                .ok_or_else(|| StoreError::Corrupt("a finished run has no known verdict".into()))?,
        ),
        Some(RunState::Interrupted) => None,
        Some(RunState::Running) => unreachable!("a running run is never counted"),
        None => return Err(StoreError::Corrupt("a stored run state is unknown".into())),
    };
    assert!(interrupted || verdict.is_some());
    assert!(!neutral || !matches!(outcome, ResultOutcome::Skipped | ResultOutcome::NotRun));
    let run = TestId::new(key.clone(), TestObject::Run, number)?;
    Ok(Some(Newest {
        outcome,
        run,
        verdict,
    }))
}

fn reevaluated(
    tx: &Transaction<'_>,
    host: &impl Host,
    run: RunView,
    now_ms: i64,
) -> Result<Finished, StoreError> {
    assert_ne!(run.state, RunState::Running);
    let tested = tested(tx, &run.id)?;
    let change = format!("run {} {}", run.id, run.state);
    let tickets = criteria::reevaluate(tx, host, &tested, &change, now_ms)?;
    assert_eq!(tickets.len(), tested.len());
    Ok(Finished { run, tickets })
}

fn tested(tx: &Transaction<'_>, run: &TestId) -> Result<Vec<i64>, StoreError> {
    assert_eq!(run.object(), TestObject::Run);
    let (row, project): (i64, i64) = tx
        .prepare(RUN_ROW)?
        .query_row(params![run.key().as_str(), run.number()], |found| {
            Ok((found.get(0)?, found.get(1)?))
        })?;
    let numbers: BTreeSet<u32> = tx
        .prepare(RUN_CASE_NUMBERS)?
        .query_map([row], |found| found.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if numbers.is_empty() {
        return Ok(Vec::new());
    }
    let checks: Vec<(i64, String)> = tx
        .prepare(OPEN_TEST_CHECKS)?
        .query_map([project], |found| Ok((found.get(0)?, found.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut tickets: Vec<i64> = checks
        .into_iter()
        .filter(|(_, reference)| {
            TestId::parse_of(reference, TestObject::Case)
                .is_ok_and(|case| case.key() == run.key() && numbers.contains(&case.number()))
        })
        .map(|(ticket, _)| ticket)
        .collect();
    tickets.dedup();
    assert!(tickets.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(tickets.len() <= RUN_TICKETS_MAX);
    Ok(tickets)
}

fn scope(
    tx: &Transaction<'_>,
    project: i64,
    key: &ProjectKey,
    of: &CoverageOf,
) -> Result<(Option<i64>, Option<i64>), StoreError> {
    match of {
        CoverageOf::Ticket(id) => {
            if id.key() != key {
                return Err(store::refusal(
                    codes::CROSS_PROJECT,
                    format!("{id} is not a ticket of {key}"),
                ));
            }
            Ok((Some(store::ticket(tx, id)?.id), None))
        }
        CoverageOf::Version(name) => {
            let found: Option<i64> = tx
                .prepare(RELEASE_ID)?
                .query_row(params![project, name], |row| row.get(0))
                .optional()?;
            let release = found.ok_or_else(|| {
                store::refusal(codes::RELEASE_UNKNOWN, format!("{key} has no such release"))
            })?;
            Ok((None, Some(release)))
        }
    }
}

fn row(
    tx: &Transaction<'_>,
    key: &ProjectKey,
    project: i64,
    found: Found,
) -> Result<CoverageRow, StoreError> {
    let state = CriterionState::parse(&found.state)
        .ok_or_else(|| StoreError::Corrupt("a stored criterion state is unknown".into()))?;
    let mut cases: BTreeMap<u32, (i64, String, bool)> = BTreeMap::new();
    let linked = tx
        .prepare(LINKED_CASES)?
        .query_map([found.id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<(u32, i64, String)>>>()?;
    for (number, id, title) in linked {
        cases.insert(number, (id, title, false));
    }
    let references = tx
        .prepare(TEST_REFS)?
        .query_map([found.id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    assert!(references.len() <= CHECKS_PER_CRITERION_MAX);
    for reference in references {
        let Ok(case) = TestId::parse_of(&reference, TestObject::Case) else {
            continue;
        };
        if case.key() != key {
            continue;
        }
        if let Some(entry) = cases.get_mut(&case.number()) {
            entry.2 = true;
        } else if let Some((id, title)) = case_row(tx, project, case.number())? {
            cases.insert(case.number(), (id, title, true));
        }
    }
    let cases_total = u32::try_from(cases.len()).expect("a criterion's cases fit in u32");
    let mut shown = Vec::with_capacity(cases.len().min(COVERAGE_CASES_PER_ROW_MAX));
    for (number, (id, title, proves)) in cases.into_iter().take(COVERAGE_CASES_PER_ROW_MAX) {
        shown.push(CoverageCase {
            case: TestId::new(key.clone(), TestObject::Case, number)?,
            title: runs::cut(title, COVERAGE_TITLE_BYTES_MAX).0,
            proves,
            latest: newest(tx, key, id, false, &Counted::Any, false)?.map(|found| CoverageResult {
                outcome: found.outcome,
                run: found.run,
                verdict: found.verdict.expect("a finished run has a verdict"),
            }),
        });
    }
    assert!(shown.len() <= COVERAGE_CASES_PER_ROW_MAX);
    assert!(shown.len() <= cases_total as usize);
    Ok(CoverageRow {
        criterion: CriterionRef {
            ticket: ShortId::new(key.clone(), found.ticket)?,
            position: found.position,
        },
        text: found.text,
        state,
        cases: shown,
        cases_total,
    })
}

fn case_row(
    tx: &Transaction<'_>,
    project: i64,
    number: u32,
) -> Result<Option<(i64, String)>, StoreError> {
    Ok(tx
        .prepare(CASE_BY_NUMBER)?
        .query_row(params![project, number], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?)
}

fn unsatisfied(detail: String) -> EvidenceState {
    assert!(!detail.is_empty());
    EvidenceState {
        satisfied: false,
        detail,
    }
}
