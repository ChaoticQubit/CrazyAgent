use crate::events::{self, Recorded};
use crate::host::{Authority, Sight};
use crate::model::{Actor, ProjectKey, Refusal, ShortId, codes as core_codes};
use crate::standalone::Standalone;
use crate::store::{self, StoreError};
use crate::tests_model::{
    CASE_TITLE_BYTES_MAX, CASES_PER_PROJECT_MAX, CASES_PER_SET_MAX, CRITERIA_PER_CASE_MAX, Case,
    CaseCriterion, CaseDetail, CaseKind, CaseProtectedVisibility, CaseRow, CaseStatus, CaseView,
    CriterionRef, ITEMS_PER_PLAN_MAX, LIST_ROWS_MAX, LIST_TITLE_BYTES_MAX, PLAN_NAME_BYTES_MAX,
    PLANS_PER_PROJECT_MAX, PlanItem, PlanItemRef, PlanItemView, PlanRow, PlanStatus, PlanView,
    ResultOutcome, SET_NAME_BYTES_MAX, SETS_PER_PROJECT_MAX, STABLE_NAME_BYTES_MAX, STEP_BYTES_MAX,
    STEPS_PER_CASE_MAX, SetCase, SetView, TestId, TestObject, TestPlan, TestSet, case_view, codes,
    sees_protected,
};
use crate::text;
use rusqlite::{OptionalExtension, Params, Row, Statement, Transaction, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const CASE_CREATE: &str = "case.create";
pub const CASE_EDIT: &str = "case.edit";
pub const CASE_LINK: &str = "case.link";
pub const CASE_UNLINK: &str = "case.unlink";
pub const SET_UPSERT: &str = "set.upsert";
pub const SET_ADD: &str = "set.add";
pub const SET_REMOVE: &str = "set.remove";
pub const PLAN_UPSERT: &str = "plan.upsert";
pub const PLAN_ADD: &str = "plan.add";
pub const PLAN_REMOVE: &str = "plan.remove";

/// Every event op the test model writes and replays. The ticket replay skips these.
pub const OPS: &[&str] = &[
    CASE_CREATE,
    CASE_EDIT,
    CASE_LINK,
    CASE_UNLINK,
    SET_UPSERT,
    SET_ADD,
    SET_REMOVE,
    PLAN_UPSERT,
    PLAN_ADD,
    PLAN_REMOVE,
];

/// Migration step 2: the test management tables beside the step 1 tables, and `tk_policy`
/// rebuilt with every step 1 row and column so it also takes the five V3b policy keys
/// (SQLite cannot widen a CHECK in place). Results are never updated; retention (VT-2) may
/// delete the results of a pruned run.
pub(crate) const SCHEMA_STEP_TWO: &str = "
CREATE TABLE tk_cases(
    id INTEGER PRIMARY KEY,
    project INTEGER NOT NULL REFERENCES tk_projects(id),
    number INTEGER NOT NULL CHECK (number >= 1),
    title TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN
        ('unit', 'integration', 'e2e', 'smoke', 'scenario', 'manual')),
    stable_name TEXT,
    steps_json TEXT NOT NULL,
    protected INTEGER NOT NULL CHECK (protected IN (0, 1)),
    status TEXT NOT NULL CHECK (status IN ('draft', 'ready', 'deprecated')),
    created_by TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL,
    version INTEGER NOT NULL CHECK (version >= 1),
    UNIQUE (project, number)
);
CREATE UNIQUE INDEX tk_cases_stable_name ON tk_cases(project, stable_name)
    WHERE stable_name IS NOT NULL;
CREATE TABLE tk_case_criteria(
    case_id INTEGER NOT NULL REFERENCES tk_cases(id),
    criterion_id INTEGER NOT NULL REFERENCES tk_criteria(id),
    PRIMARY KEY (case_id, criterion_id)
) WITHOUT ROWID;
CREATE INDEX tk_case_criteria_criterion ON tk_case_criteria(criterion_id, case_id);
CREATE TABLE tk_sets(
    id INTEGER PRIMARY KEY,
    project INTEGER NOT NULL REFERENCES tk_projects(id),
    number INTEGER NOT NULL CHECK (number >= 1),
    name TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version >= 1),
    UNIQUE (project, number)
);
CREATE TABLE tk_set_cases(
    set_id INTEGER NOT NULL REFERENCES tk_sets(id),
    case_id INTEGER NOT NULL REFERENCES tk_cases(id),
    position INTEGER NOT NULL CHECK (position >= 1),
    PRIMARY KEY (set_id, case_id),
    UNIQUE (set_id, position)
) WITHOUT ROWID;
CREATE INDEX tk_set_cases_case ON tk_set_cases(case_id, set_id);
CREATE TABLE tk_plans(
    id INTEGER PRIMARY KEY,
    project INTEGER NOT NULL REFERENCES tk_projects(id),
    number INTEGER NOT NULL CHECK (number >= 1),
    name TEXT NOT NULL,
    version_id INTEGER REFERENCES tk_versions(id),
    cycle_id INTEGER REFERENCES tk_cycles(id),
    status TEXT NOT NULL CHECK (status IN ('draft', 'active', 'closed')),
    version INTEGER NOT NULL CHECK (version >= 1),
    UNIQUE (project, number)
);
CREATE UNIQUE INDEX tk_plans_active_release ON tk_plans(project, version_id)
    WHERE status = 'active' AND version_id IS NOT NULL;
CREATE TABLE tk_plan_items(
    plan_id INTEGER NOT NULL REFERENCES tk_plans(id),
    position INTEGER NOT NULL CHECK (position >= 1),
    case_id INTEGER REFERENCES tk_cases(id),
    set_id INTEGER REFERENCES tk_sets(id),
    CHECK ((case_id IS NULL) <> (set_id IS NULL)),
    PRIMARY KEY (plan_id, position)
) WITHOUT ROWID;
CREATE UNIQUE INDEX tk_plan_items_case ON tk_plan_items(plan_id, case_id)
    WHERE case_id IS NOT NULL;
CREATE UNIQUE INDEX tk_plan_items_set ON tk_plan_items(plan_id, set_id)
    WHERE set_id IS NOT NULL;
CREATE TABLE tk_runs(
    id INTEGER PRIMARY KEY,
    project INTEGER NOT NULL REFERENCES tk_projects(id),
    number INTEGER NOT NULL CHECK (number >= 1),
    plan_id INTEGER REFERENCES tk_plans(id),
    batch TEXT,
    title TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('running', 'finished', 'interrupted')),
    verdict TEXT CHECK (verdict IN
        ('passed', 'failed', 'no_tests', 'build_failed', 'unknown', 'unavailable')),
    reason TEXT,
    source_json TEXT NOT NULL,
    policy_json TEXT NOT NULL,
    integrity_json TEXT,
    started_ms INTEGER NOT NULL,
    finished_ms INTEGER,
    recorded_by TEXT NOT NULL,
    total INTEGER NOT NULL CHECK (total >= 0),
    passed INTEGER NOT NULL CHECK (passed >= 0),
    failed INTEGER NOT NULL CHECK (failed >= 0),
    skipped INTEGER NOT NULL CHECK (skipped >= 0),
    errored INTEGER NOT NULL CHECK (errored >= 0),
    version INTEGER NOT NULL CHECK (version >= 1),
    UNIQUE (project, number),
    CHECK ((state = 'running') = (finished_ms IS NULL)),
    CHECK (state <> 'running' OR verdict IS NULL)
);
CREATE INDEX tk_runs_batch ON tk_runs(project, batch) WHERE batch IS NOT NULL;
CREATE INDEX tk_runs_plan ON tk_runs(plan_id) WHERE plan_id IS NOT NULL;
CREATE TABLE tk_results(
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES tk_runs(id),
    position INTEGER NOT NULL CHECK (position >= 1),
    case_id INTEGER REFERENCES tk_cases(id),
    name TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN
        ('passed', 'failed', 'skipped', 'errored', 'not_run')),
    duration_ms INTEGER CHECK (duration_ms >= 0),
    message TEXT,
    artifact TEXT,
    UNIQUE (run_id, name),
    UNIQUE (run_id, position)
);
CREATE INDEX tk_results_case ON tk_results(case_id, run_id) WHERE case_id IS NOT NULL;
CREATE TRIGGER tk_results_no_update BEFORE UPDATE ON tk_results
    BEGIN SELECT RAISE(ABORT, 'tk_results is append-only'); END;
CREATE TABLE tk_policy_step_two(
    project INTEGER NOT NULL REFERENCES tk_projects(id),
    key TEXT NOT NULL CHECK (key IN
        ('close', 'create', 'agent_edit', 'cycle_days', 'rollover', 'import',
         'test_evidence', 'skipped_counts', 'case_protected_visibility', 'run_writers',
         'run_retention')),
    value_json TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version >= 1),
    PRIMARY KEY (project, key)
) WITHOUT ROWID;
INSERT INTO tk_policy_step_two(project, key, value_json, version)
    SELECT project, key, value_json, version FROM tk_policy;
DROP TABLE tk_policy;
ALTER TABLE tk_policy_step_two RENAME TO tk_policy;
";

macro_rules! case_select {
    () => {
        "SELECT c.id, c.project, p.key, c.number, c.title, c.kind, c.stable_name, c.steps_json, \
         c.protected, c.status, c.created_by, c.created_ms, c.updated_ms, c.version \
         FROM tk_cases c JOIN tk_projects p ON p.id = c.project"
    };
}

macro_rules! set_select {
    () => {
        "SELECT s.id, s.project, p.key, s.number, s.name, s.version \
         FROM tk_sets s JOIN tk_projects p ON p.id = s.project"
    };
}

macro_rules! plan_select {
    () => {
        "SELECT l.id, l.project, p.key, l.number, l.name, l.version_id, l.cycle_id, l.status, \
         l.version FROM tk_plans l JOIN tk_projects p ON p.id = l.project"
    };
}

const CASE_BY_ID: &str = concat!(case_select!(), " WHERE c.id = ?1");
const CASE_BY_NUMBER: &str = concat!(case_select!(), " WHERE p.key = ?1 AND c.number = ?2");
const CASES_OF_PROJECT: &str = concat!(case_select!(), " WHERE c.project = ?1 ORDER BY c.number");
const CASE_PAGE: &str = concat!(
    case_select!(),
    " WHERE c.project = ?1 AND c.number > ?2",
    " AND (?3 IS NULL OR c.kind = ?3) AND (?4 IS NULL OR c.status = ?4)",
    " AND (?5 IS NULL OR EXISTS (SELECT 1 FROM tk_set_cases m",
    " WHERE m.case_id = c.id AND m.set_id = ?5))",
    " AND (?6 = 0 OR NOT EXISTS (SELECT 1 FROM tk_case_criteria k WHERE k.case_id = c.id))",
    " AND (?7 = 1 OR c.protected = 0",
    " OR (?3 IS NULL AND ?4 IS NULL AND ?5 IS NULL AND ?6 = 0))",
    " ORDER BY c.number LIMIT ?8"
);
const CASE_COUNT: &str = "SELECT count(*) FROM tk_cases WHERE project = ?1";
const CASE_NEXT: &str = "SELECT coalesce(max(number), 0) + 1 FROM tk_cases WHERE project = ?1";
const CASE_INSERT: &str = "INSERT INTO tk_cases(project, number, title, kind, stable_name, \
     steps_json, protected, status, created_by, created_ms, updated_ms, version) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1)";
const CASE_UPDATE: &str = "UPDATE tk_cases SET title = ?3, kind = ?4, stable_name = ?5, \
     steps_json = ?6, protected = ?7, status = ?8, updated_ms = ?9, version = version + 1 \
     WHERE id = ?1 AND version = ?2";
const CASE_TOUCH: &str =
    "UPDATE tk_cases SET updated_ms = ?3, version = version + 1 WHERE id = ?1 AND version = ?2";
const CASE_OF_STABLE_NAME: &str = "SELECT id FROM tk_cases WHERE project = ?1 AND stable_name = ?2";
const CRITERION_OF_TICKET: &str = "SELECT c.id FROM tk_criteria c JOIN tk_tickets t \
     ON t.id = c.ticket WHERE t.project = ?1 AND t.number = ?2 AND c.position = ?3";
const CASE_LINK_GET: &str =
    "SELECT count(*) FROM tk_case_criteria WHERE case_id = ?1 AND criterion_id = ?2";
const CASE_LINK_COUNT: &str = "SELECT count(*) FROM tk_case_criteria WHERE case_id = ?1";
const CASE_LINK_INSERT: &str =
    "INSERT INTO tk_case_criteria(case_id, criterion_id) VALUES (?1, ?2)";
const CASE_LINK_DELETE: &str =
    "DELETE FROM tk_case_criteria WHERE case_id = ?1 AND criterion_id = ?2";
const CRITERIA_OF_CASE: &str = "SELECT t.number, c.position FROM tk_case_criteria k \
     JOIN tk_criteria c ON c.id = k.criterion_id JOIN tk_tickets t ON t.id = c.ticket \
     WHERE k.case_id = ?1 ORDER BY t.number, c.position";
const CASE_LINKS_OF_PROJECT: &str = "SELECT k.case_id, k.criterion_id FROM tk_case_criteria k \
     JOIN tk_cases c ON c.id = k.case_id WHERE c.project = ?1 ORDER BY k.case_id, k.criterion_id";
const SETS_OF_CASE: &str = "SELECT s.number FROM tk_set_cases m JOIN tk_sets s \
     ON s.id = m.set_id WHERE m.case_id = ?1 ORDER BY s.number";
const LATEST_OUTCOME: &str = "SELECT r.outcome, u.number FROM tk_results r \
     JOIN tk_runs u ON u.id = r.run_id WHERE r.case_id = ?1 AND u.state = 'finished' \
     ORDER BY u.started_ms DESC, u.id DESC, r.position DESC LIMIT 1";
const VISIBILITY_READ: &str =
    "SELECT value_json FROM tk_policy WHERE project = ?1 AND key = 'case_protected_visibility'";

const SET_BY_ID: &str = concat!(set_select!(), " WHERE s.id = ?1");
const SET_BY_NUMBER: &str = concat!(set_select!(), " WHERE p.key = ?1 AND s.number = ?2");
const SETS_OF_PROJECT: &str = concat!(set_select!(), " WHERE s.project = ?1 ORDER BY s.number");
const SET_COUNT: &str = "SELECT count(*) FROM tk_sets WHERE project = ?1";
const SET_NEXT: &str = "SELECT coalesce(max(number), 0) + 1 FROM tk_sets WHERE project = ?1";
const SET_INSERT: &str =
    "INSERT INTO tk_sets(project, number, name, version) VALUES (?1, ?2, ?3, 1)";
const SET_UPDATE: &str =
    "UPDATE tk_sets SET name = ?3, version = version + 1 WHERE id = ?1 AND version = ?2";
const SET_TOUCH: &str = "UPDATE tk_sets SET version = version + 1 WHERE id = ?1 AND version = ?2";
const SET_MEMBER_GET: &str = "SELECT position FROM tk_set_cases WHERE set_id = ?1 AND case_id = ?2";
const SET_MEMBER_COUNT: &str = "SELECT count(*) FROM tk_set_cases WHERE set_id = ?1";
const SET_MEMBER_NEXT: &str =
    "SELECT coalesce(max(position), 0) + 1 FROM tk_set_cases WHERE set_id = ?1";
const SET_MEMBER_INSERT: &str =
    "INSERT INTO tk_set_cases(set_id, case_id, position) VALUES (?1, ?2, ?3)";
const SET_MEMBER_DELETE: &str = "DELETE FROM tk_set_cases WHERE set_id = ?1 AND case_id = ?2";
const CASES_OF_SET: &str = "SELECT c.number FROM tk_set_cases m JOIN tk_cases c \
     ON c.id = m.case_id WHERE m.set_id = ?1 ORDER BY m.position";
const SET_MEMBERS_OF_PROJECT: &str = "SELECT m.set_id, m.case_id, m.position FROM tk_set_cases m \
     JOIN tk_sets s ON s.id = m.set_id WHERE s.project = ?1 ORDER BY m.set_id, m.position";

const PLAN_BY_ID: &str = concat!(plan_select!(), " WHERE l.id = ?1");
const PLAN_BY_NUMBER: &str = concat!(plan_select!(), " WHERE p.key = ?1 AND l.number = ?2");
const PLANS_OF_PROJECT: &str = concat!(plan_select!(), " WHERE l.project = ?1 ORDER BY l.number");
const PLAN_PAGE: &str = concat!(
    plan_select!(),
    " WHERE l.project = ?1 AND l.number > ?2 AND (?3 IS NULL OR l.status = ?3)",
    " ORDER BY l.number LIMIT ?4"
);
const PLAN_COUNT: &str = "SELECT count(*) FROM tk_plans WHERE project = ?1";
const PLAN_NEXT: &str = "SELECT coalesce(max(number), 0) + 1 FROM tk_plans WHERE project = ?1";
const PLAN_INSERT: &str = "INSERT INTO tk_plans(project, number, name, version_id, cycle_id, \
     status, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)";
const PLAN_UPDATE: &str = "UPDATE tk_plans SET name = ?3, version_id = ?4, cycle_id = ?5, \
     status = ?6, version = version + 1 WHERE id = ?1 AND version = ?2";
const PLAN_TOUCH: &str = "UPDATE tk_plans SET version = version + 1 WHERE id = ?1 AND version = ?2";
const RELEASE_BY_NAME: &str = "SELECT id FROM tk_versions WHERE project = ?1 AND name = ?2";
const RELEASE_NAME: &str = "SELECT name FROM tk_versions WHERE id = ?1";
const CYCLE_BY_NUMBER: &str = "SELECT id FROM tk_cycles WHERE project = ?1 AND number = ?2";
const CYCLE_NUMBER: &str = "SELECT number FROM tk_cycles WHERE id = ?1";
const PLAN_ITEM_OF_CASE: &str =
    "SELECT position FROM tk_plan_items WHERE plan_id = ?1 AND case_id = ?2";
const PLAN_ITEM_OF_SET: &str =
    "SELECT position FROM tk_plan_items WHERE plan_id = ?1 AND set_id = ?2";
const PLAN_ITEM_COUNT: &str = "SELECT count(*) FROM tk_plan_items WHERE plan_id = ?1";
const PLAN_ITEM_NEXT: &str =
    "SELECT coalesce(max(position), 0) + 1 FROM tk_plan_items WHERE plan_id = ?1";
const PLAN_ITEM_INSERT: &str =
    "INSERT INTO tk_plan_items(plan_id, position, case_id, set_id) VALUES (?1, ?2, ?3, ?4)";
const PLAN_ITEM_DELETE: &str = "DELETE FROM tk_plan_items WHERE plan_id = ?1 AND position = ?2";
const ITEMS_OF_PLAN: &str = "SELECT i.position, c.number, s.number FROM tk_plan_items i \
     LEFT JOIN tk_cases c ON c.id = i.case_id LEFT JOIN tk_sets s ON s.id = i.set_id \
     WHERE i.plan_id = ?1 ORDER BY i.position";
const PLAN_ITEMS_OF_PROJECT: &str = "SELECT i.plan_id, i.position, i.case_id, i.set_id \
     FROM tk_plan_items i JOIN tk_plans l ON l.id = i.plan_id WHERE l.project = ?1 \
     ORDER BY i.plan_id, i.position";
const ACTIVE_PLAN_OF_RELEASE: &str = "SELECT number FROM tk_plans WHERE project = ?1 \
     AND version_id = ?2 AND status = 'active' AND id IS NOT ?3";
const PLAN_HOLDS_PROTECTED: &str = "SELECT EXISTS (SELECT 1 FROM tk_plan_items i \
     LEFT JOIN tk_set_cases m ON m.set_id = i.set_id \
     JOIN tk_cases c ON c.id = coalesce(i.case_id, m.case_id) \
     WHERE i.plan_id = ?1 AND c.protected = 1)";
const ITEM_HOLDS_PROTECTED: &str = "SELECT EXISTS (SELECT 1 FROM tk_cases c \
     WHERE c.protected = 1 AND (c.id = ?1 OR c.id IN \
     (SELECT m.case_id FROM tk_set_cases m WHERE m.set_id = ?2)))";
const TEST_EVENTS_OF_PROJECT: &str =
    "SELECT op, before_json, after_json FROM tk_events WHERE project = ?1 ORDER BY id";

const STATEMENTS: &[&str] = &[
    CASE_BY_ID,
    CASE_BY_NUMBER,
    CASES_OF_PROJECT,
    CASE_PAGE,
    CASE_COUNT,
    CASE_NEXT,
    CASE_INSERT,
    CASE_UPDATE,
    CASE_TOUCH,
    CASE_OF_STABLE_NAME,
    CRITERION_OF_TICKET,
    CASE_LINK_GET,
    CASE_LINK_COUNT,
    CASE_LINK_INSERT,
    CASE_LINK_DELETE,
    CRITERIA_OF_CASE,
    CASE_LINKS_OF_PROJECT,
    SETS_OF_CASE,
    LATEST_OUTCOME,
    VISIBILITY_READ,
    SET_BY_ID,
    SET_BY_NUMBER,
    SETS_OF_PROJECT,
    SET_COUNT,
    SET_NEXT,
    SET_INSERT,
    SET_UPDATE,
    SET_TOUCH,
    SET_MEMBER_GET,
    SET_MEMBER_COUNT,
    SET_MEMBER_NEXT,
    SET_MEMBER_INSERT,
    SET_MEMBER_DELETE,
    CASES_OF_SET,
    SET_MEMBERS_OF_PROJECT,
    PLAN_BY_ID,
    PLAN_BY_NUMBER,
    PLANS_OF_PROJECT,
    PLAN_PAGE,
    PLAN_COUNT,
    PLAN_NEXT,
    PLAN_INSERT,
    PLAN_UPDATE,
    PLAN_TOUCH,
    RELEASE_BY_NAME,
    RELEASE_NAME,
    CYCLE_BY_NUMBER,
    CYCLE_NUMBER,
    PLAN_ITEM_OF_CASE,
    PLAN_ITEM_OF_SET,
    PLAN_ITEM_COUNT,
    PLAN_ITEM_NEXT,
    PLAN_ITEM_INSERT,
    PLAN_ITEM_DELETE,
    ITEMS_OF_PLAN,
    PLAN_ITEMS_OF_PROJECT,
    ACTIVE_PLAN_OF_RELEASE,
    PLAN_HOLDS_PROTECTED,
    ITEM_HOLDS_PROTECTED,
    TEST_EVENTS_OF_PROJECT,
];

/// Every SQL statement the test model runs, so a test can prepare each one against a
/// database that holds only the core schema. Output: the statements.
pub fn statements() -> &'static [&'static str] {
    STATEMENTS
}

/// A new case's fields. The core sets its number, actor and times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCase {
    pub title: String,
    pub kind: CaseKind,
    pub stable_name: Option<String>,
    pub steps: Vec<String>,
    pub protected: bool,
    pub status: CaseStatus,
}

/// The fields an edit changes. `None` keeps a field; `stable_name: Some(None)` clears it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CaseChange {
    pub title: Option<String>,
    pub kind: Option<CaseKind>,
    pub stable_name: Option<Option<String>>,
    pub steps: Option<Vec<String>>,
    pub protected: Option<bool>,
    pub status: Option<CaseStatus>,
}

/// A plan's fields: its name, the release version (by name) and cycle (by number) of its
/// project it belongs to, and its status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFields {
    pub name: String,
    pub release: Option<String>,
    pub cycle: Option<u32>,
    pub status: PlanStatus,
}

/// `case_list` filters and cursor: by kind, status, set, cases with no criterion link, and
/// cases after a given case.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CaseFilter {
    pub kind: Option<CaseKind>,
    pub status: Option<CaseStatus>,
    pub set: Option<TestId>,
    pub unlinked: bool,
    pub after: Option<TestId>,
}

/// One project's cases, links, sets and plans, each in a fixed order: cases, sets and plans
/// by number, links by case and criterion, members and items by parent and position.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TestState {
    pub cases: Vec<Case>,
    pub case_criteria: Vec<CaseCriterion>,
    pub sets: Vec<TestSet>,
    pub set_cases: Vec<SetCase>,
    pub plans: Vec<TestPlan>,
    pub plan_items: Vec<PlanItem>,
}

/// An event payload for a membership change: the parent row, with or without the member.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Membership<P, M> {
    parent: P,
    member: Option<M>,
}

/// Creates a case and its event. A non-owner may not create a protected case. Inputs: the
/// transaction, the actor, the project key, the fields and the time. Output: the case, or
/// a text, bound, `stable_name_taken` or `case_protected` refusal.
pub fn create_case(
    tx: &Transaction<'_>,
    actor: &Actor,
    key: &ProjectKey,
    case: &NewCase,
    now_ms: i64,
) -> Result<Case, StoreError> {
    check_case(&case.title, case.stable_name.as_deref(), &case.steps)?;
    if case.protected && !sees_protected(actor) {
        return Err(store::refusal(
            codes::CASE_PROTECTED,
            "only the owner creates a protected case",
        ));
    }
    store::write(tx, || {
        let project = store::project(tx, key)?;
        if count(tx, CASE_COUNT, [project.id])? >= CASES_PER_PROJECT_MAX {
            return Err(full(codes::CASES_FULL, "cases", CASES_PER_PROJECT_MAX));
        }
        if let Some(name) = &case.stable_name {
            stable_name_free(tx, project.id, name, None)?;
        }
        let number = next(tx, CASE_NEXT, project.id)?;
        let changed = prepare(tx, CASE_INSERT)?.execute(params![
            project.id,
            number,
            case.title,
            case.kind.as_str(),
            case.stable_name,
            steps_json(&case.steps),
            case.protected,
            case.status.as_str(),
            actor.as_str(),
            now_ms,
            now_ms,
        ])?;
        assert_eq!(changed, 1);
        let created = case_by_id(tx, tx.last_insert_rowid())?;
        assert_eq!(i64::from(created.short_id.number()), number);
        assert_eq!(created.version, 1);
        record(
            tx,
            created.project,
            CASE_CREATE,
            actor,
            now_ms,
            None,
            Some(&created),
        )?;
        Ok(created)
    })
}

/// Edits a case at the version the caller read. A non-owner may not edit a protected case
/// or protect one. Inputs: the transaction, the actor, the case, its version, the change and
/// the time. Output: the case, or `stale_version`, `case_protected` or a field refusal.
pub fn edit_case(
    tx: &Transaction<'_>,
    actor: &Actor,
    id: &TestId,
    version: i64,
    change: &CaseChange,
    now_ms: i64,
) -> Result<Case, StoreError> {
    store::write(tx, || {
        let before = case(tx, id)?;
        store::current(&format!("case {id}"), before.version, version)?;
        if (before.protected || change.protected == Some(true)) && !sees_protected(actor) {
            return Err(protected_refused(id));
        }
        let after = Case {
            title: change.title.clone().unwrap_or_else(|| before.title.clone()),
            kind: change.kind.unwrap_or(before.kind),
            stable_name: change
                .stable_name
                .clone()
                .unwrap_or_else(|| before.stable_name.clone()),
            steps: change.steps.clone().unwrap_or_else(|| before.steps.clone()),
            protected: change.protected.unwrap_or(before.protected),
            status: change.status.unwrap_or(before.status),
            updated_ms: now_ms,
            version: before.version + 1,
            ..before.clone()
        };
        check_case(&after.title, after.stable_name.as_deref(), &after.steps)?;
        if let Some(name) = &after.stable_name {
            stable_name_free(tx, before.project, name, Some(before.id))?;
        }
        let changed = prepare(tx, CASE_UPDATE)?.execute(params![
            before.id,
            version,
            after.title,
            after.kind.as_str(),
            after.stable_name,
            steps_json(&after.steps),
            after.protected,
            after.status.as_str(),
            now_ms,
        ])?;
        assert_eq!(changed, 1, "the version was checked in this transaction");
        let stored = case_by_id(tx, before.id)?;
        assert_eq!(stored, after, "the stored case is the edit");
        record(
            tx,
            stored.project,
            CASE_EDIT,
            actor,
            now_ms,
            Some(&before),
            Some(&stored),
        )?;
        Ok(stored)
    })
}

/// Links a case to a criterion of a ticket in its own project. Inputs: the transaction, the
/// actor, the case, its version, the criterion and the time. Output: the case at its next
/// version, or `case_link_exists`, `case_links_full`, `cross_project` or
/// `criterion_unknown`.
pub fn link_case(
    tx: &Transaction<'_>,
    actor: &Actor,
    id: &TestId,
    version: i64,
    criterion: &CriterionRef,
    now_ms: i64,
) -> Result<Case, StoreError> {
    case_link(tx, actor, (id, version), criterion, now_ms, true)
}

/// Removes a case's link to a criterion. Inputs: as `link_case`. Output: the case at its
/// next version, or `case_link_unknown`.
pub fn unlink_case(
    tx: &Transaction<'_>,
    actor: &Actor,
    id: &TestId,
    version: i64,
    criterion: &CriterionRef,
    now_ms: i64,
) -> Result<Case, StoreError> {
    case_link(tx, actor, (id, version), criterion, now_ms, false)
}

fn case_link(
    tx: &Transaction<'_>,
    actor: &Actor,
    (id, version): (&TestId, i64),
    criterion: &CriterionRef,
    now_ms: i64,
    adding: bool,
) -> Result<Case, StoreError> {
    store::write(tx, || {
        let before = case(tx, id)?;
        store::current(&format!("case {id}"), before.version, version)?;
        if before.protected && !sees_protected(actor) {
            return Err(protected_refused(id));
        }
        let member = CaseCriterion {
            case_id: before.id,
            criterion_id: criterion_id(tx, &before, criterion)?,
        };
        let linked = count(tx, CASE_LINK_GET, [member.case_id, member.criterion_id])? == 1;
        if adding {
            if linked {
                return Err(store::refusal(
                    codes::CASE_LINK_EXISTS,
                    format!("{id} is already linked to that criterion"),
                ));
            }
            if count(tx, CASE_LINK_COUNT, [before.id])? >= CRITERIA_PER_CASE_MAX {
                return Err(full(
                    codes::CASE_LINKS_FULL,
                    "criterion links",
                    CRITERIA_PER_CASE_MAX,
                ));
            }
            prepare(tx, CASE_LINK_INSERT)?.execute([member.case_id, member.criterion_id])?;
        } else {
            if !linked {
                return Err(store::refusal(
                    codes::CASE_LINK_UNKNOWN,
                    format!("{id} is not linked to that criterion"),
                ));
            }
            prepare(tx, CASE_LINK_DELETE)?.execute([member.case_id, member.criterion_id])?;
        }
        let changed = prepare(tx, CASE_TOUCH)?.execute(params![before.id, version, now_ms])?;
        assert_eq!(changed, 1, "the version was checked in this transaction");
        let after = case_by_id(tx, before.id)?;
        assert_eq!(after.version, before.version + 1);
        let op = if adding { CASE_LINK } else { CASE_UNLINK };
        let parents = (&before, &after);
        record_member(
            tx,
            (after.project, op),
            actor,
            now_ms,
            parents,
            (member, adding),
        )?;
        Ok(after)
    })
}

/// Creates a set, or renames one at the version the caller read. Inputs: the transaction,
/// the actor, the project key, the set and its version when it exists, the name and the
/// time. Output: the set, or `sets_full`, `stale_version` or a name refusal.
pub fn upsert_set(
    tx: &Transaction<'_>,
    actor: &Actor,
    key: &ProjectKey,
    existing: Option<(&TestId, i64)>,
    name: &str,
    now_ms: i64,
) -> Result<TestSet, StoreError> {
    checked("set name", name, SET_NAME_BYTES_MAX, false)?;
    store::write(tx, || {
        let Some((id, version)) = existing else {
            let project = store::project(tx, key)?;
            if count(tx, SET_COUNT, [project.id])? >= SETS_PER_PROJECT_MAX {
                return Err(full(codes::SETS_FULL, "sets", SETS_PER_PROJECT_MAX));
            }
            let number = next(tx, SET_NEXT, project.id)?;
            prepare(tx, SET_INSERT)?.execute(params![project.id, number, name])?;
            let created = set_by_id(tx, tx.last_insert_rowid())?;
            assert_eq!(i64::from(created.short_id.number()), number);
            record(
                tx,
                created.project,
                SET_UPSERT,
                actor,
                now_ms,
                None,
                Some(&created),
            )?;
            return Ok(created);
        };
        same_key(id, key)?;
        let before = set(tx, id)?;
        store::current(&format!("set {id}"), before.version, version)?;
        let changed = prepare(tx, SET_UPDATE)?.execute(params![before.id, version, name])?;
        assert_eq!(changed, 1, "the version was checked in this transaction");
        let after = set_by_id(tx, before.id)?;
        assert_eq!(after.name, name);
        record(
            tx,
            after.project,
            SET_UPSERT,
            actor,
            now_ms,
            Some(&before),
            Some(&after),
        )?;
        Ok(after)
    })
}

/// Appends a case of the set's project to a set. Inputs: the transaction, the actor, the
/// set, its version, the case and the time. Output: the set at its next version, or
/// `set_case_exists`, `set_cases_full` or `cross_project`.
pub fn set_add(
    tx: &Transaction<'_>,
    actor: &Actor,
    id: &TestId,
    version: i64,
    case_id: &TestId,
    now_ms: i64,
) -> Result<TestSet, StoreError> {
    set_member(tx, actor, (id, version), case_id, now_ms, true)
}

/// Removes a case from a set. Inputs: as `set_add`. Output: the set at its next version, or
/// `set_case_unknown`.
pub fn set_remove(
    tx: &Transaction<'_>,
    actor: &Actor,
    id: &TestId,
    version: i64,
    case_id: &TestId,
    now_ms: i64,
) -> Result<TestSet, StoreError> {
    set_member(tx, actor, (id, version), case_id, now_ms, false)
}

fn set_member(
    tx: &Transaction<'_>,
    actor: &Actor,
    (id, version): (&TestId, i64),
    case_id: &TestId,
    now_ms: i64,
    adding: bool,
) -> Result<TestSet, StoreError> {
    store::write(tx, || {
        let before = set(tx, id)?;
        store::current(&format!("set {id}"), before.version, version)?;
        let held = case(tx, case_id)?;
        if held.project != before.project {
            return Err(cross_project(case_id, id));
        }
        if !adding && held.protected && !sees_protected(actor) {
            return Err(protected_refused(case_id));
        }
        let member = set_slot(tx, (id, &before), (case_id, &held), adding)?;
        let changed = prepare(tx, SET_TOUCH)?.execute([before.id, version])?;
        assert_eq!(changed, 1, "the version was checked in this transaction");
        let after = set_by_id(tx, before.id)?;
        assert_eq!(after.version, before.version + 1);
        let op = if adding { SET_ADD } else { SET_REMOVE };
        let parents = (&before, &after);
        record_member(
            tx,
            (after.project, op),
            actor,
            now_ms,
            parents,
            (member, adding),
        )?;
        Ok(after)
    })
}

fn set_slot(
    tx: &Transaction<'_>,
    (id, set): (&TestId, &TestSet),
    (case_id, held): (&TestId, &Case),
    adding: bool,
) -> Result<SetCase, StoreError> {
    assert_eq!(
        held.project, set.project,
        "the caller refused a case of another project"
    );
    let position: Option<u32> = prepare(tx, SET_MEMBER_GET)?
        .query_row([set.id, held.id], |row| row.get(0))
        .optional()?;
    if !adding {
        let position = position.ok_or_else(|| {
            store::refusal(codes::SET_CASE_UNKNOWN, format!("{case_id} is not in {id}"))
        })?;
        prepare(tx, SET_MEMBER_DELETE)?.execute([set.id, held.id])?;
        return Ok(SetCase {
            set_id: set.id,
            case_id: held.id,
            position,
        });
    }
    if position.is_some() {
        let message = format!("{case_id} is already in {id}");
        return Err(store::refusal(codes::SET_CASE_EXISTS, message));
    }
    if count(tx, SET_MEMBER_COUNT, [set.id])? >= CASES_PER_SET_MAX {
        return Err(full(
            codes::SET_CASES_FULL,
            "cases in a set",
            CASES_PER_SET_MAX,
        ));
    }
    let position = next(tx, SET_MEMBER_NEXT, set.id)?;
    prepare(tx, SET_MEMBER_INSERT)?.execute([set.id, held.id, position])?;
    Ok(SetCase {
        set_id: set.id,
        case_id: held.id,
        position: number_of(position),
    })
}

/// Creates a plan, or changes one at the version the caller read; a release holds at most
/// one active plan. Inputs: the transaction, the actor, the key, the plan and its version
/// when it exists, the fields and the time. Output: the plan, or `plans_full`,
/// `active_plan_exists`, `release_unknown`, `cycle_unknown`, `stale_version` or a name
/// refusal.
pub fn upsert_plan(
    tx: &Transaction<'_>,
    actor: &Actor,
    key: &ProjectKey,
    existing: Option<(&TestId, i64)>,
    fields: &PlanFields,
    now_ms: i64,
) -> Result<TestPlan, StoreError> {
    checked("plan name", &fields.name, PLAN_NAME_BYTES_MAX, false)?;
    store::write(tx, || {
        let project = store::project(tx, key)?;
        let (release, cycle) = plan_targets(tx, project.id, fields)?;
        let status = fields.status.as_str();
        let Some((id, version)) = existing else {
            if count(tx, PLAN_COUNT, [project.id])? >= PLANS_PER_PROJECT_MAX {
                return Err(full(codes::PLANS_FULL, "plans", PLANS_PER_PROJECT_MAX));
            }
            active_plan_free(tx, project.id, (release, fields.status), None)?;
            let number = next(tx, PLAN_NEXT, project.id)?;
            prepare(tx, PLAN_INSERT)?.execute(params![
                project.id,
                number,
                fields.name,
                release,
                cycle,
                status
            ])?;
            let created = plan_by_id(tx, tx.last_insert_rowid())?;
            assert_eq!(i64::from(created.short_id.number()), number);
            record(
                tx,
                created.project,
                PLAN_UPSERT,
                actor,
                now_ms,
                None,
                Some(&created),
            )?;
            return Ok(created);
        };
        same_key(id, key)?;
        change_plan(tx, actor, (id, version), fields, (release, cycle), now_ms)
    })
}

fn change_plan(
    tx: &Transaction<'_>,
    actor: &Actor,
    (id, version): (&TestId, i64),
    fields: &PlanFields,
    (release, cycle): (Option<i64>, Option<i64>),
    now_ms: i64,
) -> Result<TestPlan, StoreError> {
    let before = plan(tx, id)?;
    store::current(&format!("plan {id}"), before.version, version)?;
    active_plan_free(
        tx,
        before.project,
        (release, fields.status),
        Some(before.id),
    )?;
    let reaimed =
        before.status != fields.status || before.version_id != release || before.cycle_id != cycle;
    if reaimed && !sees_protected(actor) && holds_protected(tx, PLAN_HOLDS_PROTECTED, [before.id])?
    {
        let message = format!(
            "{id} holds a protected case; only the owner moves its status, release or cycle"
        );
        return Err(store::refusal(codes::CASE_PROTECTED, message));
    }
    let status = fields.status.as_str();
    let changed = prepare(tx, PLAN_UPDATE)?.execute(params![
        before.id,
        version,
        fields.name,
        release,
        cycle,
        status
    ])?;
    assert_eq!(changed, 1, "the version was checked in this transaction");
    let after = plan_by_id(tx, before.id)?;
    assert_eq!(after.status, fields.status);
    assert_eq!(after.version_id, release);
    record(
        tx,
        after.project,
        PLAN_UPSERT,
        actor,
        now_ms,
        Some(&before),
        Some(&after),
    )?;
    Ok(after)
}

/// Appends one case or one set of the plan's project to an open plan. Inputs: the
/// transaction, the actor, the plan, its version, the item and the time. Output: the plan
/// at its next version, or `plan_item_exists`, `plan_items_full`, `plan_closed` or
/// `cross_project`.
pub fn plan_add(
    tx: &Transaction<'_>,
    actor: &Actor,
    id: &TestId,
    version: i64,
    item: &PlanItemRef,
    now_ms: i64,
) -> Result<TestPlan, StoreError> {
    plan_member(tx, actor, (id, version), item, now_ms, true)
}

/// Removes one item from an open plan. Inputs: as `plan_add`. Output: the plan at its next
/// version, or `plan_item_unknown` or `plan_closed`.
pub fn plan_remove(
    tx: &Transaction<'_>,
    actor: &Actor,
    id: &TestId,
    version: i64,
    item: &PlanItemRef,
    now_ms: i64,
) -> Result<TestPlan, StoreError> {
    plan_member(tx, actor, (id, version), item, now_ms, false)
}

fn plan_member(
    tx: &Transaction<'_>,
    actor: &Actor,
    (id, version): (&TestId, i64),
    item: &PlanItemRef,
    now_ms: i64,
    adding: bool,
) -> Result<TestPlan, StoreError> {
    store::write(tx, || {
        let before = plan(tx, id)?;
        store::current(&format!("plan {id}"), before.version, version)?;
        if before.status == PlanStatus::Closed {
            return Err(store::refusal(
                codes::PLAN_CLOSED,
                format!("{id} is closed"),
            ));
        }
        let (case_id, set_id) = item_ids(tx, &before, item)?;
        if !adding
            && !sees_protected(actor)
            && holds_protected(tx, ITEM_HOLDS_PROTECTED, params![case_id, set_id])?
        {
            let (PlanItemRef::Case(target) | PlanItemRef::Set(target)) = item;
            let message = format!("{target} is or holds a protected case; only the owner drops it");
            return Err(store::refusal(codes::CASE_PROTECTED, message));
        }
        let position = plan_slot(tx, (id, before.id), (case_id, set_id), adding)?;
        let member = PlanItem {
            plan_id: before.id,
            position,
            case_id,
            set_id,
        };
        let changed = prepare(tx, PLAN_TOUCH)?.execute([before.id, version])?;
        assert_eq!(changed, 1, "the version was checked in this transaction");
        let after = plan_by_id(tx, before.id)?;
        assert_eq!(after.version, before.version + 1);
        let op = if adding { PLAN_ADD } else { PLAN_REMOVE };
        let parents = (&before, &after);
        record_member(
            tx,
            (after.project, op),
            actor,
            now_ms,
            parents,
            (member, adding),
        )?;
        Ok(after)
    })
}

fn plan_slot(
    tx: &Transaction<'_>,
    (id, plan_id): (&TestId, i64),
    (case_id, set_id): (Option<i64>, Option<i64>),
    adding: bool,
) -> Result<u32, StoreError> {
    assert_ne!(
        case_id.is_some(),
        set_id.is_some(),
        "a plan item names one case or one set"
    );
    let held = plan_position(tx, plan_id, (case_id, set_id))?;
    if !adding {
        let position = held.ok_or_else(|| {
            store::refusal(codes::PLAN_ITEM_UNKNOWN, format!("{id} does not hold it"))
        })?;
        prepare(tx, PLAN_ITEM_DELETE)?.execute(params![plan_id, position])?;
        return Ok(position);
    }
    if held.is_some() {
        return Err(store::refusal(
            codes::PLAN_ITEM_EXISTS,
            format!("{id} holds it"),
        ));
    }
    if count(tx, PLAN_ITEM_COUNT, [plan_id])? >= ITEMS_PER_PLAN_MAX {
        return Err(full(
            codes::PLAN_ITEMS_FULL,
            "plan items",
            ITEMS_PER_PLAN_MAX,
        ));
    }
    let position = next(tx, PLAN_ITEM_NEXT, plan_id)?;
    prepare(tx, PLAN_ITEM_INSERT)?.execute(params![plan_id, position, case_id, set_id])?;
    Ok(number_of(position))
}

fn active_plan_free(
    tx: &Transaction<'_>,
    project: i64,
    (release, status): (Option<i64>, PlanStatus),
    except: Option<i64>,
) -> Result<(), StoreError> {
    let (Some(release), PlanStatus::Active) = (release, status) else {
        return Ok(());
    };
    let holder: Option<u32> = prepare(tx, ACTIVE_PLAN_OF_RELEASE)?
        .query_row(params![project, release, except], |row| row.get(0))
        .optional()?;
    let Some(number) = holder else {
        return Ok(());
    };
    assert!(number >= 1, "plan numbers start at 1");
    let message = format!("plan {number} is already the active plan of that release");
    Err(store::refusal(codes::ACTIVE_PLAN_EXISTS, message))
}

fn holds_protected<P: Params>(
    tx: &Transaction<'_>,
    sql: &'static str,
    args: P,
) -> Result<bool, StoreError> {
    assert!(sql == PLAN_HOLDS_PROTECTED || sql == ITEM_HOLDS_PROTECTED);
    let held: i64 = prepare(tx, sql)?.query_row(args, |row| row.get(0))?;
    assert!(held == 0 || held == 1, "EXISTS reads 0 or 1");
    Ok(held == 1)
}

fn plan_position(
    tx: &Transaction<'_>,
    plan_id: i64,
    item: (Option<i64>, Option<i64>),
) -> Result<Option<u32>, StoreError> {
    let (sql, held) = match item {
        (Some(case_id), None) => (PLAN_ITEM_OF_CASE, case_id),
        (None, Some(set_id)) => (PLAN_ITEM_OF_SET, set_id),
        other => panic!("a plan item names exactly one case or set, not {other:?}"),
    };
    let position: Option<u32> = prepare(tx, sql)?
        .query_row([plan_id, held], |row| row.get(0))
        .optional()?;
    assert!(position.is_none_or(|position| position >= 1));
    Ok(position)
}

fn record_member<P: Serialize, M: Serialize>(
    tx: &Transaction<'_>,
    (project, op): (i64, &'static str),
    actor: &Actor,
    now_ms: i64,
    (before, after): (&P, &P),
    (member, adding): (M, bool),
) -> Result<(), StoreError> {
    assert!(MEMBER_OPS.contains(&op), "{op} changes a membership");
    assert_eq!(adding, [CASE_LINK, SET_ADD, PLAN_ADD].contains(&op));
    let (old, new) = if adding {
        (None, Some(member))
    } else {
        (Some(member), None)
    };
    let was = Membership {
        parent: before,
        member: old,
    };
    let now = Membership {
        parent: after,
        member: new,
    };
    record(tx, project, op, actor, now_ms, Some(&was), Some(&now))
}

const MEMBER_OPS: &[&str] = &[
    CASE_LINK,
    CASE_UNLINK,
    SET_ADD,
    SET_REMOVE,
    PLAN_ADD,
    PLAN_REMOVE,
];

/// Reads a stored case by its short id. Inputs: the transaction and the id. Output: the
/// case, or `test_id_invalid` for another object's id, or `case_unknown`.
pub fn case(tx: &Transaction<'_>, id: &TestId) -> Result<Case, StoreError> {
    of_object(id, TestObject::Case)?;
    let found = prepare(tx, CASE_BY_NUMBER)?
        .query_row(params![id.key().as_str(), id.number()], case_row)
        .optional()?
        .ok_or_else(|| store::refusal(codes::CASE_UNKNOWN, format!("case {id} does not exist")))?;
    assert_eq!(&found.short_id, id);
    assert!(found.version >= 1);
    Ok(found)
}

/// Reads a stored set by its short id. Inputs: the transaction and the id. Output: the set,
/// or `test_id_invalid` or `set_unknown`.
pub fn set(tx: &Transaction<'_>, id: &TestId) -> Result<TestSet, StoreError> {
    of_object(id, TestObject::Set)?;
    let found = prepare(tx, SET_BY_NUMBER)?
        .query_row(params![id.key().as_str(), id.number()], set_row)
        .optional()?
        .ok_or_else(|| store::refusal(codes::SET_UNKNOWN, format!("set {id} does not exist")))?;
    assert_eq!(&found.short_id, id);
    assert!(found.version >= 1);
    Ok(found)
}

/// Reads a stored plan by its short id. Inputs: the transaction and the id. Output: the
/// plan, or `test_id_invalid` or `plan_unknown`.
pub fn plan(tx: &Transaction<'_>, id: &TestId) -> Result<TestPlan, StoreError> {
    of_object(id, TestObject::Plan)?;
    let found = prepare(tx, PLAN_BY_NUMBER)?
        .query_row(params![id.key().as_str(), id.number()], plan_row)
        .optional()?
        .ok_or_else(|| store::refusal(codes::PLAN_UNKNOWN, format!("plan {id} does not exist")))?;
    assert_eq!(&found.short_id, id);
    assert!(found.version >= 1);
    Ok(found)
}

/// Finds the case a runner's stable name names in one project, for result rows (VT-2).
/// Inputs: the transaction, the internal project id and the stable name. Output: the
/// case's internal id, or None.
pub(crate) fn case_id_for(
    tx: &Transaction<'_>,
    project: i64,
    stable_name: &str,
) -> Result<Option<i64>, StoreError> {
    Ok(prepare(tx, CASE_OF_STABLE_NAME)?
        .query_row(params![project, stable_name], |row| row.get(0))
        .optional()?)
}

/// Finds the case a stable name names in a project. Inputs: the transaction, the key and the
/// stable name. Output: the case's short id, or None.
pub fn case_for(
    tx: &Transaction<'_>,
    key: &ProjectKey,
    stable_name: &str,
) -> Result<Option<TestId>, StoreError> {
    let project = store::project(tx, key)?;
    let Some(id) = case_id_for(tx, project.id, stable_name)? else {
        return Ok(None);
    };
    let found = case_by_id(tx, id)?;
    assert_eq!(found.project, project.id);
    assert_eq!(found.stable_name.as_deref(), Some(stable_name));
    Ok(Some(found.short_id))
}

/// Shows one case as the actor may see it under `case_protected_visibility`, with full run
/// sight (the standalone host's). Inputs: the transaction, the actor and the case. Output:
/// the view, or `case_unknown`.
pub fn case_show(tx: &Transaction<'_>, actor: &Actor, id: &TestId) -> Result<CaseView, StoreError> {
    case_show_as(tx, &Standalone, actor, id)
}

/// Shows one case as `case_show` does, without its latest outcome when that came from a run
/// the host's `run_sight` gives the actor as an outcome only. Inputs: the transaction, the
/// host, the actor and the case. Output: the view, `case_unknown`, or the host's refusal.
pub fn case_show_as(
    tx: &Transaction<'_>,
    host: &(impl Authority + ?Sized),
    actor: &Actor,
    id: &TestId,
) -> Result<CaseView, StoreError> {
    let found = case(tx, id)?;
    let detail = case_detail(tx, &found)?;
    let latest = latest_as(tx, host, actor, &found)?;
    let visibility = protected_visibility(tx, found.project)?;
    let view = case_view(&found, detail, latest, actor, visibility);
    assert_eq!(&view.id, id);
    assert_eq!(view.protected, found.protected);
    Ok(view)
}

/// Lists one page of a project's cases, at most `LIST_ROWS_MAX`, in number order. For an
/// actor that may not see protected cases, their kind and status stay hidden and no kind,
/// status, set or unlinked filter matches them. Run sight is full (the standalone host's).
/// Inputs: the transaction, actor, key and filter. Output: the rows, or `project_unknown`,
/// `set_unknown` or `cross_project`.
pub fn case_list(
    tx: &Transaction<'_>,
    actor: &Actor,
    key: &ProjectKey,
    filter: &CaseFilter,
) -> Result<Vec<CaseRow>, StoreError> {
    case_list_as(tx, &Standalone, actor, key, filter)
}

/// Lists cases as `case_list` does, leaving out each latest outcome that came from a run the
/// host's `run_sight` gives the actor as an outcome only. Inputs: the transaction, host,
/// actor, key and filter. Output: the rows, the refusals of `case_list`, or the host's.
pub fn case_list_as(
    tx: &Transaction<'_>,
    host: &(impl Authority + ?Sized),
    actor: &Actor,
    key: &ProjectKey,
    filter: &CaseFilter,
) -> Result<Vec<CaseRow>, StoreError> {
    let project = store::project(tx, key)?;
    let after = match &filter.after {
        None => 0,
        Some(id) => {
            of_object(id, TestObject::Case)?;
            same_key(id, key)?;
            i64::from(id.number())
        }
    };
    let set_id = match &filter.set {
        None => None,
        Some(id) => {
            same_key(id, key)?;
            Some(set(tx, id)?.id)
        }
    };
    let visibility = protected_visibility(tx, project.id)?;
    let unnarrowed = sees_protected(actor) || visibility == CaseProtectedVisibility::Full;
    let cases = prepare(tx, CASE_PAGE)?
        .query_map(
            params![
                project.id,
                after,
                filter.kind.map(CaseKind::as_str),
                filter.status.map(CaseStatus::as_str),
                set_id,
                filter.unlinked,
                unnarrowed,
                LIST_ROWS_MAX as i64,
            ],
            case_row,
        )?
        .collect::<rusqlite::Result<Vec<Case>>>()?;
    assert!(cases.len() <= LIST_ROWS_MAX);
    cases
        .iter()
        .map(|found| {
            let shown = unnarrowed || !found.protected;
            let (title, elided) = elide(&found.title);
            Ok(CaseRow {
                id: found.short_id.clone(),
                title,
                elided,
                protected: found.protected,
                kind: shown.then_some(found.kind),
                status: shown.then_some(found.status),
                latest: latest_as(tx, host, actor, found)?,
            })
        })
        .collect()
}

/// Shows one set with its cases in order. Inputs: the transaction and the set. Output: the
/// view, or `set_unknown`.
pub fn set_show(tx: &Transaction<'_>, id: &TestId) -> Result<SetView, StoreError> {
    let found = set(tx, id)?;
    let key = found.short_id.key().clone();
    let cases = prepare(tx, CASES_OF_SET)?
        .query_map([found.id], |row| {
            TestId::new(key.clone(), TestObject::Case, row.get(0)?)
                .map_err(|refused| store::conversion(0, refused))
        })?
        .collect::<rusqlite::Result<Vec<TestId>>>()?;
    assert!(cases.len() <= CASES_PER_SET_MAX);
    Ok(SetView {
        id: found.short_id,
        name: found.name,
        cases,
        version: found.version,
    })
}

/// Shows one plan with its release version name, cycle number and items in order. Inputs:
/// the transaction and the plan. Output: the view, or `plan_unknown`.
pub fn plan_show(tx: &Transaction<'_>, id: &TestId) -> Result<PlanView, StoreError> {
    let found = plan(tx, id)?;
    let key = found.short_id.key().clone();
    let items = prepare(tx, ITEMS_OF_PLAN)?
        .query_map([found.id], |row| plan_item_view(row, &key))?
        .collect::<rusqlite::Result<Vec<PlanItemView>>>()?;
    assert!(items.len() <= ITEMS_PER_PLAN_MAX);
    let release: Option<String> = match found.version_id {
        None => None,
        Some(release) => Some(prepare(tx, RELEASE_NAME)?.query_row([release], |row| row.get(0))?),
    };
    let cycle: Option<u32> = match found.cycle_id {
        None => None,
        Some(cycle) => Some(prepare(tx, CYCLE_NUMBER)?.query_row([cycle], |row| row.get(0))?),
    };
    Ok(PlanView {
        id: found.short_id,
        name: found.name,
        status: found.status,
        release,
        cycle,
        items,
        version: found.version,
    })
}

/// Lists one page of a project's plans, at most `LIST_ROWS_MAX`, in number order. Inputs:
/// the transaction, the key, an optional status and the plan to list after. Output: the
/// rows, or `project_unknown`.
pub fn plan_list(
    tx: &Transaction<'_>,
    key: &ProjectKey,
    status: Option<PlanStatus>,
    after: Option<&TestId>,
) -> Result<Vec<PlanRow>, StoreError> {
    let project = store::project(tx, key)?;
    let after = match after {
        None => 0,
        Some(id) => {
            of_object(id, TestObject::Plan)?;
            same_key(id, key)?;
            i64::from(id.number())
        }
    };
    let plans = prepare(tx, PLAN_PAGE)?
        .query_map(
            params![
                project.id,
                after,
                status.map(PlanStatus::as_str),
                LIST_ROWS_MAX as i64
            ],
            plan_row,
        )?
        .collect::<rusqlite::Result<Vec<TestPlan>>>()?;
    assert!(plans.len() <= LIST_ROWS_MAX);
    plans
        .into_iter()
        .map(|found| {
            let items = count(tx, PLAN_ITEM_COUNT, [found.id])?;
            assert!(items <= ITEMS_PER_PLAN_MAX);
            Ok(PlanRow {
                id: found.short_id,
                name: found.name,
                status: found.status,
                items: number_of(items as i64),
            })
        })
        .collect()
}

/// Reads one project's test rows from the tables. Inputs: the transaction and the key.
/// Output: the state, or `project_unknown`.
pub fn state(tx: &Transaction<'_>, key: &ProjectKey) -> Result<TestState, StoreError> {
    let project = store::project(tx, key)?;
    let state = TestState {
        cases: all(tx, CASES_OF_PROJECT, project.id, case_row)?,
        case_criteria: all(tx, CASE_LINKS_OF_PROJECT, project.id, |row| {
            Ok(CaseCriterion {
                case_id: row.get(0)?,
                criterion_id: row.get(1)?,
            })
        })?,
        sets: all(tx, SETS_OF_PROJECT, project.id, set_row)?,
        set_cases: all(tx, SET_MEMBERS_OF_PROJECT, project.id, |row| {
            Ok(SetCase {
                set_id: row.get(0)?,
                case_id: row.get(1)?,
                position: row.get(2)?,
            })
        })?,
        plans: all(tx, PLANS_OF_PROJECT, project.id, plan_row)?,
        plan_items: all(tx, PLAN_ITEMS_OF_PROJECT, project.id, |row| {
            Ok(PlanItem {
                plan_id: row.get(0)?,
                position: row.get(1)?,
                case_id: row.get(2)?,
                set_id: row.get(3)?,
            })
        })?,
    };
    assert!(state.cases.iter().all(|case| case.project == project.id));
    assert!(state.sets.iter().all(|set| set.project == project.id));
    assert!(state.plans.iter().all(|plan| plan.project == project.id));
    Ok(state)
}

/// Rebuilds a project's test rows from its events alone, in the order `state` reads the
/// tables; ticket events are skipped. Inputs: the transaction and the key. Output: the
/// state, or `project_unknown`, or `Corrupt` for an event that cannot be applied.
pub fn replay(tx: &Transaction<'_>, key: &ProjectKey) -> Result<TestState, StoreError> {
    let project = store::project(tx, key)?;
    let mut replayed = Replayed::default();
    let mut statement = prepare(tx, TEST_EVENTS_OF_PROJECT)?;
    let mut rows = statement.query([project.id])?;
    while let Some(row) = rows.next()? {
        let op: String = row.get(0)?;
        if !OPS.contains(&op.as_str()) {
            continue;
        }
        let before: Option<String> = row.get(1)?;
        let after: Option<String> = row.get(2)?;
        replayed.apply(&op, before.as_deref(), after.as_deref())?;
    }
    Ok(replayed.into_state())
}

/// The test rows rebuilt so far by `replay`, keyed as their tables are ordered.
#[derive(Default)]
struct Replayed {
    cases: BTreeMap<i64, Case>,
    case_criteria: BTreeSet<CaseCriterion>,
    sets: BTreeMap<i64, TestSet>,
    set_cases: BTreeMap<(i64, u32), SetCase>,
    plans: BTreeMap<i64, TestPlan>,
    plan_items: BTreeMap<(i64, u32), PlanItem>,
}

impl Replayed {
    fn apply(
        &mut self,
        op: &str,
        before: Option<&str>,
        after: Option<&str>,
    ) -> Result<(), StoreError> {
        match op {
            CASE_CREATE | CASE_EDIT => {
                let case: Case = decoded(after)?;
                self.cases.insert(case.id, case);
            }
            CASE_LINK | CASE_UNLINK => {
                let (was, now): (
                    Membership<Case, CaseCriterion>,
                    Membership<Case, CaseCriterion>,
                ) = (decoded(before)?, decoded(after)?);
                if let Some(member) = was.member
                    && !self.case_criteria.remove(&member)
                {
                    return Err(unmatched());
                }
                self.case_criteria.extend(now.member);
                self.cases.insert(now.parent.id, now.parent);
            }
            SET_UPSERT => {
                let set: TestSet = decoded(after)?;
                self.sets.insert(set.id, set);
            }
            SET_ADD | SET_REMOVE => {
                let (was, now): (Membership<TestSet, SetCase>, Membership<TestSet, SetCase>) =
                    (decoded(before)?, decoded(after)?);
                if let Some(member) = was.member {
                    self.set_cases
                        .remove(&(member.set_id, member.position))
                        .ok_or_else(unmatched)?;
                }
                if let Some(member) = now.member {
                    self.set_cases
                        .insert((member.set_id, member.position), member);
                }
                self.sets.insert(now.parent.id, now.parent);
            }
            PLAN_UPSERT => {
                let plan: TestPlan = decoded(after)?;
                self.plans.insert(plan.id, plan);
            }
            PLAN_ADD | PLAN_REMOVE => {
                let (was, now): (
                    Membership<TestPlan, PlanItem>,
                    Membership<TestPlan, PlanItem>,
                ) = (decoded(before)?, decoded(after)?);
                if let Some(member) = was.member {
                    self.plan_items
                        .remove(&(member.plan_id, member.position))
                        .ok_or_else(unmatched)?;
                }
                if let Some(member) = now.member {
                    self.plan_items
                        .insert((member.plan_id, member.position), member);
                }
                self.plans.insert(now.parent.id, now.parent);
            }
            other => panic!("{other} is listed in OPS but not replayed"),
        }
        Ok(())
    }

    fn into_state(self) -> TestState {
        let mut cases: Vec<Case> = self.cases.into_values().collect();
        cases.sort_by_key(|case| case.short_id.number());
        let mut sets: Vec<TestSet> = self.sets.into_values().collect();
        sets.sort_by_key(|set| set.short_id.number());
        let mut plans: Vec<TestPlan> = self.plans.into_values().collect();
        plans.sort_by_key(|plan| plan.short_id.number());
        TestState {
            cases,
            case_criteria: self.case_criteria.into_iter().collect(),
            sets,
            set_cases: self.set_cases.into_values().collect(),
            plans,
            plan_items: self.plan_items.into_values().collect(),
        }
    }
}

fn unmatched() -> StoreError {
    StoreError::Corrupt("an event removes a test member no event added".into())
}

fn decoded<T: DeserializeOwned>(json: Option<&str>) -> Result<T, StoreError> {
    let json = json.ok_or_else(|| StoreError::Corrupt("a test event lacks its row".into()))?;
    serde_json::from_str(json).map_err(|error| {
        StoreError::Corrupt(format!("a test event row does not read back: {error}"))
    })
}

/// Reads `case_protected_visibility` for a project; a project that never set it gets the
/// proposed default `hidden`. Inputs: the transaction and the internal project id. Output:
/// the policy, or `Corrupt` for a stored value this core does not know.
pub(crate) fn protected_visibility(
    tx: &Transaction<'_>,
    project: i64,
) -> Result<CaseProtectedVisibility, StoreError> {
    let stored: Option<String> = prepare(tx, VISIBILITY_READ)?
        .query_row([project], |row| row.get(0))
        .optional()?;
    let Some(json) = stored else {
        return Ok(CaseProtectedVisibility::Hidden);
    };
    serde_json::from_str(&json).map_err(|_| {
        StoreError::Corrupt("a stored case_protected_visibility is not one this core knows".into())
    })
}

fn case_detail(tx: &Transaction<'_>, case: &Case) -> Result<CaseDetail, StoreError> {
    let key = case.short_id.key().clone();
    let criteria = prepare(tx, CRITERIA_OF_CASE)?
        .query_map([case.id], |row| {
            Ok(CriterionRef {
                ticket: ShortId::new(key.clone(), row.get(0)?)
                    .map_err(|refused| store::conversion(0, refused))?,
                position: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<CriterionRef>>>()?;
    assert!(criteria.len() <= CRITERIA_PER_CASE_MAX);
    let sets = prepare(tx, SETS_OF_CASE)?
        .query_map([case.id], |row| {
            TestId::new(key.clone(), TestObject::Set, row.get(0)?)
                .map_err(|refused| store::conversion(0, refused))
        })?
        .collect::<rusqlite::Result<Vec<TestId>>>()?;
    assert!(sets.len() <= SETS_PER_PROJECT_MAX);
    Ok(CaseDetail {
        kind: case.kind,
        status: case.status,
        stable_name: case.stable_name.clone(),
        criteria,
        sets,
        created_by: case.created_by.clone(),
        created_ms: case.created_ms,
        updated_ms: case.updated_ms,
        version: case.version,
    })
}

fn latest_outcome(
    tx: &Transaction<'_>,
    found: &Case,
) -> Result<Option<(ResultOutcome, TestId)>, StoreError> {
    let key = found.short_id.key();
    Ok(prepare(tx, LATEST_OUTCOME)?
        .query_row([found.id], |row| {
            let run = TestId::new(key.clone(), TestObject::Run, row.get(1)?)
                .map_err(|refused| store::conversion(1, refused))?;
            Ok((named(row, 0, ResultOutcome::parse)?, run))
        })
        .optional()?)
}

/// A case's latest outcome as the actor may read it: None when the host's `run_sight`
/// gives the actor only the outcome of the run it came from. Inputs: the transaction, the
/// host, the actor and the case. Output: the outcome, or the host's refusal.
fn latest_as(
    tx: &Transaction<'_>,
    host: &(impl Authority + ?Sized),
    actor: &Actor,
    found: &Case,
) -> Result<Option<ResultOutcome>, StoreError> {
    let Some((outcome, run)) = latest_outcome(tx, found)? else {
        return Ok(None);
    };
    Ok(match host.run_sight(tx, run.key(), actor, &run)? {
        Sight::Full => Some(outcome),
        Sight::Outcome => None,
    })
}

fn criterion_id(
    tx: &Transaction<'_>,
    case: &Case,
    criterion: &CriterionRef,
) -> Result<i64, StoreError> {
    if criterion.ticket.key() != case.short_id.key() {
        return Err(store::refusal(
            codes::CROSS_PROJECT,
            format!(
                "{} is in another project than {}",
                criterion.ticket, case.short_id
            ),
        ));
    }
    prepare(tx, CRITERION_OF_TICKET)?
        .query_row(
            params![case.project, criterion.ticket.number(), criterion.position],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            store::refusal(
                core_codes::CRITERION_UNKNOWN,
                format!(
                    "{} has no criterion {}",
                    criterion.ticket, criterion.position
                ),
            )
        })
}

fn item_ids(
    tx: &Transaction<'_>,
    plan: &TestPlan,
    item: &PlanItemRef,
) -> Result<(Option<i64>, Option<i64>), StoreError> {
    let (project, ids) = match item {
        PlanItemRef::Case(id) => {
            let found = case(tx, id)?;
            (found.project, (Some(found.id), None))
        }
        PlanItemRef::Set(id) => {
            let found = set(tx, id)?;
            (found.project, (None, Some(found.id)))
        }
    };
    if project != plan.project {
        let (PlanItemRef::Case(id) | PlanItemRef::Set(id)) = item;
        return Err(cross_project(id, &plan.short_id));
    }
    Ok(ids)
}

fn plan_targets(
    tx: &Transaction<'_>,
    project: i64,
    fields: &PlanFields,
) -> Result<(Option<i64>, Option<i64>), StoreError> {
    let release = match &fields.release {
        None => None,
        Some(name) => Some(
            prepare(tx, RELEASE_BY_NAME)?
                .query_row(params![project, name], |row| row.get(0))
                .optional()?
                .ok_or_else(|| {
                    store::refusal(codes::RELEASE_UNKNOWN, format!("no release named {name}"))
                })?,
        ),
    };
    let cycle = match fields.cycle {
        None => None,
        Some(number) => Some(
            prepare(tx, CYCLE_BY_NUMBER)?
                .query_row(params![project, number], |row| row.get(0))
                .optional()?
                .ok_or_else(|| {
                    store::refusal(codes::CYCLE_UNKNOWN, format!("no cycle number {number}"))
                })?,
        ),
    };
    Ok((release, cycle))
}

fn stable_name_free(
    tx: &Transaction<'_>,
    project: i64,
    name: &str,
    except: Option<i64>,
) -> Result<(), StoreError> {
    match case_id_for(tx, project, name)? {
        Some(holder) if Some(holder) != except => Err(store::refusal(
            codes::STABLE_NAME_TAKEN,
            "another case in this project already has that stable name",
        )),
        _ => Ok(()),
    }
}

fn check_case(title: &str, stable_name: Option<&str>, steps: &[String]) -> Result<(), StoreError> {
    checked("title", title, CASE_TITLE_BYTES_MAX, false)?;
    if let Some(name) = stable_name {
        checked("stable name", name, STABLE_NAME_BYTES_MAX, false)?;
    }
    if steps.len() > STEPS_PER_CASE_MAX {
        return Err(full(codes::STEPS_FULL, "steps", STEPS_PER_CASE_MAX));
    }
    for step in steps {
        checked("step", step, STEP_BYTES_MAX, true)?;
    }
    Ok(())
}

fn checked(field: &str, text: &str, bytes_max: usize, multiline: bool) -> Result<(), StoreError> {
    let code = if text.trim().is_empty() {
        core_codes::TEXT_EMPTY
    } else if text.len() > bytes_max {
        core_codes::TEXT_TOO_LONG
    } else if !multiline && text.contains(['\n', '\r']) {
        core_codes::TEXT_LINE_BREAK
    } else if text::hidden(text) {
        core_codes::TEXT_HIDDEN_CHARACTERS
    } else {
        return Ok(());
    };
    let message = format!("the {field} must be 1 to {bytes_max} bytes of plain text");
    Err(StoreError::Refused(Refusal::new(code, message)))
}

fn elide(title: &str) -> (String, bool) {
    if title.len() <= LIST_TITLE_BYTES_MAX {
        return (title.to_owned(), false);
    }
    let mut end = LIST_TITLE_BYTES_MAX;
    while !title.is_char_boundary(end) {
        end -= 1;
    }
    assert!(end > 0);
    assert!(end <= LIST_TITLE_BYTES_MAX);
    (title[..end].to_owned(), true)
}

fn of_object(id: &TestId, object: TestObject) -> Result<(), StoreError> {
    if id.object() == object {
        return Ok(());
    }
    Err(store::refusal(
        codes::TEST_ID_INVALID,
        format!("{id} is not a {object} id"),
    ))
}

fn same_key(id: &TestId, key: &ProjectKey) -> Result<(), StoreError> {
    if id.key() == key {
        return Ok(());
    }
    Err(store::refusal(
        codes::CROSS_PROJECT,
        format!("{id} is not in project {key}"),
    ))
}

fn cross_project(member: &TestId, parent: &TestId) -> StoreError {
    store::refusal(
        codes::CROSS_PROJECT,
        format!("{member} is in another project than {parent}"),
    )
}

fn protected_refused(id: &TestId) -> StoreError {
    store::refusal(
        codes::CASE_PROTECTED,
        format!("{id} is protected; only the owner changes it"),
    )
}

fn full(code: &'static str, what: &str, max: usize) -> StoreError {
    store::refusal(code, format!("at most {max} {what}"))
}

/// Appends one test event in the write's transaction. Test events carry no ticket, so a
/// ticket's history never holds a case's steps or stable name; any read of a project's
/// events must narrow protected cases with `case_view` before they leave the core. Inputs:
/// the transaction, project, op, actor, time and the row before and after. Output: Ok.
fn record<T: Serialize>(
    tx: &Transaction<'_>,
    project: i64,
    op: &'static str,
    actor: &Actor,
    now_ms: i64,
    before: Option<&T>,
    after: Option<&T>,
) -> Result<(), StoreError> {
    assert!(OPS.contains(&op), "every test op is listed");
    events::record(
        tx,
        Recorded {
            project,
            ticket: None,
            op,
            actor,
            at_ms: now_ms,
            before: before.map(events::snapshot),
            after: after.map(events::snapshot),
        },
    )
}

fn prepare<'t>(tx: &'t Transaction<'_>, sql: &'static str) -> rusqlite::Result<Statement<'t>> {
    assert!(
        STATEMENTS.contains(&sql),
        "every test model statement is listed"
    );
    tx.prepare(sql)
}

fn count<P: Params>(tx: &Transaction<'_>, sql: &'static str, args: P) -> Result<usize, StoreError> {
    let counted: i64 = prepare(tx, sql)?.query_row(args, |row| row.get(0))?;
    assert!(counted >= 0);
    Ok(counted as usize)
}

fn next(tx: &Transaction<'_>, sql: &'static str, owner: i64) -> Result<i64, StoreError> {
    let number: i64 = prepare(tx, sql)?.query_row([owner], |row| row.get(0))?;
    assert!(number >= 1);
    assert!(number <= i64::from(u32::MAX));
    Ok(number)
}

fn number_of(value: i64) -> u32 {
    match u32::try_from(value) {
        Ok(number) => number,
        Err(_) => panic!("a bounded count or position fits u32: {value}"),
    }
}

fn steps_json(steps: &[String]) -> String {
    match serde_json::to_string(steps) {
        Ok(json) => json,
        Err(error) => panic!("a list of strings always serializes: {error}"),
    }
}

fn all<T>(
    tx: &Transaction<'_>,
    sql: &'static str,
    project: i64,
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, StoreError> {
    Ok(prepare(tx, sql)?
        .query_map([project], map)?
        .collect::<rusqlite::Result<Vec<T>>>()?)
}

fn case_by_id(tx: &Transaction<'_>, id: i64) -> Result<Case, StoreError> {
    let found: Case = prepare(tx, CASE_BY_ID)?.query_row([id], case_row)?;
    assert_eq!(found.id, id);
    assert_eq!(found.short_id.object(), TestObject::Case);
    Ok(found)
}

fn set_by_id(tx: &Transaction<'_>, id: i64) -> Result<TestSet, StoreError> {
    let found: TestSet = prepare(tx, SET_BY_ID)?.query_row([id], set_row)?;
    assert_eq!(found.id, id);
    assert_eq!(found.short_id.object(), TestObject::Set);
    Ok(found)
}

fn plan_by_id(tx: &Transaction<'_>, id: i64) -> Result<TestPlan, StoreError> {
    let found: TestPlan = prepare(tx, PLAN_BY_ID)?.query_row([id], plan_row)?;
    assert_eq!(found.id, id);
    assert_eq!(found.short_id.object(), TestObject::Plan);
    Ok(found)
}

fn test_id(row: &Row<'_>, object: TestObject) -> rusqlite::Result<TestId> {
    let key = ProjectKey::new(&row.get::<_, String>(2)?)
        .map_err(|refused| store::conversion(2, refused))?;
    TestId::new(key, object, row.get(3)?).map_err(|refused| store::conversion(3, refused))
}

fn named<T>(row: &Row<'_>, index: usize, parse: fn(&str) -> Option<T>) -> rusqlite::Result<T> {
    let stored: String = row.get(index)?;
    parse(&stored).ok_or_else(|| {
        store::conversion(
            index,
            Refusal::new(
                core_codes::STORE_FAILED,
                "a stored name is not one this core knows",
            ),
        )
    })
}

fn case_row(row: &Row<'_>) -> rusqlite::Result<Case> {
    let steps_json: String = row.get(7)?;
    let steps: Vec<String> =
        serde_json::from_str(&steps_json).map_err(|error| store::conversion(7, error))?;
    Ok(Case {
        id: row.get(0)?,
        project: row.get(1)?,
        short_id: test_id(row, TestObject::Case)?,
        title: row.get(4)?,
        kind: named(row, 5, CaseKind::parse)?,
        stable_name: row.get(6)?,
        steps,
        protected: row.get(8)?,
        status: named(row, 9, CaseStatus::parse)?,
        created_by: store::actor(row, 10)?,
        created_ms: row.get(11)?,
        updated_ms: row.get(12)?,
        version: row.get(13)?,
    })
}

fn set_row(row: &Row<'_>) -> rusqlite::Result<TestSet> {
    Ok(TestSet {
        id: row.get(0)?,
        project: row.get(1)?,
        short_id: test_id(row, TestObject::Set)?,
        name: row.get(4)?,
        version: row.get(5)?,
    })
}

fn plan_row(row: &Row<'_>) -> rusqlite::Result<TestPlan> {
    Ok(TestPlan {
        id: row.get(0)?,
        project: row.get(1)?,
        short_id: test_id(row, TestObject::Plan)?,
        name: row.get(4)?,
        version_id: row.get(5)?,
        cycle_id: row.get(6)?,
        status: named(row, 7, PlanStatus::parse)?,
        version: row.get(8)?,
    })
}

fn plan_item_view(row: &Row<'_>, key: &ProjectKey) -> rusqlite::Result<PlanItemView> {
    let position: u32 = row.get(0)?;
    let case: Option<u32> = row.get(1)?;
    let set: Option<u32> = row.get(2)?;
    let item = match (case, set) {
        (Some(number), None) => PlanItemRef::Case(
            TestId::new(key.clone(), TestObject::Case, number)
                .map_err(|refused| store::conversion(1, refused))?,
        ),
        (None, Some(number)) => PlanItemRef::Set(
            TestId::new(key.clone(), TestObject::Set, number)
                .map_err(|refused| store::conversion(2, refused))?,
        ),
        _ => {
            return Err(store::conversion(
                1,
                Refusal::new(
                    core_codes::STORE_FAILED,
                    "a plan item holds one case or one set",
                ),
            ));
        }
    };
    Ok(PlanItemView { position, item })
}
