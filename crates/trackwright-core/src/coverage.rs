use crate::model::{CriterionState, ProjectKey, ShortId, TICKETS_PER_PROJECT_MAX};
use crate::runs::{PAGE_BYTES_MAX, ResultOutcome, RunView, TestId, Verdict};
use crate::tests_model::{CriterionRef, LIST_ROWS_MAX, LIST_TITLE_BYTES_MAX};
use crate::text::CRITERION_BYTES_MAX;
use serde::{Deserialize, Serialize};

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

/// What a finished run left. Fields: the run and the open tickets it re-evaluated, in
/// creation order; a ticket it closed under `close = evidence` is among them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finished {
    pub run: RunView,
    pub tickets: Vec<ShortId>,
}
