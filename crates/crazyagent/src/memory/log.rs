use crate::engine::EngineError;
use crate::owner_texts::{self, TextDigest};
use crate::project_setup::removed::event as removed;
use crate::protocol::{Command, Outcome, ProjectId, RequestId};
use crate::state_log::replay::{self, Clock, Logged};
use crate::state_log::{Aggregate, Appender, BODY_BYTES_MAX, Chunk, Draft, SNAPSHOT_CHUNK};
#[cfg(any(unix, test))]
use crate::storage::opening::Opening;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Transaction, params, params_from_iter};
use serde::{Deserialize, Serialize};

#[cfg(all(test, unix))]
mod tests;

/// The kind of the event a memory change appends (v2.5 L8 E4).
pub(crate) const KIND: &str = "MemoryChanged";
/// The version of the change body.
const SCHEMA: u16 = 1;
/// The most row keys one request, removal or sweep may touch.
const TOUCHED_MAX: usize = 1 << 20;
/// The most rows one table may bring to a snapshot run.
const SNAPSHOT_ROWS_MAX: usize = 1 << 20;
/// The bytes of a chunk body kept for its envelope (aggregate, table, index) around rows.
const CHUNK_ENVELOPE_BYTES: usize = 1_024;
/// The most project removals one memory replay applies.
const REMOVALS_MAX: usize = 100_000;

/// The 14 tables of the memory aggregate, each with its columns in [`Table::columns`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Table {
    KnowledgePrincipals,
    KnowledgeSources,
    KnowledgeActivity,
    KnowledgeNominations,
    KnowledgeEntries,
    KnowledgeHeads,
    KnowledgeGlobals,
    KnowledgeJobs,
    KnowledgeWatermarks,
    MemoryLibrary,
    MemorySettings,
    MemoryAgentScope,
    AssetSources,
    AssetSnapshots,
}

/// Every memory table, in the order a snapshot run carries them.
pub(crate) const TABLES: [Table; 14] = [
    Table::KnowledgePrincipals,
    Table::KnowledgeSources,
    Table::KnowledgeActivity,
    Table::KnowledgeNominations,
    Table::KnowledgeEntries,
    Table::KnowledgeHeads,
    Table::KnowledgeGlobals,
    Table::KnowledgeJobs,
    Table::KnowledgeWatermarks,
    Table::MemoryLibrary,
    Table::MemorySettings,
    Table::MemoryAgentScope,
    Table::AssetSources,
    Table::AssetSnapshots,
];

/// What one column holds: an integer, an engine id (one that may be absent), or a text the
/// kept-text store holds while the event carries its digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Int,
    Id,
    OptionalId,
    Text,
}

impl Table {
    /// The table's name in the store. Output: the name.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::KnowledgePrincipals => "knowledge_principals",
            Self::KnowledgeSources => "knowledge_sources",
            Self::KnowledgeActivity => "knowledge_activity",
            Self::KnowledgeNominations => "knowledge_nominations",
            Self::KnowledgeEntries => "knowledge_entries",
            Self::KnowledgeHeads => "knowledge_heads",
            Self::KnowledgeGlobals => "knowledge_globals",
            Self::KnowledgeJobs => "knowledge_jobs",
            Self::KnowledgeWatermarks => "knowledge_watermarks",
            Self::MemoryLibrary => "memory_library",
            Self::MemorySettings => "memory_settings",
            Self::MemoryAgentScope => "memory_agent_scope",
            Self::AssetSources => "asset_sources",
            Self::AssetSnapshots => "asset_snapshots",
        }
    }

    /// Every column of the table, its key columns first ([`Table::keys`]), with what each
    /// holds. Output: the columns.
    fn columns(self) -> &'static [(&'static str, Column)] {
        use Column::{Id, Int, OptionalId, Text};
        match self {
            Self::KnowledgePrincipals => &[("id", Id), ("body", Text), ("revoked", Int)],
            Self::KnowledgeSources => &[("project", Id), ("id", Id), ("revoked", Int)],
            Self::KnowledgeActivity => &[
                ("id", Id),
                ("ordinal", Int),
                ("project", Id),
                ("source", Id),
                ("body", Text),
            ],
            Self::KnowledgeNominations => &[("id", Id), ("project", Id), ("body", Text)],
            Self::KnowledgeEntries => &[
                ("id", Id),
                ("version", Int),
                ("project", Id),
                ("body", Text),
            ],
            Self::KnowledgeHeads => &[("id", Id), ("version", Int), ("project", Id)],
            Self::KnowledgeGlobals => &[
                ("id", Id),
                ("entry", Id),
                ("version", Int),
                ("hash", Text),
                ("approved", Int),
                ("body", Text),
            ],
            Self::KnowledgeJobs => &[
                ("id", Id),
                ("project", Id),
                ("destination", Id),
                ("body", Text),
            ],
            Self::KnowledgeWatermarks => &[("project", Id), ("destination", Id), ("ordinal", Int)],
            Self::MemoryLibrary => &[
                ("id", Id),
                ("ordinal", Int),
                ("project", OptionalId),
                ("origin", Text),
                ("text", Text),
            ],
            Self::MemorySettings => &[("singleton", Int), ("agent_writes", Id)],
            Self::MemoryAgentScope => &[("singleton", Int), ("scope", Id)],
            Self::AssetSources => &[
                ("id", Id),
                ("scope", Text),
                ("name", Text),
                ("body", Text),
                ("revoked", Int),
            ],
            Self::AssetSnapshots => &[("id", Id), ("body", Text)],
        }
    }

    /// How many leading columns key one row. Output: 1 or 2.
    fn keys(self) -> usize {
        match self {
            Self::KnowledgeSources | Self::KnowledgeEntries | Self::KnowledgeWatermarks => 2,
            _ => 1,
        }
    }

    /// The table of a stored name. Input: the name. Output: the table, none for another.
    fn named(name: &str) -> Option<Self> {
        TABLES.into_iter().find(|table| table.name() == name)
    }

    /// The table's column names, comma separated. Output: the list.
    fn list(self) -> String {
        let names: Vec<&str> = self.columns().iter().map(|(name, _)| *name).collect();
        names.join(",")
    }
}

/// One cell of a logged row: an integer, an engine id, a kept text's digest, or none.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Cell {
    Null,
    Int(i64),
    Id(RequestId),
    Text(TextDigest),
}

impl Cell {
    /// Whether the cell is one the column can hold. Input: the column. Output: the answer.
    fn fits(&self, column: Column) -> bool {
        matches!(
            (self, column),
            (Self::Int(_), Column::Int)
                | (Self::Id(_), Column::Id | Column::OptionalId)
                | (Self::Null, Column::OptionalId)
                | (Self::Text(_), Column::Text)
        )
    }

    /// The value the cell binds in a statement. Output: the value.
    fn bound(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Int(number) => Value::Integer(*number),
            Self::Id(id) => Value::Text(id.as_str().into()),
            Self::Text(digest) => Value::Text(digest.as_str().into()),
        }
    }
}

/// The key of one row: its table and its key cells.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    table: Table,
    key: Vec<Cell>,
}

impl Key {
    /// Whether the key has its table's key cells, each one its column holds. Output: the
    /// answer.
    fn fits(&self) -> bool {
        let columns = &self.table.columns()[..self.table.keys()];
        self.key.len() == columns.len()
            && self
                .key
                .iter()
                .zip(columns)
                .all(|(cell, (_, column))| *column != Column::Text && cell.fits(*column))
    }

    /// The kept-text holder of one column of this row, `table/key parts/column`, built from
    /// engine ids and names only. Input: the column. Output: the holder key.
    fn holder(&self, column: &str) -> String {
        let parts: Vec<String> = self
            .key
            .iter()
            .map(|cell| match cell {
                Cell::Int(number) => number.to_string(),
                Cell::Id(id) => id.as_str().to_owned(),
                Cell::Null | Cell::Text(_) => unreachable!("a key cell is an integer or an id"),
            })
            .collect();
        format!("{}/{}/{column}", self.table.name(), parts.join("/"))
    }
}

/// One row as an event or a snapshot chunk carries it: its table and every cell in
/// [`Table::columns`] order, texts as their digests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    table: Table,
    cells: Vec<Cell>,
}

impl Row {
    /// Whether the row has every column of its table, each cell one its column holds.
    /// Output: the answer.
    fn fits(&self) -> bool {
        let columns = self.table.columns();
        self.cells.len() == columns.len()
            && self
                .cells
                .iter()
                .zip(columns)
                .all(|(cell, (_, column))| cell.fits(*column))
    }
}

/// What made a memory change: the request's command, an agent's `remember` call, or the
/// open sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Knowledge,
    Library,
    Assets,
    Project,
    AgentRemembered,
    Swept,
}

impl Action {
    /// The action of a request's command. Input: the command. Output: the action.
    pub(crate) fn of(command: &Command) -> Self {
        match command {
            Command::Knowledge { .. } => Self::Knowledge,
            Command::OwnerMemory { .. } => Self::Library,
            Command::Assets { .. } => Self::Assets,
            _ => Self::Project,
        }
    }
}

/// The body of one `MemoryChanged` event: what made it, the rows it deleted by key and the
/// rows it wrote whole. Ids, integers and digests only, so no free text rides in it (L1-N1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Changed {
    action: Action,
    deleted: Vec<Key>,
    put: Vec<Row>,
}

impl Changed {
    /// Whether every key and row fits its table. Output: the answer.
    fn fits(&self) -> bool {
        self.deleted.iter().all(Key::fits) && self.put.iter().all(Row::fits)
    }
}

/// Whether a body decodes as a closed change whose keys and rows fit their tables, as
/// replay takes one (L1-N1). Input: the body. Output: the answer.
#[cfg(test)]
pub(crate) fn closed(body: &[u8]) -> bool {
    serde_json::from_slice::<Changed>(body).is_ok_and(|change| change.fits())
}

/// What a replay rebuilt: the last memory `seq` read and every digest whose text was gone.
#[derive(Debug)]
pub struct Rebuilt {
    pub last: u64,
    pub missing: Vec<TextDigest>,
}

/// An engine error from any failure. Input: the failure. Output: the error.
fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// Creates the touched-key table and, on each memory table, the triggers that note the key
/// of every row an insert, update or delete touches, so each writer's change reaches the
/// log whatever statement made it. A trigger skips a key already noted rather than relying
/// on `OR IGNORE`, since SQLite runs a trigger's statements under the conflict policy of an
/// outer statement that names one, and an upsert's update names ABORT. Input: the
/// connection, with the 14 tables made. Output: Ok, or the store's refusal.
pub fn initialize(db: &Connection) -> Result<(), EngineError> {
    let mut sql = String::from(
        "CREATE TABLE IF NOT EXISTS memory_touched
         (tbl TEXT NOT NULL, k1 NOT NULL, k2 NOT NULL, PRIMARY KEY (tbl, k1, k2)) WITHOUT ROWID;",
    );
    for table in TABLES {
        let name = table.name();
        let columns = table.columns();
        let touch = |row: &str| {
            let first = format!("{row}.{}", columns[0].0);
            let second = if table.keys() == 2 {
                format!("{row}.{}", columns[1].0)
            } else {
                "''".to_owned()
            };
            format!(
                "INSERT INTO memory_touched SELECT '{name}', {first}, {second}
                 WHERE NOT EXISTS (SELECT 1 FROM memory_touched
                     WHERE tbl='{name}' AND k1={first} AND k2={second});"
            )
        };
        sql.push_str(&format!(
            "CREATE TRIGGER IF NOT EXISTS memory_touched_{name}_insert AFTER INSERT ON {name}
             BEGIN {} END;
             CREATE TRIGGER IF NOT EXISTS memory_touched_{name}_update AFTER UPDATE ON {name}
             BEGIN {} {} END;
             CREATE TRIGGER IF NOT EXISTS memory_touched_{name}_delete AFTER DELETE ON {name}
             BEGIN {} END;",
            touch("NEW"),
            touch("OLD"),
            touch("NEW"),
            touch("OLD")
        ));
    }
    db.execute_batch(&sql).map_err(error)
}

/// Runs one memory action in a savepoint: an action that applied keeps its writes, and any
/// other outcome or a refusal rolls back every write it made, its touched keys with them
/// (v2.5 L8 E5). Inputs: the transaction and the action. Output: the action's outcome.
pub(crate) fn atomic(
    tx: &Connection,
    run: impl FnOnce() -> Result<Outcome, EngineError>,
) -> Result<Outcome, EngineError> {
    assert!(!tx.is_autocommit(), "an action runs in its request");
    tx.execute_batch("SAVEPOINT memory_action").map_err(error)?;
    let outcome = run();
    let end = if matches!(outcome, Ok(Outcome::Applied)) {
        "RELEASE memory_action"
    } else {
        "ROLLBACK TO memory_action; RELEASE memory_action"
    };
    tx.execute_batch(end).map_err(error)?;
    outcome
}

/// Closes one request's memory change, before the request persists its own event. Rows a
/// removal deleted are settled, since replay deletes them at the removal's own event (R1).
/// Every other touched row is logged at the request's next revision, moving `state` from
/// one before it as `record_at` does. A touched row no event can carry here stays touched,
/// in a request that applied nothing, one whose revision another aggregate logged, or a row
/// a removal left standing: the next request that logs, or the open sweep, logs it as it
/// then stands. Inputs: the transaction, the action, the next revision when the request
/// applied and the removed project. Output: whether a change was logged, or
/// `state_log_revision_stale` or the fence's or the log's refusal.
pub(crate) fn close(
    tx: &Transaction<'_>,
    action: Action,
    next: Option<u64>,
    removal: Option<&ProjectId>,
) -> Result<bool, EngineError> {
    if !pending(tx)? {
        return Ok(false);
    }
    if removal.is_some() {
        settle(tx)?;
        return Ok(false);
    }
    let Some(next) = next else {
        return Ok(false);
    };
    let stale = || error("state_log_revision_stale");
    let revision: i64 = tx
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .map_err(error)?;
    let revision = u64::try_from(revision).map_err(error)?;
    if revision == next {
        return Ok(false);
    }
    if revision.checked_add(1) != Some(next) {
        return Err(stale());
    }
    tx.execute(
        "UPDATE state SET revision=?1 WHERE id=1",
        [i64::try_from(next).map_err(error)?],
    )
    .map_err(error)?;
    let fence = crate::worktrees::log::store_fence(tx)?;
    let mut appender = Appender::owner(tx, next, fence)?;
    let changes = drain(tx, action)?;
    append(&mut appender, &changes, Clock::System.now_ms())?;
    assert_eq!(appender.revision(), next);
    assert!(!pending(tx)?, "the request's change is logged");
    Ok(true)
}

/// Closes an owner or client request's memory change through [`close`]: its action comes
/// from the command, and an applied Remove from dashboard or Delete everything settles as a
/// removal. Inputs: the transaction, the command, the next revision when the request
/// applied and whether it carries Delete everything's effect. Output: as [`close`].
pub(crate) fn close_request(
    tx: &Transaction<'_>,
    command: &Command,
    next: Option<u64>,
    deleting: bool,
) -> Result<bool, EngineError> {
    let removal = next.and_then(|_| removed::removal(command, deleting));
    let project = removal.as_ref().map(|removed| &removed.project);
    close(tx, Action::of(command), next, project)
}

/// Logs the memory change of an engine writer in its own transaction, as an agent's
/// `remember` call: `own` moves the `state` revision once behind the store's engine host and
/// writes its `events` row. Inputs: the transaction and the action. Output: the revision it
/// moved to, or the fence's or the log's refusal.
pub(crate) fn record_own(tx: &Transaction<'_>, action: Action) -> Result<u64, EngineError> {
    let mut appender = Appender::own(tx, crate::worktrees::log::store_fence(tx)?)?;
    let changes = drain(tx, action)?;
    assert!(!changes.is_empty(), "an own write logs its change");
    append(&mut appender, &changes, Clock::System.now_ms())?;
    Ok(appender.revision())
}

/// Deletes a removed project's memory, live in the removal's request and in replay at the
/// removal's own event: its knowledge rows and the app-wide candidates made from them, its
/// agents' saved memories, and, when its notes are not kept, every memory of it (R1).
/// Inputs: the transaction, the project and whether its notes are kept. Output: Ok, or the
/// store's refusal.
pub(crate) fn forget_removed(
    tx: &Transaction<'_>,
    project: &ProjectId,
    kept: bool,
) -> Result<(), EngineError> {
    super::forget_project(tx, project)?;
    super::library::forget_agent_memories(tx, project)?;
    if !kept {
        super::library::forget_project_memories(tx, project)?;
    }
    Ok(())
}

/// Whether a memory row was touched and not yet logged. Input: the connection. Output: the
/// answer.
fn pending(db: &Connection) -> Result<bool, EngineError> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM memory_touched)", [], |row| {
        row.get(0)
    })
    .map_err(error)
}

/// Releases the texts of the rows a removal deleted and forgets their touched keys; a
/// touched row still standing stays touched for the next change that logs. Input: the
/// transaction. Output: Ok, or the store's refusal.
fn settle(tx: &Transaction<'_>) -> Result<(), EngineError> {
    for key in touched(tx)? {
        if live(tx, &key)?.is_some() {
            continue;
        }
        release(tx, &key)?;
        let second = match key.key.get(1) {
            Some(cell) => cell.bound(),
            None => Value::Text(String::new()),
        };
        let forgotten = tx
            .execute(
                "DELETE FROM memory_touched WHERE tbl=?1 AND k1=?2 AND k2=?3",
                params![key.table.name(), key.key[0].bound(), second],
            )
            .map_err(error)?;
        assert_eq!(forgotten, 1, "a settled key was touched");
    }
    Ok(())
}

/// The cell of a stored value in a column that holds no text. Inputs: the column and the
/// value. Output: the cell, or `memory_log_cell_invalid`.
fn plain(column: Column, value: Value) -> Result<Cell, EngineError> {
    let invalid = || error("memory_log_cell_invalid");
    match (column, value) {
        (Column::Int, Value::Integer(number)) => Ok(Cell::Int(number)),
        (Column::Id | Column::OptionalId, Value::Text(text)) => {
            RequestId::new(text).map(Cell::Id).map_err(|_| invalid())
        }
        (Column::OptionalId, Value::Null) => Ok(Cell::Null),
        _ => Err(invalid()),
    }
}

/// Every touched key, in key order. Input: the connection. Output: the keys, or
/// `memory_log_change_too_large` past [`TOUCHED_MAX`] or `memory_log_cell_invalid`.
fn touched(db: &Connection) -> Result<Vec<Key>, EngineError> {
    let mut statement = db
        .prepare("SELECT tbl, k1, k2 FROM memory_touched ORDER BY tbl, k1, k2 LIMIT ?1")
        .map_err(error)?;
    let limit = i64::try_from(TOUCHED_MAX + 1).map_err(error)?;
    let rows = statement
        .query_map([limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Value>(1)?,
                row.get::<_, Value>(2)?,
            ))
        })
        .map_err(error)?;
    let mut keys = Vec::new();
    for row in rows {
        let (name, first, second) = row.map_err(error)?;
        let table = Table::named(&name).ok_or_else(|| error("memory_log_cell_invalid"))?;
        let columns = table.columns();
        let mut key = vec![plain(columns[0].1, first)?];
        if table.keys() == 2 {
            key.push(plain(columns[1].1, second)?);
        }
        keys.push(Key { table, key });
    }
    if keys.len() > TOUCHED_MAX {
        return Err(error("memory_log_change_too_large"));
    }
    assert!(keys.iter().all(Key::fits), "a touched key fits its table");
    Ok(keys)
}

/// The row a key names as it stands. Inputs: the connection and the key. Output: its
/// values in [`Table::columns`] order, none once it is deleted.
fn live(db: &Connection, key: &Key) -> Result<Option<Vec<Value>>, EngineError> {
    let table = key.table;
    let filter: Vec<String> = table.columns()[..table.keys()]
        .iter()
        .enumerate()
        .map(|(index, (name, _))| format!("{name}=?{}", index + 1))
        .collect();
    let width = table.columns().len();
    db.query_row(
        &format!(
            "SELECT {} FROM {} WHERE {}",
            table.list(),
            table.name(),
            filter.join(" AND ")
        ),
        params_from_iter(key.key.iter().map(Cell::bound)),
        |row| (0..width).map(|index| row.get::<_, Value>(index)).collect(),
    )
    .optional()
    .map_err(error)
}

/// One stored row as an event carries it: each text kept for its holder, under the row's
/// project when it has one, so a project's clear takes it. Inputs: the live writer's
/// transaction, the table and the row's values. Output: the row, or
/// `memory_log_cell_invalid` or the store's refusal.
fn kept(tx: &Connection, table: Table, values: Vec<Value>) -> Result<Row, EngineError> {
    let columns = table.columns();
    assert_eq!(values.len(), columns.len(), "a row has every column");
    let project = columns
        .iter()
        .zip(&values)
        .find_map(|((name, _), value)| match (*name, value) {
            ("project", Value::Text(text)) => ProjectId::new(text.as_str()).ok(),
            _ => None,
        });
    let mut key = Key {
        table,
        key: Vec::new(),
    };
    for ((_, column), value) in columns[..table.keys()].iter().zip(&values) {
        key.key.push(plain(*column, value.clone())?);
    }
    let mut cells = Vec::with_capacity(columns.len());
    for ((name, column), value) in columns.iter().zip(values) {
        cells.push(match (column, value) {
            (Column::Text, Value::Text(text)) => Cell::Text(owner_texts::keep(
                tx,
                &key.holder(name),
                project.as_ref(),
                text.as_bytes(),
            )?),
            (column, value) => plain(*column, value)?,
        });
    }
    let row = Row { table, cells };
    assert!(row.fits(), "a kept row fits its table");
    Ok(row)
}

/// Releases the texts a deleted row held. Inputs: the live writer's transaction and the
/// row's key. Output: Ok, or the store's refusal.
fn release(tx: &Connection, key: &Key) -> Result<(), EngineError> {
    for (name, column) in key.table.columns() {
        if *column == Column::Text {
            owner_texts::release(tx, &key.holder(name))?;
        }
    }
    Ok(())
}

/// Reads every touched key back as a deleted key or a kept row, forgets the touched keys,
/// and packs the change into bodies that each fit one event. Inputs: the live writer's
/// transaction and the action. Output: the bodies, none when nothing was touched, or
/// `memory_log_row_too_large` or the store's refusal.
fn drain(tx: &Connection, action: Action) -> Result<Vec<Changed>, EngineError> {
    let empty = |action| Changed {
        action,
        deleted: Vec::new(),
        put: Vec::new(),
    };
    let budget = BODY_BYTES_MAX - serde_json::to_vec(&empty(action)).map_err(error)?.len();
    let mut changes = Vec::new();
    let mut current = empty(action);
    let mut used = 0;
    for key in touched(tx)? {
        let row = match live(tx, &key)? {
            Some(values) => Some(kept(tx, key.table, values)?),
            None => {
                release(tx, &key)?;
                None
            }
        };
        let bytes = match &row {
            Some(row) => serde_json::to_vec(row),
            None => serde_json::to_vec(&key),
        }
        .map_err(error)?
        .len()
            + 1;
        if bytes > budget {
            return Err(error("memory_log_row_too_large"));
        }
        if used + bytes > budget {
            changes.push(std::mem::replace(&mut current, empty(action)));
            used = 0;
        }
        used += bytes;
        match row {
            Some(row) => current.put.push(row),
            None => current.deleted.push(key),
        }
    }
    if !current.deleted.is_empty() || !current.put.is_empty() {
        changes.push(current);
    }
    tx.execute("DELETE FROM memory_touched", [])
        .map_err(error)?;
    assert!(changes.iter().all(Changed::fits), "every change fits");
    Ok(changes)
}

/// Appends each change body as one `MemoryChanged` event. Inputs: the appender, the bodies
/// and the writer's clock. Output: the last `seq`, or the log's refusal.
fn append(
    appender: &mut Appender<'_, '_>,
    changes: &[Changed],
    at_ms: u64,
) -> Result<u64, EngineError> {
    let mut last = 0;
    for change in changes {
        let body = serde_json::to_vec(change).map_err(error)?;
        assert!(body.len() <= BODY_BYTES_MAX, "a change body fits one event");
        last = appender.append(&Draft {
            aggregate: Aggregate::Memory,
            kind: KIND,
            schema: SCHEMA,
            at_ms,
            body: &body,
        })?;
    }
    Ok(last)
}

/// Decodes one logged memory change. Input: the event. Output: the change, or
/// `memory_log_event_invalid` for another aggregate, kind or schema, or a body that is not
/// a closed change whose keys and rows fit their tables.
fn decode(logged: &Logged) -> Result<Changed, EngineError> {
    let invalid = || error("memory_log_event_invalid");
    if logged.aggregate != Aggregate::Memory || logged.schema != SCHEMA || logged.kind != KIND {
        return Err(invalid());
    }
    let change: Changed = serde_json::from_slice(&logged.body).map_err(|_| invalid())?;
    if !change.fits() {
        return Err(invalid());
    }
    Ok(change)
}

/// Writes one logged row whole in replay, each text read back from the kept-text store. A
/// row with a text no longer kept is skipped and its digest named, since a later event of
/// that row replaced or deleted it. Inputs: the transaction, the row and the missing list.
/// Output: Ok, or `memory_log_text_invalid` or the store's refusal.
fn put(db: &Connection, row: &Row, missing: &mut Vec<TextDigest>) -> Result<(), EngineError> {
    let mut values = Vec::with_capacity(row.cells.len());
    for cell in &row.cells {
        let Cell::Text(digest) = cell else {
            values.push(cell.bound());
            continue;
        };
        let before = missing.len();
        let bytes = owner_texts::replayed(db, digest, missing)?;
        if missing.len() > before {
            return Ok(());
        }
        let text = String::from_utf8(bytes).map_err(|_| error("memory_log_text_invalid"))?;
        values.push(Value::Text(text));
    }
    let marks: Vec<String> = (1..=values.len())
        .map(|index| format!("?{index}"))
        .collect();
    db.execute(
        &format!(
            "INSERT OR REPLACE INTO {}({}) VALUES ({})",
            row.table.name(),
            row.table.list(),
            marks.join(",")
        ),
        params_from_iter(values),
    )
    .map_err(error)?;
    Ok(())
}

/// Applies one logged change in replay: its deleted keys, then its rows. Inputs: the
/// transaction, the change and the missing list. Output: Ok, or the store's refusal.
fn apply(
    db: &Connection,
    change: &Changed,
    missing: &mut Vec<TextDigest>,
) -> Result<(), EngineError> {
    for key in &change.deleted {
        let filter: Vec<String> = key.table.columns()[..key.table.keys()]
            .iter()
            .enumerate()
            .map(|(index, (name, _))| format!("{name}=?{}", index + 1))
            .collect();
        db.execute(
            &format!(
                "DELETE FROM {} WHERE {}",
                key.table.name(),
                filter.join(" AND ")
            ),
            params_from_iter(key.key.iter().map(Cell::bound)),
        )
        .map_err(error)?;
    }
    for row in &change.put {
        put(db, row, missing)?;
    }
    Ok(())
}

/// Inserts one snapshot chunk's rows into its table. Inputs: the transaction, the chunk and
/// the missing list. Output: Ok, or `memory_log_chunk_invalid` for another table or a row
/// that does not fit it.
fn restore(
    db: &Connection,
    chunk: &Chunk,
    missing: &mut Vec<TextDigest>,
) -> Result<(), EngineError> {
    let invalid = || error("memory_log_chunk_invalid");
    let table = Table::named(&chunk.table).ok_or_else(invalid)?;
    for value in &chunk.rows {
        let cells: Vec<Cell> = serde_json::from_value(value.clone()).map_err(|_| invalid())?;
        let row = Row { table, cells };
        if !row.fits() {
            return Err(invalid());
        }
        put(db, &row, missing)?;
    }
    Ok(())
}

/// Deletes every row of the 14 tables. Input: the connection. Output: Ok, or the store's
/// refusal.
fn clear(db: &Connection) -> Result<(), EngineError> {
    for table in TABLES {
        db.execute(&format!("DELETE FROM {}", table.name()), [])
            .map_err(error)?;
    }
    Ok(())
}

/// Each logged project removal with its `seq`, in log order. Input: the connection.
/// Output: the removals, or `memory_log_removals` past [`REMOVALS_MAX`] or
/// `memory_log_event_invalid` for a removal body that is not one.
fn removals(db: &Connection) -> Result<Vec<(u64, removed::ProjectRemoved)>, EngineError> {
    let mut found = Vec::new();
    replay::replay(db, Aggregate::Projects, 0, |logged| {
        if logged.kind != removed::KIND {
            return Ok(());
        }
        if found.len() == REMOVALS_MAX {
            return Err(error("memory_log_removals"));
        }
        let removal =
            serde_json::from_slice(&logged.body).map_err(|_| error("memory_log_event_invalid"))?;
        found.push((logged.seq, removal));
        Ok(())
    })?;
    assert!(found.len() <= REMOVALS_MAX);
    Ok(found)
}

/// Rebuilds the 14 memory tables from the log: clears them, then replays every memory event
/// in order, a project removal deleting its project's memory at its place in the log (R1).
/// A snapshot run holds every row, so its first chunk clears the tables again. Texts come
/// from the kept-text store and are never kept or released here. Input: the transaction.
/// Output: the last memory `seq` read and the digests whose text was gone, or the first
/// refusal.
pub fn rebuild(tx: &Transaction<'_>) -> Result<Rebuilt, EngineError> {
    let mut removals = removals(tx)?.into_iter().peekable();
    clear(tx)?;
    let mut missing = Vec::new();
    let mut chunk_seq = 0;
    let last = replay::replay(tx, Aggregate::Memory, 0, |logged| {
        while let Some((_, removal)) = removals.next_if(|(seq, _)| *seq < logged.seq) {
            forget_removed(tx, &removal.project, removal.kept)?;
        }
        if logged.kind != SNAPSHOT_CHUNK {
            return apply(tx, &decode(logged)?, &mut missing);
        }
        if chunk_seq == 0 || logged.seq != chunk_seq + 1 {
            clear(tx)?;
        }
        chunk_seq = logged.seq;
        restore(tx, &replay::chunk(logged)?, &mut missing)
    })?;
    for (_, removal) in removals {
        forget_removed(tx, &removal.project, removal.kept)?;
    }
    tx.execute("DELETE FROM memory_touched", [])
        .map_err(error)?;
    assert!(
        last >= chunk_seq,
        "replay reads past every chunk it applied"
    );
    Ok(Rebuilt { last, missing })
}

/// Every row of one table in key order, each kept as an event carries it. Inputs: the live
/// writer's transaction and the table. Output: the rows as JSON cell lists, or
/// `memory_log_snapshot_rows` past [`SNAPSHOT_ROWS_MAX`].
fn table_rows(tx: &Connection, table: Table) -> Result<Vec<serde_json::Value>, EngineError> {
    let width = table.columns().len();
    let order: Vec<&str> = table.columns()[..table.keys()]
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let mut statement = tx
        .prepare(&format!(
            "SELECT {} FROM {} ORDER BY {} LIMIT ?1",
            table.list(),
            table.name(),
            order.join(",")
        ))
        .map_err(error)?;
    let limit = i64::try_from(SNAPSHOT_ROWS_MAX + 1).map_err(error)?;
    let stored = statement
        .query_map([limit], |row| {
            (0..width)
                .map(|index| row.get::<_, Value>(index))
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(error)?;
    let mut rows = Vec::new();
    for values in stored {
        let row = kept(tx, table, values.map_err(error)?)?;
        rows.push(serde_json::to_value(&row.cells).map_err(error)?);
    }
    if rows.len() > SNAPSHOT_ROWS_MAX {
        return Err(error("memory_log_snapshot_rows"));
    }
    Ok(rows)
}

/// Splits one table's rows into chunks that each fit one event body, at least one chunk.
/// Inputs: the table and its rows. Output: the chunks, or `memory_log_row_too_large`.
fn pack(table: Table, rows: Vec<serde_json::Value>) -> Result<Vec<Chunk>, EngineError> {
    let budget = BODY_BYTES_MAX - CHUNK_ENVELOPE_BYTES;
    let total = rows.len();
    let fresh = || Chunk {
        table: table.name().into(),
        rows: Vec::new(),
    };
    let mut chunks = vec![fresh()];
    let mut used = 0;
    for row in rows {
        let bytes = serde_json::to_vec(&row).map_err(error)?.len() + 1;
        if bytes > budget {
            return Err(error("memory_log_row_too_large"));
        }
        if used + bytes > budget {
            chunks.push(fresh());
            used = 0;
        }
        used += bytes;
        if let Some(chunk) = chunks.last_mut() {
            chunk.rows.push(row);
        }
    }
    assert_eq!(
        chunks.iter().map(|chunk| chunk.rows.len()).sum::<usize>(),
        total
    );
    Ok(chunks)
}

/// Appends a snapshot run of the 14 tables as they stand, each in chunks that fit one event
/// body and at least one chunk each, every text kept for its holder. Inputs: the appender
/// and the writer's clock. Output: the `seq` of the run's closing event, or the log's
/// refusal.
pub fn snapshot(appender: &mut Appender<'_, '_>, at_ms: u64) -> Result<u64, EngineError> {
    let mut chunks = Vec::new();
    for table in TABLES {
        let rows = appender.project(|tx| table_rows(tx, table))?;
        chunks.extend(pack(table, rows)?);
    }
    assert!(chunks.len() >= TABLES.len(), "one chunk or more per table");
    appender.snapshot(Aggregate::Memory, at_ms, chunks)
}

/// The open-time sweep, run once every memory table exists: makes the touched-key triggers
/// ([`initialize`]), forgets the agent memories of projects the engine no longer
/// records, and logs every touched row through the open's proof at one revision, a row an
/// earlier writer left unlogged included. When nothing is touched no appender is built, so
/// a plain start moves no revision. Inputs: the connection, outside any transaction, and the
/// open's proof. Output: whether a change was logged, or the first refusal, which rolls the
/// whole sweep back.
#[cfg(unix)]
pub(crate) fn sweep(db: &Connection, opening: Opening) -> Result<bool, EngineError> {
    assert!(db.is_autocommit(), "the sweep owns its transaction");
    initialize(db)?;
    let tx =
        Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate).map_err(error)?;
    let forgotten = super::library::forget_orphan_agent_memories(&tx)?;
    let changed = pending(&tx)?;
    if changed {
        let at_ms = opening.at_ms();
        let mut appender = Appender::opening(&tx, opening)?;
        let changes = drain(&tx, Action::Swept)?;
        append(&mut appender, &changes, at_ms)?;
    }
    tx.commit().map_err(error)?;
    if forgotten > 0 {
        eprintln!("memory_orphan_agent_rows_removed count={forgotten}");
    }
    assert!(db.is_autocommit(), "the sweep committed");
    Ok(changed)
}

/// Whether the log already holds a memory event, so the store's memory reached it as it
/// was written. Input: the transaction. Output: the answer, or the store's refusal.
#[cfg(any(unix, test))]
fn on_the_log(tx: &Transaction<'_>) -> Result<bool, EngineError> {
    let log: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='state_log')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !log {
        return Ok(false);
    }
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM state_log WHERE aggregate=?1)",
        [Aggregate::Memory.name()],
        |row| row.get(0),
    )
    .map_err(error)
}

/// The first snapshot run of the 14 tables, for the schema step that puts memory on the
/// log: rows written before the log reach replay through it, through the open's proof. A
/// store without the `state` table has no log to write. The 14 tables are made first, as
/// the settings log's first run does, because the open makes them only after the steps and
/// a store from an older release can lack one (v2.2.0 had no `memory_agent_scope`). One
/// with no memory row needs no run, and one whose memory is already on the log needs none
/// again, as when an upgrade from a reset version runs the step over it; none of them moves
/// a revision. Inputs: the step's transaction and the open's proof. Output: whether a run
/// was appended, or the log's refusal.
#[cfg(any(unix, test))]
pub(crate) fn first_run(tx: &Transaction<'_>, opening: Opening) -> Result<bool, EngineError> {
    let stated: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='state')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if !stated {
        return Ok(false);
    }
    crate::memory::initialize(tx)?;
    crate::assets::initialize(tx)?;
    let counts: Vec<String> = TABLES
        .iter()
        .map(|table| format!("(SELECT COUNT(*) FROM {})", table.name()))
        .collect();
    let rows: i64 = tx
        .query_row(&format!("SELECT {}", counts.join(" + ")), [], |row| {
            row.get(0)
        })
        .map_err(error)?;
    if rows == 0 || on_the_log(tx)? {
        return Ok(false);
    }
    let at_ms = opening.at_ms();
    let mut appender = Appender::opening(tx, opening)?;
    snapshot(&mut appender, at_ms)?;
    let touched: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='memory_touched')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if touched {
        tx.execute("DELETE FROM memory_touched", [])
            .map_err(error)?;
    }
    Ok(true)
}

/// Gives a test store the memory log: the `state` and `events` tables, the 14 memory
/// tables, the touched-key triggers, and the owner tables with the test host started, so
/// writes through the engine's own fence log as they do in a running engine. Input: the
/// connection. Output: none; panics on a store failure.
#[cfg(all(test, unix))]
pub(crate) fn seed(db: &Connection) {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS state
            (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS events (cursor INTEGER PRIMARY KEY, body TEXT NOT NULL);
        INSERT OR IGNORE INTO state VALUES (1,0);",
    )
    .unwrap();
    super::initialize(db).unwrap();
    crate::assets::initialize(db).unwrap();
    initialize(db).unwrap();
    crate::state_log::fence::seed(db);
}
