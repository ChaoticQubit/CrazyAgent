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

/// Migration step 2: the test management tables, added beside the step 1 tables without
/// touching a step 1 row. Results are never updated; retention (VT-2) may delete the
/// results of a pruned run.
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
";
