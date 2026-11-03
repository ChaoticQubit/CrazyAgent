use crate::model::{Refusal, codes};
use crate::store::{self, StoreError};
use rusqlite::{OptionalExtension, Transaction};

pub const SCHEMA_VERSION: i64 = 2;

const META: &str = "CREATE TABLE IF NOT EXISTS tk_meta(
    key TEXT PRIMARY KEY NOT NULL,
    value INTEGER NOT NULL
) WITHOUT ROWID;";

pub(crate) const VERSION_READ: &str = "SELECT value FROM tk_meta WHERE key = 'schema_version'";

pub(crate) const VERSION_WRITE: &str =
    "INSERT INTO tk_meta(key, value) VALUES ('schema_version', ?1)
    ON CONFLICT(key) DO UPDATE SET value = excluded.value";

const V1: &str = "
CREATE TABLE tk_projects(
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE CHECK (length(key) BETWEEN 2 AND 8 AND key NOT GLOB '*[^A-Z]*'),
    name TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    version INTEGER NOT NULL CHECK (version >= 1)
);
CREATE TABLE tk_versions(
    id INTEGER PRIMARY KEY,
    project INTEGER NOT NULL REFERENCES tk_projects(id),
    name TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    target_day TEXT,
    status TEXT NOT NULL CHECK (status IN ('planned', 'active', 'released')),
    source TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version >= 1),
    UNIQUE (project, name)
);
CREATE TABLE tk_cycles(
    id INTEGER PRIMARY KEY,
    project INTEGER NOT NULL REFERENCES tk_projects(id),
    version_id INTEGER REFERENCES tk_versions(id),
    number INTEGER NOT NULL CHECK (number >= 1),
    starts_day TEXT NOT NULL,
    ends_day TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('upcoming', 'active', 'closed')),
    version INTEGER NOT NULL CHECK (version >= 1),
    UNIQUE (project, number)
);
CREATE TABLE tk_tickets(
    id INTEGER PRIMARY KEY,
    project INTEGER NOT NULL REFERENCES tk_projects(id),
    number INTEGER NOT NULL CHECK (number >= 1),
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('feature', 'bug', 'chore', 'spike')),
    status TEXT NOT NULL CHECK (status IN
        ('triage', 'backlog', 'todo', 'in_progress', 'in_review', 'done', 'cancelled')),
    priority INTEGER NOT NULL CHECK (priority BETWEEN 0 AND 4),
    version_id INTEGER REFERENCES tk_versions(id),
    cycle_id INTEGER REFERENCES tk_cycles(id),
    parent_id INTEGER REFERENCES tk_tickets(id),
    assignee TEXT,
    created_by TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL,
    closed_ms INTEGER,
    closed_by TEXT,
    close_reason TEXT,
    version INTEGER NOT NULL CHECK (version >= 1),
    UNIQUE (project, number)
);
CREATE INDEX tk_tickets_created_by ON tk_tickets(created_by, status);
CREATE INDEX tk_tickets_parent ON tk_tickets(parent_id);
CREATE INDEX tk_tickets_cycle ON tk_tickets(cycle_id);
CREATE INDEX tk_tickets_version ON tk_tickets(version_id);
CREATE TABLE tk_deps(
    ticket INTEGER NOT NULL REFERENCES tk_tickets(id),
    blocks INTEGER NOT NULL REFERENCES tk_tickets(id),
    PRIMARY KEY (ticket, blocks),
    CHECK (ticket <> blocks)
) WITHOUT ROWID;
CREATE INDEX tk_deps_blocks ON tk_deps(blocks);
CREATE TABLE tk_criteria(
    id INTEGER PRIMARY KEY,
    ticket INTEGER NOT NULL REFERENCES tk_tickets(id),
    position INTEGER NOT NULL CHECK (position >= 1),
    text TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('open', 'met', 'waived')),
    version INTEGER NOT NULL CHECK (version >= 1),
    UNIQUE (ticket, position)
);
CREATE TABLE tk_checks(
    id INTEGER PRIMARY KEY,
    criterion INTEGER NOT NULL REFERENCES tk_criteria(id),
    kind TEXT NOT NULL CHECK (kind IN ('test', 'change', 'manual', 'ci')),
    ref TEXT NOT NULL,
    UNIQUE (criterion, kind, ref)
);
CREATE TABLE tk_evidence(
    id INTEGER PRIMARY KEY,
    check_id INTEGER NOT NULL REFERENCES tk_checks(id),
    result TEXT NOT NULL CHECK (result IN ('pass', 'fail')),
    ref TEXT NOT NULL,
    recorded_by TEXT NOT NULL,
    recorded_ms INTEGER NOT NULL
);
CREATE INDEX tk_evidence_check ON tk_evidence(check_id, id);
CREATE TABLE tk_links(
    ticket INTEGER NOT NULL REFERENCES tk_tickets(id),
    kind TEXT NOT NULL,
    target TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    PRIMARY KEY (ticket, kind, target)
) WITHOUT ROWID;
CREATE INDEX tk_links_target ON tk_links(kind, target);
CREATE TABLE tk_events(
    id INTEGER PRIMARY KEY,
    ticket INTEGER REFERENCES tk_tickets(id),
    project INTEGER NOT NULL REFERENCES tk_projects(id),
    at_ms INTEGER NOT NULL,
    actor TEXT NOT NULL,
    op TEXT NOT NULL,
    before_json TEXT,
    after_json TEXT,
    CHECK (before_json IS NOT NULL OR after_json IS NOT NULL)
);
CREATE INDEX tk_events_ticket ON tk_events(ticket, id);
CREATE INDEX tk_events_project ON tk_events(project, id);
CREATE INDEX tk_events_actor ON tk_events(actor, at_ms);
CREATE TABLE tk_requests(
    actor TEXT NOT NULL,
    request_id TEXT NOT NULL,
    payload_sha TEXT NOT NULL,
    reply_json TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    PRIMARY KEY (actor, request_id)
) WITHOUT ROWID;
CREATE INDEX tk_requests_at ON tk_requests(at_ms);
CREATE TABLE tk_policy(
    project INTEGER NOT NULL REFERENCES tk_projects(id),
    key TEXT NOT NULL CHECK (key IN
        ('close', 'create', 'agent_edit', 'cycle_days', 'rollover', 'import')),
    value_json TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version >= 1),
    PRIMARY KEY (project, key)
) WITHOUT ROWID;
CREATE TRIGGER tk_events_no_update BEFORE UPDATE ON tk_events
    BEGIN SELECT RAISE(ABORT, 'tk_events is append-only'); END;
CREATE TRIGGER tk_events_no_delete BEFORE DELETE ON tk_events
    BEGIN SELECT RAISE(ABORT, 'tk_events is append-only'); END;
CREATE TRIGGER tk_evidence_no_update BEFORE UPDATE ON tk_evidence
    BEGIN SELECT RAISE(ABORT, 'tk_evidence is append-only'); END;
CREATE TRIGGER tk_evidence_no_delete BEFORE DELETE ON tk_evidence
    BEGIN SELECT RAISE(ABORT, 'tk_evidence is append-only'); END;
";

const MIGRATIONS: [&str; SCHEMA_VERSION as usize] = [V1, crate::tests_store::SCHEMA_STEP_TWO];

/// Brings the core's `tk_` tables to `SCHEMA_VERSION` inside the caller's transaction, so a
/// host migrates them beside its own tables. Forward-only and idempotent: a current schema
/// is left as it is. Input: the open transaction. Output: Ok, or `store_schema_newer` for a
/// database a later core wrote.
pub fn migrate(tx: &Transaction<'_>) -> Result<(), StoreError> {
    migrate_to(tx, SCHEMA_VERSION)
}

/// Brings the core's `tk_` tables up to one schema step, so a test can build the file an
/// earlier core wrote. A schema at or past the step is left as it is. Inputs: the open
/// transaction and the step, 1 to `SCHEMA_VERSION`. Output: Ok, or `store_schema_newer`.
pub fn migrate_to(tx: &Transaction<'_>, step: i64) -> Result<(), StoreError> {
    assert!(step >= 1, "schema steps start at 1");
    assert!(
        step <= SCHEMA_VERSION,
        "step {step} is past this core's schema"
    );
    tx.execute_batch(META)?;
    let current: i64 = store::prepare(tx, VERSION_READ)?
        .query_row([], |row| row.get(0))
        .optional()?
        .unwrap_or(0);
    if current > SCHEMA_VERSION {
        return Err(StoreError::Refused(Refusal::new(
            codes::STORE_SCHEMA_NEWER,
            format!(
                "the tracker schema is version {current}; this tracker reads up to version {SCHEMA_VERSION}"
            ),
        )));
    }
    if current < 0 {
        return Err(StoreError::Corrupt(format!(
            "the tracker schema version {current} is negative"
        )));
    }
    if current >= step {
        return Ok(());
    }
    for (index, migration) in MIGRATIONS
        .iter()
        .enumerate()
        .take(step as usize)
        .skip(current as usize)
    {
        tx.execute_batch(migration)?;
        store::prepare(tx, VERSION_WRITE)?.execute([index as i64 + 1])?;
    }
    Ok(())
}
