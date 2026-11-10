use super::{AgentId, BODY_BYTES_MAX, Message, MessageKind, Party, QuestionFor, Receipt, Severity};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Row, params};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS coord_work (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    title TEXT NOT NULL,
    owner_agent INTEGER,
    state TEXT NOT NULL,
    version INTEGER NOT NULL,
    evidence TEXT,
    created INTEGER NOT NULL,
    updated INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS coord_work_project ON coord_work(project, state);
CREATE TABLE IF NOT EXISTS coord_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    from_kind TEXT NOT NULL,
    from_agent INTEGER,
    to_kind TEXT NOT NULL,
    to_agent INTEGER,
    kind TEXT NOT NULL,
    q_for TEXT,
    q_severity TEXT,
    work INTEGER,
    reply_to INTEGER,
    body TEXT NOT NULL,
    idem_key TEXT,
    receipt TEXT NOT NULL,
    created INTEGER NOT NULL,
    receipt_at INTEGER NOT NULL,
    UNIQUE(from_agent, idem_key)
);
CREATE INDEX IF NOT EXISTS coord_messages_to ON coord_messages(to_kind, to_agent, receipt);
CREATE INDEX IF NOT EXISTS coord_messages_project ON coord_messages(project, kind, receipt);
CREATE TABLE IF NOT EXISTS coord_claims (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    agent INTEGER NOT NULL,
    work INTEGER NOT NULL,
    pattern TEXT NOT NULL,
    created INTEGER NOT NULL,
    released INTEGER
);
CREATE INDEX IF NOT EXISTS coord_claims_project ON coord_claims(project, released);
CREATE INDEX IF NOT EXISTS coord_claims_work ON coord_claims(work, released);
CREATE TABLE IF NOT EXISTS coord_held (
    claim INTEGER PRIMARY KEY,
    project TEXT NOT NULL,
    change_set TEXT NOT NULL,
    released INTEGER
);
CREATE INDEX IF NOT EXISTS coord_held_set ON coord_held(change_set, released);
CREATE TABLE IF NOT EXISTS coord_claims_full (
    project TEXT PRIMARY KEY,
    told INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS coord_waits (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    agent INTEGER NOT NULL,
    work INTEGER NOT NULL,
    file TEXT NOT NULL,
    held TEXT NOT NULL,
    created INTEGER NOT NULL,
    UNIQUE(agent, work)
);
CREATE TABLE IF NOT EXISTS coord_wait_cycles (
    project TEXT NOT NULL,
    first INTEGER NOT NULL,
    second INTEGER NOT NULL,
    holder INTEGER NOT NULL,
    file TEXT NOT NULL,
    created INTEGER NOT NULL,
    told INTEGER,
    PRIMARY KEY(project, first, second)
);
CREATE TABLE IF NOT EXISTS coord_activity (
    agent INTEGER PRIMARY KEY,
    last_seen INTEGER NOT NULL,
    waiting TEXT,
    waiting_since INTEGER
);
CREATE TABLE IF NOT EXISTS coord_tool_use (
    agent INTEGER NOT NULL,
    tool TEXT NOT NULL,
    count INTEGER NOT NULL,
    last INTEGER NOT NULL,
    PRIMARY KEY(agent, tool)
);
CREATE TABLE IF NOT EXISTS coord_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project TEXT NOT NULL,
    work INTEGER NOT NULL,
    by_kind TEXT NOT NULL,
    by_agent INTEGER,
    action TEXT NOT NULL,
    from_version INTEGER NOT NULL,
    to_version INTEGER NOT NULL,
    at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS coord_audit_agent ON coord_audit(by_kind, by_agent, action);
CREATE TABLE IF NOT EXISTS coord_settings (
    project TEXT PRIMARY KEY,
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS coord_harness_settings (
    project TEXT NOT NULL,
    harness TEXT NOT NULL,
    mode TEXT NOT NULL DEFAULT 'app_spawned',
    delivery TEXT NOT NULL DEFAULT 'auto',
    PRIMARY KEY(project, harness)
);
CREATE TABLE IF NOT EXISTS tool_switches (
    scope_project TEXT NOT NULL,
    server TEXT NOT NULL,
    tool TEXT NOT NULL,
    switch TEXT NOT NULL,
    PRIMARY KEY(scope_project, server, tool)
);";

/// The coordination tables a project's removal clears by their `project` column.
const REMOVED_WITH_PROJECT: [&str; 10] = [
    "coord_work",
    "coord_messages",
    "coord_claims",
    "coord_held",
    "coord_claims_full",
    "coord_waits",
    "coord_wait_cycles",
    "coord_audit",
    "coord_settings",
    "coord_harness_settings",
];

/// Creates the coordination tables (DEC-104) when they are missing, like the team
/// tables, and adds `coord_messages.turn` (the Codex turn a bridge delivery went into,
/// b19 P2) to a database made before it. Runs after `team::initialize`, whose agent rows
/// the layer reads. Takes the engine database. Returns Ok, or the SQLite failure.
pub fn ensure(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(state)?;
    let present: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('coord_messages') WHERE name='turn')",
            [],
            |row| row.get(0),
        )
        .map_err(state)?;
    if !present {
        db.execute_batch("ALTER TABLE coord_messages ADD COLUMN turn TEXT")
            .map_err(state)?;
    }
    Ok(())
}

/// Clears a removed project's coordination rows: its work, messages, claims and their
/// change-set holds, full-scan notice, waits and wait cycles, audit, settings and tool switches, and the activity and tool counts
/// of the stopped agents removed with it. Runs inside the removal's transaction before its agents go.
/// Takes the database and the project. Returns Ok or the SQLite failure.
pub fn forget_project(db: &Connection, project: &ProjectId) -> Result<(), EngineError> {
    assert!(!project.as_str().is_empty());
    for table in REMOVED_WITH_PROJECT {
        db.execute(
            &format!("DELETE FROM {table} WHERE project=?1"),
            [project.as_str()],
        )
        .map_err(state)?;
    }
    for table in ["coord_activity", "coord_tool_use"] {
        db.execute(
            &format!(
                "DELETE FROM {table} WHERE agent IN (SELECT id FROM team_agents
                 WHERE project=?1 AND status IN ('stopped','failed'))"
            ),
            [project.as_str()],
        )
        .map_err(state)?;
    }
    db.execute(
        "DELETE FROM tool_switches WHERE scope_project=?1",
        [project.as_str()],
    )
    .map_err(state)?;
    Ok(())
}

/// A plain refusal code for the caller.
pub(super) fn failure(code: &str) -> EngineError {
    EngineError(code.into())
}

/// A SQLite failure, kept apart from refusals so callers can tell bad input from an
/// unreadable store.
pub(super) fn state(problem: impl std::fmt::Display) -> EngineError {
    EngineError(format!("coord_state_unavailable: {problem}"))
}

/// One atomic change to the coordination tables: an immediate transaction on a
/// connection outside one, else a savepoint inside the caller's transaction, so team
/// actions that already hold a transaction can call the layer. Rolled back unless
/// committed.
pub(super) struct Change<'a> {
    db: &'a Connection,
    nested: bool,
    open: bool,
}

impl<'a> Change<'a> {
    /// Starts the change. Takes the engine database. Returns the open change.
    pub(super) fn begin(db: &'a Connection) -> Result<Self, EngineError> {
        let nested = !db.is_autocommit();
        db.execute_batch(if nested {
            "SAVEPOINT coord_change"
        } else {
            "BEGIN IMMEDIATE"
        })
        .map_err(state)?;
        Ok(Self {
            db,
            nested,
            open: true,
        })
    }

    /// The connection the change runs on.
    pub(super) fn db(&self) -> &'a Connection {
        self.db
    }

    /// Makes the change last. Takes the change. Returns Ok, or the SQLite failure after
    /// which nothing of it stays.
    pub(super) fn commit(mut self) -> Result<(), EngineError> {
        self.db
            .execute_batch(if self.nested {
                "RELEASE coord_change"
            } else {
                "COMMIT"
            })
            .map_err(state)?;
        self.open = false;
        Ok(())
    }
}

impl Drop for Change<'_> {
    fn drop(&mut self) {
        if self.open {
            let _ = self.db.execute_batch(if self.nested {
                "ROLLBACK TO coord_change; RELEASE coord_change"
            } else {
                "ROLLBACK"
            });
        }
    }
}

/// The parts of a team agent's row the layer decides on.
pub(super) struct AgentRow {
    pub project: String,
    pub role: String,
    pub parent: Option<AgentId>,
    pub status: String,
    pub created: i64,
}

impl AgentRow {
    /// Says whether the agent's process is not stopped or failed.
    pub(super) fn live(&self) -> bool {
        !matches!(self.status.as_str(), "stopped" | "failed")
    }

    /// Says whether the agent is the project's master.
    pub(super) fn master(&self) -> bool {
        self.role == "master"
    }
}

/// Reads one team agent. Takes the database and the id. Returns the row, or None.
pub(super) fn agent(db: &Connection, id: AgentId) -> Result<Option<AgentRow>, EngineError> {
    db.query_row(
        "SELECT project,role,parent,status,created_ms FROM team_agents WHERE id=?1",
        [id],
        |row| {
            Ok(AgentRow {
                project: row.get(0)?,
                role: row.get(1)?,
                parent: row.get(2)?,
                status: row.get(3)?,
                created: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(state)
}

/// Reads one team agent of a project. Takes the database, the project and the id.
/// Returns the row, or `coord_agent_unknown` for an agent of no or another project.
pub(super) fn agent_in(
    db: &Connection,
    project: &ProjectId,
    id: AgentId,
) -> Result<AgentRow, EngineError> {
    agent(db, id)?
        .filter(|row| row.project == project.as_str())
        .ok_or_else(|| failure("coord_agent_unknown"))
}

/// Finds the project's running master. Takes the database and the project. Returns its
/// id, or None while no master runs.
pub fn master_of(db: &Connection, project: &ProjectId) -> Result<Option<AgentId>, EngineError> {
    db.query_row(
        "SELECT id FROM team_agents WHERE project=?1 AND role='master'
         AND status NOT IN ('stopped','failed') ORDER BY id DESC LIMIT 1",
        [project.as_str()],
        |row| row.get(0),
    )
    .optional()
    .map_err(state)
}

/// Says whether an agent is its project's master. Takes the database and the id. Returns
/// the answer, false for an agent with no row.
pub fn is_master(db: &Connection, id: AgentId) -> Result<bool, EngineError> {
    Ok(agent(db, id)?.is_some_and(|row| row.master()))
}

/// Says whether two agents are lead and sub-agent of each other, whose shared scope is
/// expected and never an overlap (Concord). Takes the database and both ids.
pub(super) fn related(db: &Connection, a: AgentId, b: AgentId) -> Result<bool, EngineError> {
    let parent = |id: AgentId| -> Result<Option<AgentId>, EngineError> {
        Ok(agent(db, id)?.and_then(|row| row.parent))
    };
    Ok(parent(a)? == Some(b) || parent(b)? == Some(a))
}

/// Checks that a party may act in a project: the owner and the engine always, an agent
/// only when it belongs to the project. Takes the database, the project and the party.
/// Returns Ok, or `coord_agent_unknown`.
pub(super) fn party_in(
    db: &Connection,
    project: &ProjectId,
    party: Party,
) -> Result<(), EngineError> {
    match party {
        Party::Owner | Party::Engine => Ok(()),
        Party::Agent(id) => agent_in(db, project, id).map(|_| ()),
    }
}

/// Checks that a project exists before a setting is stored for it. Takes the database
/// and the project. Returns Ok, or `coord_project_unknown`.
pub(super) fn project_known(db: &Connection, project: &ProjectId) -> Result<(), EngineError> {
    let found: Option<i64> = db
        .query_row(
            "SELECT 1 FROM projects WHERE id=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?;
    found
        .map(|_| ())
        .ok_or_else(|| failure("coord_project_unknown"))
}

/// The stored kind and agent column of a party.
pub(super) fn party_columns(party: Party) -> (&'static str, Option<AgentId>) {
    match party {
        Party::Owner => ("owner", None),
        Party::Agent(id) => ("agent", Some(id)),
        Party::Engine => ("engine", None),
    }
}

/// Reads a party back from its stored kind and agent column. Returns None for a pair the
/// layer never writes.
pub(super) fn party_of(kind: &str, agent: Option<AgentId>) -> Option<Party> {
    match (kind, agent) {
        ("owner", None) => Some(Party::Owner),
        ("agent", Some(id)) => Some(Party::Agent(id)),
        ("engine", None) => Some(Party::Engine),
        _ => None,
    }
}

/// Checks free text an agent or the owner wrote: not blank, at most `max` bytes, no
/// control character but newline and tab. Takes the text and the bound. Returns Ok, or
/// `coord_text_invalid`.
pub(super) fn check_text(text: &str, max: usize) -> Result<(), EngineError> {
    assert!(max <= BODY_BYTES_MAX);
    let fine = !text.trim().is_empty()
        && text.len() <= max
        && !text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t');
    fine.then_some(())
        .ok_or_else(|| failure("coord_text_invalid"))
}

/// The columns `message_from_row` reads, in its order.
pub(super) const MESSAGE_COLUMNS: &str = "id,project,from_kind,from_agent,to_kind,to_agent,kind,q_for,q_severity,work,reply_to,body,receipt,created";

/// Reads one message row selected with `MESSAGE_COLUMNS`. A row the layer never writes
/// fails as a SQLite conversion error.
pub(super) fn message_from_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    let bad = |index: usize| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            "coord_message_row_invalid".into(),
        )
    };
    let project: String = row.get(1)?;
    let from_kind: String = row.get(2)?;
    let to_kind: String = row.get(4)?;
    let kind: String = row.get(6)?;
    let q_for: Option<String> = row.get(7)?;
    let q_severity: Option<String> = row.get(8)?;
    let receipt: String = row.get(12)?;
    let question = match (q_for, q_severity) {
        (None, None) => None,
        (Some(target), Some(severity)) => Some((
            QuestionFor::parse(&target).ok_or_else(|| bad(7))?,
            Severity::parse(&severity).ok_or_else(|| bad(8))?,
        )),
        _ => return Err(bad(7)),
    };
    Ok(Message {
        id: row.get(0)?,
        project: ProjectId::new(project).map_err(|_| bad(1))?,
        from: party_of(&from_kind, row.get(3)?).ok_or_else(|| bad(2))?,
        to: party_of(&to_kind, row.get(5)?).ok_or_else(|| bad(4))?,
        kind: MessageKind::parse(&kind).ok_or_else(|| bad(6))?,
        question,
        work: row.get(9)?,
        reply_to: row.get(10)?,
        body: row.get(11)?,
        receipt: Receipt::parse(&receipt).ok_or_else(|| bad(12))?,
        created: row.get(13)?,
    })
}

/// Reads one message. Takes the database and the id. Returns it, or
/// `coord_message_unknown`.
pub fn message(db: &Connection, id: i64) -> Result<Message, EngineError> {
    db.query_row(
        &format!("SELECT {MESSAGE_COLUMNS} FROM coord_messages WHERE id=?1"),
        params![id],
        message_from_row,
    )
    .optional()
    .map_err(state)?
    .ok_or_else(|| failure("coord_message_unknown"))
}
