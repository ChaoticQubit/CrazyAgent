use crate::engine::EngineError;
use crate::state_log::replay::{self, Logged};
use crate::state_log::{Aggregate, Appender, BODY_BYTES_MAX, Chunk, Draft, SNAPSHOT_CHUNK};
use crate::storage::opening::Opening;
use rusqlite::{Connection, Transaction, params, params_from_iter};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
mod tests;

/// The kind of the event a repository admission appends (v2.5 L6 Q5).
pub const KIND_ADMITTED: &str = "RepositoryAdmitted";
/// The kind of the event a worktree reservation appends.
pub const KIND_RESERVED: &str = "WorktreeReserved";
/// The kind of the event a lease body save appends.
pub const KIND_SAVED: &str = "LeaseSaved";
/// The schema every worktrees event is written and read at.
const SCHEMA: u16 = 1;
/// The admissions table, first in a snapshot run, and its columns in key order.
const ADMISSIONS: (&str, &str) = ("repository_admissions", "project,repository,body");
/// The leases table, second in a snapshot run, and its columns in key order.
const LEASES: (&str, &str) = ("worktree_leases", "task,repository,path,body");
/// The most rows one table may bring to a snapshot run; leases stop at 128 by
/// `lease_capacity`, admissions at the projects' repositories.
const SNAPSHOT_ROWS_MAX: usize = 65_536;
/// The bytes of a chunk body kept for its envelope (aggregate, table, index) around rows.
const CHUNK_ENVELOPE_BYTES: usize = 1_024;

/// A repository admitted to a project: its `repository_admissions` row, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admitted {
    pub project: String,
    pub repository: String,
    pub body: String,
}

/// A worktree reserved for a task: its new `worktree_leases` row, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reserved {
    pub task: String,
    pub repository: String,
    pub path: String,
    pub body: String,
}

/// A lease's body saved: the key of its `worktree_leases` row and the new body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    pub task: String,
    pub repository: String,
    pub body: String,
}

/// One worktrees event. Each carries the stored column text of the row it writes, which
/// holds ids, paths, a branch, a commit and file identities, never a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Admitted(Admitted),
    Reserved(Reserved),
    Saved(Saved),
}

impl Event {
    /// The event's kind in the log. Output: the kind.
    fn kind(&self) -> &'static str {
        match self {
            Self::Admitted(_) => KIND_ADMITTED,
            Self::Reserved(_) => KIND_RESERVED,
            Self::Saved(_) => KIND_SAVED,
        }
    }

    /// The event's body in the log, its fields as JSON. Output: the bytes.
    fn body(&self) -> Result<Vec<u8>, EngineError> {
        match self {
            Self::Admitted(event) => serde_json::to_vec(event),
            Self::Reserved(event) => serde_json::to_vec(event),
            Self::Saved(event) => serde_json::to_vec(event),
        }
        .map_err(error)
    }
}

/// Writes one worktrees event: the projection applies it in the appender's transaction,
/// then the log appends it, so an event is logged only when its statement applied. A
/// refusal leaves the transaction for the caller to roll back. Inputs: the appender, the
/// writer's clock and the event. Output: the event's `seq`, or the first refusal.
pub fn record(
    appender: &mut Appender<'_, '_>,
    at_ms: u64,
    event: &Event,
) -> Result<u64, EngineError> {
    let body = event.body()?;
    appender.project(|tx| apply(tx, event))?;
    let seq = appender.append(&Draft {
        aggregate: Aggregate::Worktrees,
        kind: event.kind(),
        schema: SCHEMA,
        at_ms,
        body: &body,
    })?;
    assert!(seq > 0, "a logged event has a seq");
    assert!(
        body.len() <= BODY_BYTES_MAX,
        "the log bounds what it appended"
    );
    Ok(seq)
}

/// Applies one worktrees event to its table, for the live writers and replay alike. A save
/// needs its lease; an admission or a lease must be new. Inputs: the connection and the
/// event. Output: Ok, or `worktrees_log_admission_exists`, `worktrees_log_lease_exists` or
/// `worktrees_log_lease_missing`.
pub fn apply(db: &Connection, event: &Event) -> Result<(), EngineError> {
    let (count, refused) = match event {
        Event::Admitted(row) => (
            db.execute(
                "INSERT OR IGNORE INTO repository_admissions VALUES (?1,?2,?3)",
                params![row.project, row.repository, row.body],
            ),
            "worktrees_log_admission_exists",
        ),
        Event::Reserved(row) => (
            db.execute(
                "INSERT OR IGNORE INTO worktree_leases VALUES (?1,?2,?3,?4)",
                params![row.task, row.repository, row.path, row.body],
            ),
            "worktrees_log_lease_exists",
        ),
        Event::Saved(row) => (
            db.execute(
                "UPDATE worktree_leases SET body=?3 WHERE task=?1 AND repository=?2",
                params![row.task, row.repository, row.body],
            ),
            "worktrees_log_lease_missing",
        ),
    };
    let count = count.map_err(error)?;
    if count == 0 {
        return Err(error(refused));
    }
    assert_eq!(count, 1, "a worktrees event writes one keyed row");
    Ok(())
}

/// Reads one logged worktrees event: one of the three kinds at schema 1, its body exactly
/// that kind's fields. Input: the logged event. Output: the event, or
/// `worktrees_log_event_invalid`.
pub fn decode(logged: &Logged) -> Result<Event, EngineError> {
    let invalid = || error("worktrees_log_event_invalid");
    if logged.aggregate != Aggregate::Worktrees || logged.schema != SCHEMA {
        return Err(invalid());
    }
    let body = logged.body.as_slice();
    let event = match logged.kind.as_str() {
        KIND_ADMITTED => serde_json::from_slice(body).map(Event::Admitted),
        KIND_RESERVED => serde_json::from_slice(body).map(Event::Reserved),
        KIND_SAVED => serde_json::from_slice(body).map(Event::Saved),
        _ => return Err(invalid()),
    }
    .map_err(|_| invalid())?;
    assert_eq!(event.kind(), logged.kind);
    Ok(event)
}

/// Rebuilds both worktree tables from the log: clears them, then replays every worktrees
/// event in order. A snapshot run holds every row of both, so its first chunk clears them
/// again; a chunk opens a run unless it follows another chunk at the previous `seq`, since
/// a run's chunks are appended back to back and its closing event follows them. Input: the
/// transaction. Output: the last `seq` read, or the first refusal.
pub fn rebuild(tx: &Transaction<'_>) -> Result<u64, EngineError> {
    clear(tx)?;
    let mut chunk_seq = 0;
    let last = replay::replay(tx, Aggregate::Worktrees, 0, |logged| {
        if logged.kind != SNAPSHOT_CHUNK {
            return apply(tx, &decode(logged)?);
        }
        if chunk_seq == 0 || logged.seq != chunk_seq + 1 {
            clear(tx)?;
        }
        chunk_seq = logged.seq;
        restore(tx, &replay::chunk(logged)?)
    })?;
    assert!(
        last >= chunk_seq,
        "replay reads past every chunk it applied"
    );
    Ok(last)
}

/// Appends a snapshot run of both worktree tables as they stand, each in chunks that fit
/// one event body and at least one chunk each, so replay of a run always clears and fills
/// both. Inputs: the appender and the writer's clock. Output: the `seq` of the run's
/// closing event, or `worktrees_log_snapshot_rows`, `worktrees_log_row_too_large` or the
/// log's refusal.
pub fn snapshot(appender: &mut Appender<'_, '_>, at_ms: u64) -> Result<u64, EngineError> {
    let mut chunks = Vec::new();
    for (table, columns) in [ADMISSIONS, LEASES] {
        let rows = appender.project(|tx| table_rows(tx, table, columns))?;
        chunks.extend(pack(table, rows)?);
    }
    assert!(chunks.len() >= 2, "one chunk or more per table");
    assert_eq!(chunks[0].table, ADMISSIONS.0);
    appender.snapshot(Aggregate::Worktrees, at_ms, chunks)
}

/// The open-time sweep of both worktree tables in one transaction: every lease is settled
/// (`reconcile`) and the admissions of removed projects are forgotten. When a row changed,
/// one snapshot run of both tables follows through the open's proof, since the log has no
/// delete kind and a lease a stop left Reserved may have no event (v2.5 L6 Q8). When
/// nothing changed no appender is built, so a plain start moves no revision. Inputs: the
/// connection, outside any transaction, and the open's proof. Output: whether a run was
/// appended, or the first refusal, which rolls the whole sweep back.
#[cfg(unix)]
pub(crate) fn sweep(db: &Connection, opening: Opening) -> Result<bool, EngineError> {
    assert!(db.is_autocommit(), "the sweep owns its transaction");
    let tx =
        Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate).map_err(error)?;
    let before = live(&tx)?;
    super::reconcile(&tx)?;
    crate::project_setup::forget_removed(&tx, &[ADMISSIONS.0])?;
    let changed = live(&tx)? != before;
    if changed {
        let at_ms = opening.at_ms();
        let mut appender = Appender::opening(&tx, opening)?;
        snapshot(&mut appender, at_ms)?;
    }
    tx.commit().map_err(error)?;
    assert!(db.is_autocommit(), "the sweep committed");
    Ok(changed)
}

/// The first snapshot run of both worktree tables, for the schema step that puts worktrees
/// on the log (step 12): rows written before the log reach replay through it, through the
/// open's proof. A store without the `state` table or the worktree tables has no log to
/// write, and one with no worktree row needs no run, so it moves no revision. Inputs: the
/// step's transaction and the open's proof. Output: whether a run was appended, or the
/// log's refusal.
pub(crate) fn first_run(tx: &Transaction<'_>, opening: Opening) -> Result<bool, EngineError> {
    let present: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table'
             AND name IN ('state','repository_admissions','worktree_leases')",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if present < 3 {
        return Ok(false);
    }
    let rows: i64 = tx
        .query_row(
            "SELECT (SELECT COUNT(*) FROM repository_admissions)
             + (SELECT COUNT(*) FROM worktree_leases)",
            [],
            |row| row.get(0),
        )
        .map_err(error)?;
    if rows == 0 {
        return Ok(false);
    }
    let at_ms = opening.at_ms();
    let mut appender = Appender::opening(tx, opening)?;
    let done = snapshot(&mut appender, at_ms)?;
    assert!(done > 0, "a run closes at a seq");
    assert_eq!(present, 3);
    Ok(true)
}

/// Both worktree tables as they stand, each ordered by all its columns. Input: the
/// connection. Output: the admissions and the leases.
#[cfg(unix)]
fn live(db: &Connection) -> Result<[Vec<Vec<rusqlite::types::Value>>; 2], EngineError> {
    Ok([replay::rows(db, ADMISSIONS.0)?, replay::rows(db, LEASES.0)?])
}

/// Every row of one worktree table in key order, each as a JSON array of its column text.
/// Inputs: the connection, the table and its columns. Output: the rows, or
/// `worktrees_log_snapshot_rows` past [`SNAPSHOT_ROWS_MAX`].
fn table_rows(db: &Connection, table: &str, columns: &str) -> Result<Vec<Value>, EngineError> {
    let width = columns.split(',').count();
    let mut statement = db
        .prepare(&format!(
            "SELECT {columns} FROM {table} ORDER BY {columns} LIMIT ?1"
        ))
        .map_err(error)?;
    let limit = i64::try_from(SNAPSHOT_ROWS_MAX + 1).map_err(error)?;
    let rows = statement
        .query_map([limit], |row| {
            (0..width)
                .map(|column| row.get::<_, String>(column).map(Value::String))
                .collect::<rusqlite::Result<Vec<_>>>()
                .map(Value::Array)
        })
        .map_err(error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(error)?;
    if rows.len() > SNAPSHOT_ROWS_MAX {
        return Err(error("worktrees_log_snapshot_rows"));
    }
    assert!(width == 3 || width == 4);
    assert!(
        rows.iter()
            .all(|row| row.as_array().is_some_and(|row| row.len() == width))
    );
    Ok(rows)
}

/// Splits one table's rows, in order, into chunks whose rows fit one event body with room
/// for the chunk's envelope; a table with no rows is one empty chunk. Inputs: the table and
/// its rows. Output: the chunks, or `worktrees_log_row_too_large`.
fn pack(table: &str, rows: Vec<Value>) -> Result<Vec<Chunk>, EngineError> {
    let budget = BODY_BYTES_MAX - CHUNK_ENVELOPE_BYTES;
    let total = rows.len();
    let mut chunks = vec![Chunk {
        table: table.into(),
        rows: Vec::new(),
    }];
    let mut used = 0;
    for row in rows {
        let bytes = serde_json::to_vec(&row).map_err(error)?.len() + 1;
        if bytes > budget {
            return Err(error("worktrees_log_row_too_large"));
        }
        if used + bytes > budget {
            chunks.push(Chunk {
                table: table.into(),
                rows: Vec::new(),
            });
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
    assert!(chunks.iter().all(|chunk| chunk.table == table));
    Ok(chunks)
}

/// Inserts one snapshot chunk's rows into its table. Inputs: the connection and the chunk.
/// Output: Ok, or `worktrees_log_chunk_invalid` for an unknown table, a row that is not
/// that table's column text, or a row the table refuses.
fn restore(db: &Connection, chunk: &Chunk) -> Result<(), EngineError> {
    let invalid = || error("worktrees_log_chunk_invalid");
    let (table, columns) = [ADMISSIONS, LEASES]
        .into_iter()
        .find(|(table, _)| *table == chunk.table)
        .ok_or_else(invalid)?;
    if chunk.rows.len() > SNAPSHOT_ROWS_MAX {
        return Err(invalid());
    }
    let width = columns.split(',').count();
    let marks = (1..=width)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>();
    let mut statement = db
        .prepare(&format!("INSERT INTO {table} VALUES ({})", marks.join(",")))
        .map_err(error)?;
    for row in &chunk.rows {
        let values = row
            .as_array()
            .filter(|values| values.len() == width)
            .ok_or_else(invalid)?;
        let texts = values
            .iter()
            .map(|value| value.as_str().ok_or_else(invalid))
            .collect::<Result<Vec<_>, _>>()?;
        statement
            .execute(params_from_iter(texts))
            .map_err(|_| invalid())?;
    }
    assert!(width == 3 || width == 4);
    assert!(chunk.rows.len() <= SNAPSHOT_ROWS_MAX);
    Ok(())
}

/// Deletes every row of both worktree tables. Input: the connection. Output: Ok or the
/// store's refusal.
fn clear(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch("DELETE FROM repository_admissions; DELETE FROM worktree_leases;")
        .map_err(error)
}

/// An engine error from any failure. Input: the failure. Output: the error.
fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}
