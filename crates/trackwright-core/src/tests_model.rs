use crate::model::{Actor, ProjectKey, Refusal, ShortId, named_enum};
use serde::{Deserialize, Serialize};
use std::fmt;

/// The refusal codes of the test model, declared once in `model::codes` so every core refusal
/// is checked against one list.
pub mod codes {
    pub use crate::model::codes::{
        ACTIVE_PLAN_EXISTS, CASE_LINK_EXISTS, CASE_LINK_UNKNOWN, CASE_LINKS_FULL, CASE_PROTECTED,
        CASE_UNKNOWN, CASES_FULL, CROSS_PROJECT, CYCLE_UNKNOWN, OPTION_NEEDS_HOST, PLAN_CLOSED,
        PLAN_ITEM_EXISTS, PLAN_ITEM_UNKNOWN, PLAN_ITEMS_FULL, PLAN_UNKNOWN, PLANS_FULL,
        RELEASE_UNKNOWN, RUN_UNKNOWN, SET_CASE_EXISTS, SET_CASE_UNKNOWN, SET_CASES_FULL,
        SET_UNKNOWN, SETS_FULL, STABLE_NAME_TAKEN, STEPS_FULL, TEST_ID_INVALID,
    };
}

pub const CASE_TITLE_BYTES_MAX: usize = 200;
pub const STABLE_NAME_BYTES_MAX: usize = 512;
pub const STEPS_PER_CASE_MAX: usize = 64;
pub const STEP_BYTES_MAX: usize = 1024;
pub const STEP_HEADING_BYTES_MAX: usize = 80;
pub const CRITERIA_PER_CASE_MAX: usize = 64;
pub const CASES_PER_PROJECT_MAX: usize = 50_000;
pub const SET_NAME_BYTES_MAX: usize = 200;
pub const SETS_PER_PROJECT_MAX: usize = 1_000;
pub const CASES_PER_SET_MAX: usize = 5_000;
pub const PLAN_NAME_BYTES_MAX: usize = 200;
pub const PLANS_PER_PROJECT_MAX: usize = 1_000;
pub const ITEMS_PER_PLAN_MAX: usize = 1_000;
pub const LIST_ROWS_MAX: usize = 50;
pub const LIST_TITLE_BYTES_MAX: usize = 80;
pub const RUN_TITLE_BYTES_MAX: usize = 200;
pub const RUN_BATCH_BYTES_MAX: usize = 64;
pub const RUN_REASON_BYTES_MAX: usize = 200;
pub const RESULTS_PER_RUN_MAX: usize = 20_000;
pub const RESULT_ROWS_PER_RECORD_MAX: usize = 500;
pub const RESULT_NAME_BYTES_MAX: usize = 512;
pub const RESULT_MESSAGE_BYTES_MAX: usize = 2 * 1024;
pub const RESULT_ARTIFACT_BYTES_MAX: usize = 512;
pub const RUN_SHOW_PAGE_ROWS: usize = 50;
pub const SOURCE_REPOSITORY_BYTES_MAX: usize = 200;
pub const SOURCE_DECLARED_BYTES_MAX: usize = 512;
pub const POLICY_JSON_BYTES_MAX: usize = 4 * 1024;
pub const POLICY_ARGV_ITEMS_MAX: usize = 32;
pub const POLICY_ARGV_ITEM_BYTES_MAX: usize = 512;
pub const TOOLCHAIN_VERSION_BYTES_MAX: usize = 200;
pub const INTEGRITY_JSON_BYTES_MAX: usize = 8 * 1024;
pub const INTEGRITY_LIST_ITEMS_MAX: usize = 50;
pub const JUNIT_BYTES_MAX: usize = 8 * 1024 * 1024;
pub const JUNIT_DEPTH_MAX: usize = 32;
pub const ALLURE_RESULT_BYTES_MAX: usize = 256 * 1024;
pub const LIBTEST_BYTES_MAX: usize = 16 * 1024 * 1024;
pub const RUNS_RETAINED_PER_PROJECT_MAX: usize = 100_000;
pub const RUN_RETAINED_DAYS_MAX: u32 = 90;

/// The actor whose view of a protected case is never narrowed.
pub const OWNER_ACTOR: &str = "owner";

const _: () = assert!(STEP_HEADING_BYTES_MAX < STEP_BYTES_MAX);
const _: () = assert!(LIST_TITLE_BYTES_MAX < CASE_TITLE_BYTES_MAX);
const _: () = assert!(RESULT_ROWS_PER_RECORD_MAX <= RESULTS_PER_RUN_MAX);
const _: () = assert!(RUN_SHOW_PAGE_ROWS <= LIST_ROWS_MAX);
const _: () = assert!(CASES_PER_SET_MAX <= CASES_PER_PROJECT_MAX);

named_enum! {
    /// Which object a test short id names, by its wire name.
    TestObject { Case => "case", Set => "set", Plan => "plan", Run => "run" }
}

impl TestObject {
    /// The letter between the project key and the number: T, S, P or R.
    pub fn letter(self) -> char {
        match self {
            TestObject::Case => 'T',
            TestObject::Set => 'S',
            TestObject::Plan => 'P',
            TestObject::Run => 'R',
        }
    }

    fn of_letter(letter: u8) -> Option<Self> {
        TestObject::ALL
            .iter()
            .copied()
            .find(|object| object.letter() as u32 == u32::from(letter))
    }
}

named_enum! {
    /// What a case checks, from a unit test to a manual scenario.
    CaseKind {
        Unit => "unit",
        Integration => "integration",
        E2e => "e2e",
        Smoke => "smoke",
        Scenario => "scenario",
        Manual => "manual",
    }
}

named_enum! {
    /// Where a case is in its life. `deprecated` cases stay for history and are never deleted.
    CaseStatus { Draft => "draft", Ready => "ready", Deprecated => "deprecated" }
}

named_enum! {
    /// Where a plan is in its life. A closed plan takes no new items.
    PlanStatus { Draft => "draft", Active => "active", Closed => "closed" }
}

named_enum! {
    /// Where a run is: still recording, finished with a verdict, or interrupted.
    RunState { Running => "running", Finished => "finished", Interrupted => "interrupted" }
}

named_enum! {
    /// A run's verdict under the locked verdict rules (VT-2). Only `passed` satisfies anything.
    Verdict {
        Passed => "passed",
        Failed => "failed",
        NoTests => "no_tests",
        BuildFailed => "build_failed",
        Unknown => "unknown",
        Unavailable => "unavailable",
    }
}

named_enum! {
    /// One result row's outcome. `passed` and `failed` count as executed.
    ResultOutcome {
        Passed => "passed",
        Failed => "failed",
        Skipped => "skipped",
        Errored => "errored",
        NotRun => "not_run",
    }
}

impl ResultOutcome {
    /// Tells whether the test ran to a pass or a fail. Output: true for passed and failed.
    pub fn executed(self) -> bool {
        matches!(self, ResultOutcome::Passed | ResultOutcome::Failed)
    }
}

named_enum! {
    /// Policy `test_evidence`: which result proves a linked case for a ticket's `test` check.
    TestEvidence { Latest => "latest", CurrentSource => "current_source", Plan => "plan" }
}

named_enum! {
    /// Policy `skipped_counts`: whether a skipped linked case leaves its check unsatisfied.
    SkippedCounts { Fail => "fail", Neutral => "neutral" }
}

named_enum! {
    /// Policy `case_protected_visibility`: what a non-owner actor sees of a protected case.
    CaseProtectedVisibility { Hidden => "hidden", Summary => "summary", Full => "full" }
}

named_enum! {
    /// Policy `run_writers`: who may write runs. CrazyAgent locks `engine_only`.
    RunWriters { Any => "any", EngineOnly => "engine_only" }
}

named_enum! {
    /// Policy `run_retention`: keep every run, or prune unlinked runs past the count or age bound.
    RunRetention { KeepAll => "keep_all", PruneCount => "prune_count", PruneAge => "prune_age" }
}

/// A test object's stable short id: `<KEY>-T<n>` for a case, `-S` for a set, `-P` for a
/// plan, `-R` for a run. The number counts from 1 per project and object and is never
/// reused; internal row ids never leave the core.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TestId {
    key: ProjectKey,
    object: TestObject,
    number: u32,
}

impl TestId {
    /// Builds an id. Inputs: the key, the object and the number. Output: the id, or
    /// `test_id_invalid` for number 0.
    pub fn new(key: ProjectKey, object: TestObject, number: u32) -> Result<Self, Refusal> {
        if number == 0 {
            return Err(test_id_refused());
        }
        Ok(Self {
            key,
            object,
            number,
        })
    }

    /// Reads `<KEY>-<letter><number>`, the number in decimal without a leading zero. A
    /// ticket id such as `CA-42` is never a test id. Input: the text. Output: the id, or
    /// `test_id_invalid`.
    pub fn parse(text: &str) -> Result<Self, Refusal> {
        let (key, rest) = text.split_once('-').ok_or_else(test_id_refused)?;
        let key = ProjectKey::new(key).map_err(|_| test_id_refused())?;
        let (&letter, digits) = rest.as_bytes().split_first().ok_or_else(test_id_refused)?;
        let object = TestObject::of_letter(letter).ok_or_else(test_id_refused)?;
        if digits.is_empty()
            || digits.len() > 10
            || digits[0] == b'0'
            || !digits.iter().all(u8::is_ascii_digit)
        {
            return Err(test_id_refused());
        }
        let number = rest[1..].parse::<u32>().map_err(|_| test_id_refused())?;
        let id = Self::new(key, object, number)?;
        assert_eq!(
            id.to_string(),
            text,
            "a parsed test id prints as it was read"
        );
        Ok(id)
    }

    /// Reads an id that must name one kind of object. Inputs: the text and the object.
    /// Output: the id, or `test_id_invalid` for another shape or another object.
    pub fn parse_of(text: &str, object: TestObject) -> Result<Self, Refusal> {
        let id = Self::parse(text)?;
        if id.object != object {
            return Err(Refusal::new(
                codes::TEST_ID_INVALID,
                format!("{text} is not a {object} id"),
            ));
        }
        Ok(id)
    }

    /// The project key. Output: the key.
    pub fn key(&self) -> &ProjectKey {
        &self.key
    }

    /// The object the id names. Output: the object.
    pub fn object(&self) -> TestObject {
        self.object
    }

    /// The number within its project and object. Output: the number, 1 or more.
    pub fn number(&self) -> u32 {
        self.number
    }
}

fn test_id_refused() -> Refusal {
    Refusal::new(
        codes::TEST_ID_INVALID,
        "a test id is <KEY>-T<n> for a case, -S<n> for a set, -P<n> for a plan, -R<n> for a run",
    )
}

impl TryFrom<String> for TestId {
    type Error = Refusal;

    fn try_from(text: String) -> Result<Self, Refusal> {
        Self::parse(&text)
    }
}

impl From<TestId> for String {
    fn from(id: TestId) -> String {
        id.to_string()
    }
}

impl fmt::Display for TestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}{}", self.key, self.object.letter(), self.number)
    }
}

/// One acceptance criterion on the wire: its ticket and its position there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionRef {
    pub ticket: ShortId,
    pub position: u32,
}

/// One plan item on the wire: exactly one case or one set, `{"case":"CA-T3"}` or
/// `{"set":"CA-S1"}`. Any other shape fails to parse.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanItemRef {
    Case(TestId),
    Set(TestId),
}

/// A stored case, as its row and its events hold it. `id` and `project` are internal row
/// ids that never leave the core; `steps` are stored as a JSON array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub(crate) id: i64,
    pub(crate) project: i64,
    pub short_id: TestId,
    pub title: String,
    pub kind: CaseKind,
    pub stable_name: Option<String>,
    pub steps: Vec<String>,
    pub protected: bool,
    pub status: CaseStatus,
    pub created_by: Actor,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub version: i64,
}

/// One case to criterion link row, by internal ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseCriterion {
    pub(crate) case_id: i64,
    pub(crate) criterion_id: i64,
}

/// A stored test set: a named, ordered group of cases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestSet {
    pub(crate) id: i64,
    pub(crate) project: i64,
    pub short_id: TestId,
    pub name: String,
    pub version: i64,
}

/// One case in a set, at its position (1 or more, unique within the set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetCase {
    pub(crate) set_id: i64,
    pub(crate) case_id: i64,
    pub position: u32,
}

/// A stored test plan, optionally bound to a release version and a cycle of its project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestPlan {
    pub(crate) id: i64,
    pub(crate) project: i64,
    pub short_id: TestId,
    pub name: String,
    pub(crate) version_id: Option<i64>,
    pub(crate) cycle_id: Option<i64>,
    pub status: PlanStatus,
    pub version: i64,
}

/// One plan item row at its position: exactly one of `case_id` and `set_id` is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanItem {
    pub(crate) plan_id: i64,
    pub position: u32,
    pub(crate) case_id: Option<i64>,
    pub(crate) set_id: Option<i64>,
}

/// A case's steps as one actor may see them: none, the first line of each, or in full.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shown", content = "steps", rename_all = "snake_case")]
pub enum StepsView {
    Hidden,
    Headings(Vec<String>),
    Full(Vec<String>),
}

/// What an actor sees of a case beyond its id, title and latest outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseDetail {
    pub kind: CaseKind,
    pub status: CaseStatus,
    pub stable_name: Option<String>,
    pub criteria: Vec<CriterionRef>,
    pub sets: Vec<TestId>,
    pub created_by: Actor,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub version: i64,
}

/// One case as an actor may see it. For a protected case and a non-owner actor,
/// `case_protected_visibility` decides: `hidden` leaves `steps` hidden and `detail` empty,
/// `summary` shows step headings only, `full` shows everything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseView {
    pub id: TestId,
    pub title: String,
    pub protected: bool,
    pub latest: Option<ResultOutcome>,
    pub steps: StepsView,
    pub detail: Option<CaseDetail>,
}

/// One row of `case_list`: the title elided past `LIST_TITLE_BYTES_MAX`, and kind and
/// status only where the actor may see them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseRow {
    pub id: TestId,
    pub title: String,
    pub elided: bool,
    pub protected: bool,
    pub kind: Option<CaseKind>,
    pub status: Option<CaseStatus>,
    pub latest: Option<ResultOutcome>,
}

/// One set with its cases in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetView {
    pub id: TestId,
    pub name: String,
    pub cases: Vec<TestId>,
    pub version: i64,
}

/// One plan item as shown: its position and the case or set it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItemView {
    pub position: u32,
    pub item: PlanItemRef,
}

/// One plan with its release version name, cycle number and items in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanView {
    pub id: TestId,
    pub name: String,
    pub status: PlanStatus,
    pub release: Option<String>,
    pub cycle: Option<u32>,
    pub items: Vec<PlanItemView>,
    pub version: i64,
}

/// One row of `plan_list`: the plan, its status and how many items it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRow {
    pub id: TestId,
    pub name: String,
    pub status: PlanStatus,
    pub items: u32,
}

/// Tells whether an actor sees protected cases unnarrowed. Input: the actor. Output: true
/// for the owner only.
pub fn sees_protected(actor: &Actor) -> bool {
    actor.as_str() == OWNER_ACTOR
}

/// The first line of one step, cut on a character boundary at `STEP_HEADING_BYTES_MAX`.
/// Input: the step. Output: the heading.
pub fn step_heading(step: &str) -> String {
    let line = step.lines().next().unwrap_or("").trim_end();
    let mut end = line.len().min(STEP_HEADING_BYTES_MAX);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let heading = &line[..end];
    assert!(heading.len() <= STEP_HEADING_BYTES_MAX);
    assert!(!heading.contains('\n'));
    heading.to_owned()
}

/// Narrows one case to what an actor may see under the visibility policy. Inputs: the case,
/// its detail, its latest outcome, the actor and the policy. Output: the view.
pub fn case_view(
    case: &Case,
    detail: CaseDetail,
    latest: Option<ResultOutcome>,
    actor: &Actor,
    visibility: CaseProtectedVisibility,
) -> CaseView {
    assert_eq!(
        detail.version, case.version,
        "the detail belongs to this case"
    );
    let shown = if !case.protected || sees_protected(actor) {
        CaseProtectedVisibility::Full
    } else {
        visibility
    };
    let (steps, detail) = match shown {
        CaseProtectedVisibility::Hidden => (StepsView::Hidden, None),
        CaseProtectedVisibility::Summary => (
            StepsView::Headings(case.steps.iter().map(|step| step_heading(step)).collect()),
            None,
        ),
        CaseProtectedVisibility::Full => (StepsView::Full(case.steps.clone()), Some(detail)),
    };
    assert!(detail.is_some() == (shown == CaseProtectedVisibility::Full));
    CaseView {
        id: case.short_id.clone(),
        title: case.title.clone(),
        protected: case.protected,
        latest,
        steps,
        detail,
    }
}
