use super::git::failure;
use super::{AgentKind, ChangeAgent, ChangeSetId, ChangeSetView, ChangeState, TurnOutcome};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS landing_change_sets (
    id TEXT PRIMARY KEY,
    project TEXT NOT NULL,
    agent_kind TEXT NOT NULL CHECK(agent_kind IN ('master','sub')),
    agent_id TEXT NOT NULL,
    label TEXT NOT NULL,
    repository TEXT NOT NULL,
    worktree TEXT NOT NULL,
    git_dir TEXT NOT NULL,
    base TEXT NOT NULL,
    head TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending','conflict','updating','accepted','discarded')),
    last_outcome TEXT NOT NULL CHECK(last_outcome IN ('succeeded','failed','cancelled')),
    turn TEXT NOT NULL,
    prompt_line TEXT NOT NULL,
    files INTEGER NOT NULL,
    added INTEGER NOT NULL,
    removed INTEGER NOT NULL,
    conflicts TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL,
    restorable_until_ms INTEGER,
    pruned INTEGER NOT NULL DEFAULT 0 CHECK(pruned IN (0,1)),
    landed TEXT,
    intent TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS landing_change_sets_open
    ON landing_change_sets(worktree) WHERE state IN ('pending','conflict','updating');
CREATE INDEX IF NOT EXISTS landing_change_sets_project
    ON landing_change_sets(project, updated_ms);";

const COLUMNS: &str = "id,project,agent_kind,agent_id,label,repository,worktree,git_dir,base,head,\
    state,last_outcome,turn,prompt_line,files,added,removed,conflicts,created_ms,updated_ms,\
    restorable_until_ms,pruned,landed,intent";

/// A step of owner-checkout or worktree work the engine recorded before starting it, so
/// an engine start can finish or undo it. `Land` moves the owner's `reference` from
/// `old` to the squash commit `new`; `Reset` points the agent's worktree at `target`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Intent {
    Land {
        reference: String,
        old: String,
        new: String,
    },
    Reset {
        target: String,
    },
}

/// One stored change set with everything the engine needs to act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Record {
    pub(super) id: ChangeSetId,
    pub(super) project: ProjectId,
    pub(super) agent: ChangeAgent,
    pub(super) repository: PathBuf,
    pub(super) worktree: PathBuf,
    pub(super) git_dir: PathBuf,
    pub(super) base: String,
    pub(super) head: String,
    pub(super) state: ChangeState,
    pub(super) last_outcome: TurnOutcome,
    pub(super) turn: String,
    pub(super) prompt_line: String,
    pub(super) files: u32,
    pub(super) added: u32,
    pub(super) removed: u32,
    pub(super) conflicts: Vec<String>,
    pub(super) created_ms: u64,
    pub(super) updated_ms: u64,
    pub(super) restorable_until_ms: Option<u64>,
    pub(super) pruned: bool,
    pub(super) landed: Option<String>,
    pub(super) intent: Option<Intent>,
}

impl Record {
    pub(super) fn view(&self) -> ChangeSetView {
        ChangeSetView {
            id: self.id.clone(),
            agent: self.agent.clone(),
            state: self.state,
            base: self.base.clone(),
            head: self.head.clone(),
            files: self.files,
            added: self.added,
            removed: self.removed,
            last_outcome: self.last_outcome,
            checked: false,
            created_ms: self.created_ms,
            updated_ms: self.updated_ms,
            restorable_until_ms: self.restorable_until_ms,
            conflicts: self.conflicts.clone(),
        }
    }

    pub(super) fn is_open(&self) -> bool {
        matches!(
            self.state,
            ChangeState::Pending | ChangeState::Conflict | ChangeState::Updating
        )
    }
}

fn storage(_: impl std::fmt::Display) -> EngineError {
    failure("changes_unavailable")
}

pub(super) fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(storage)
}

fn kind_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Master => "master",
        AgentKind::Sub => "sub",
    }
}

pub(super) fn state_name(state: ChangeState) -> &'static str {
    match state {
        ChangeState::Pending => "pending",
        ChangeState::Conflict => "conflict",
        ChangeState::Updating => "updating",
        ChangeState::Accepted => "accepted",
        ChangeState::Discarded => "discarded",
    }
}

pub(super) fn outcome_name(outcome: TurnOutcome) -> &'static str {
    match outcome {
        TurnOutcome::Succeeded => "succeeded",
        TurnOutcome::Failed => "failed",
        TurnOutcome::Cancelled => "cancelled",
    }
}

fn invalid(index: usize) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(
        index,
        "landing_change_sets".into(),
        rusqlite::types::Type::Text,
    )
}

fn read(row: &Row<'_>) -> rusqlite::Result<Record> {
    let text = |index: usize| row.get::<_, String>(index);
    let count = |index: usize| {
        row.get::<_, i64>(index)
            .and_then(|value| u32::try_from(value).map_err(|_| invalid(index)))
    };
    let millis = |index: usize| {
        row.get::<_, i64>(index)
            .and_then(|value| u64::try_from(value).map_err(|_| invalid(index)))
    };
    let state = match text(10)?.as_str() {
        "pending" => ChangeState::Pending,
        "conflict" => ChangeState::Conflict,
        "updating" => ChangeState::Updating,
        "accepted" => ChangeState::Accepted,
        "discarded" => ChangeState::Discarded,
        _ => return Err(invalid(10)),
    };
    let last_outcome = match text(11)?.as_str() {
        "succeeded" => TurnOutcome::Succeeded,
        "failed" => TurnOutcome::Failed,
        "cancelled" => TurnOutcome::Cancelled,
        _ => return Err(invalid(11)),
    };
    let kind = match text(2)?.as_str() {
        "master" => AgentKind::Master,
        "sub" => AgentKind::Sub,
        _ => return Err(invalid(2)),
    };
    let intent = row
        .get::<_, Option<String>>(23)?
        .map(|body| serde_json::from_str(&body).map_err(|_| invalid(23)))
        .transpose()?;
    Ok(Record {
        id: ChangeSetId::new(text(0)?).map_err(|_| invalid(0))?,
        project: ProjectId::new(text(1)?).map_err(|_| invalid(1))?,
        agent: ChangeAgent {
            kind,
            id: text(3)?,
            label: text(4)?,
        },
        repository: PathBuf::from(text(5)?),
        worktree: PathBuf::from(text(6)?),
        git_dir: PathBuf::from(text(7)?),
        base: text(8)?,
        head: text(9)?,
        state,
        last_outcome,
        turn: text(12)?,
        prompt_line: text(13)?,
        files: count(14)?,
        added: count(15)?,
        removed: count(16)?,
        conflicts: serde_json::from_str(&text(17)?).map_err(|_| invalid(17))?,
        created_ms: millis(18)?,
        updated_ms: millis(19)?,
        restorable_until_ms: row
            .get::<_, Option<i64>>(20)?
            .map(|value| u64::try_from(value).map_err(|_| invalid(20)))
            .transpose()?,
        pruned: row.get::<_, bool>(21)?,
        landed: row.get(22)?,
        intent,
    })
}

fn path_text(path: &std::path::Path) -> Result<&str, EngineError> {
    path.to_str()
        .ok_or_else(|| failure("changes_git_unsupported"))
}

fn signed(value: u64) -> Result<i64, EngineError> {
    i64::try_from(value).map_err(|_| failure("changes_unavailable"))
}

/// Writes a change set, adding it or replacing every column of the stored one.
/// Inputs: the store and the record. Output: Ok once stored; a second open change set
/// for the same worktree is refused by the store's unique index.
pub(super) fn save(db: &Connection, record: &Record) -> Result<(), EngineError> {
    let intent = record
        .intent
        .as_ref()
        .map(|intent| serde_json::to_string(intent).map_err(storage))
        .transpose()?;
    let conflicts = serde_json::to_string(&record.conflicts).map_err(storage)?;
    db.execute(
        &format!(
            "INSERT OR REPLACE INTO landing_change_sets({COLUMNS}) VALUES \
             (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)"
        ),
        params![
            record.id.as_str(),
            record.project.as_str(),
            kind_name(record.agent.kind),
            record.agent.id,
            record.agent.label,
            path_text(&record.repository)?,
            path_text(&record.worktree)?,
            path_text(&record.git_dir)?,
            record.base,
            record.head,
            state_name(record.state),
            outcome_name(record.last_outcome),
            record.turn,
            record.prompt_line,
            record.files,
            record.added,
            record.removed,
            conflicts,
            signed(record.created_ms)?,
            signed(record.updated_ms)?,
            record.restorable_until_ms.map(signed).transpose()?,
            record.pruned,
            record.landed,
            intent,
        ],
    )
    .map_err(|error| match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::ConstraintViolation) => failure("changes_agent_has_pending"),
        _ => storage(error),
    })?;
    Ok(())
}

/// Reads one change set. Inputs: the store and the id. Output: the record, or
/// `changes_not_found`.
pub(super) fn load(db: &Connection, id: &ChangeSetId) -> Result<Record, EngineError> {
    db.query_row(
        &format!("SELECT {COLUMNS} FROM landing_change_sets WHERE id=?1"),
        [id.as_str()],
        read,
    )
    .optional()
    .map_err(storage)?
    .ok_or_else(|| failure("changes_not_found"))
}

/// Reads the open change set of one worktree. Inputs: the store and the worktree path.
/// Output: the record, or None when nothing is open there.
pub(super) fn open_for(db: &Connection, worktree: &str) -> Result<Option<Record>, EngineError> {
    db.query_row(
        &format!(
            "SELECT {COLUMNS} FROM landing_change_sets WHERE worktree=?1 \
             AND state IN ('pending','conflict','updating')"
        ),
        [worktree],
        read,
    )
    .optional()
    .map_err(storage)
}

/// Reads one project's change sets, open ones first, then newest first.
/// Inputs: the store, the project and the most to read. Output: the records.
pub(super) fn of_project(
    db: &Connection,
    project: &ProjectId,
    limit: usize,
) -> Result<Vec<Record>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM landing_change_sets WHERE project=?1 \
             ORDER BY state IN ('pending','conflict','updating') DESC, updated_ms DESC, id \
             LIMIT ?2"
        ))
        .map_err(storage)?;
    let limit = i64::try_from(limit).map_err(storage)?;
    statement
        .query_map(params![project.as_str(), limit], read)
        .map_err(storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage)
}

/// Reads every change set with owner-checkout or worktree work in flight.
/// Input: the store. Output: the records, oldest first.
pub(super) fn in_flight(db: &Connection) -> Result<Vec<Record>, EngineError> {
    collect(db, "intent IS NOT NULL", rusqlite::params![])
}

/// Reads the discarded change sets whose restore window ended by `now` and whose ref
/// is not deleted yet. Inputs: the store and the time now. Output: the records.
pub(super) fn discarded_due(db: &Connection, now: u64) -> Result<Vec<Record>, EngineError> {
    collect(
        db,
        "state='discarded' AND pruned=0 AND restorable_until_ms<=?1",
        [signed(now)?],
    )
}

fn collect<P: rusqlite::Params>(
    db: &Connection,
    condition: &str,
    values: P,
) -> Result<Vec<Record>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM landing_change_sets WHERE {condition} ORDER BY created_ms, id"
        ))
        .map_err(storage)?;
    statement
        .query_map(values, read)
        .map_err(storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage)
}
