use super::fence::{self, Fence};
use super::{
    Aggregate, BODY_BYTES_MAX, Chunk, SNAPSHOT_CHUNK, SNAPSHOT_DONE, SnapshotChunk, TOMBSTONE,
    error, verify_snapshot,
};
use crate::engine::EngineError;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
mod tests;

/// The most events replay reads from the log at once (v2.5 plan, tiger-style bounds).
pub const REPLAY_BATCH_MAX: usize = 4_096;
/// The kind of the event a writer appends when a process it started exits, so projections
/// learn of the exit from the log and never from the live process table.
pub const PROCESS_EXITED: &str = "ProcessExited";
/// The most bytes a table name given to [`rows`] may hold.
const TABLE_BYTES_MAX: usize = 64;

/// One event as replay reads it back from the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logged {
    pub seq: u64,
    pub revision: u64,
    pub aggregate: Aggregate,
    pub kind: String,
    pub schema: u16,
    pub at_ms: u64,
    pub body: Vec<u8>,
}

/// The body of a [`PROCESS_EXITED`] event: the process id and how it ended, by its exit
/// code or by the signal that stopped it. The writer appends it under its own aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessExited {
    pub pid: u32,
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// Where a writer reads the time it stamps on an event: the system clock, or a manual one a
/// test sets. Replay never reads a clock; a projection takes time from each event's `at_ms`.
#[derive(Debug)]
pub enum Clock {
    System,
    Manual(AtomicU64),
}

impl Clock {
    /// A manual clock reading the given time. Input: ms since the Unix epoch. Output: the
    /// clock.
    pub fn manual(at_ms: u64) -> Self {
        Self::Manual(AtomicU64::new(at_ms))
    }

    /// The time now. Output: ms since the Unix epoch, 0 for a system time before it.
    pub fn now_ms(&self) -> u64 {
        match self {
            Self::System => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| {
                    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
                }),
            Self::Manual(now) => now.load(Ordering::SeqCst),
        }
    }

    /// Sets a manual clock. Input: ms since the Unix epoch. Output: Ok, or
    /// `state_log_clock_system` for the system clock.
    pub fn set_ms(&self, at_ms: u64) -> Result<(), EngineError> {
        match self {
            Self::System => Err(error("state_log_clock_system")),
            Self::Manual(now) => {
                now.store(at_ms, Ordering::SeqCst);
                Ok(())
            }
        }
    }
}

/// Replays the log after `after` into one aggregate's projection. Every event is read, in
/// batches of at most [`REPLAY_BATCH_MAX`]: each `seq` must be the previous plus one and no
/// revision lower than the one before, across all aggregates. `apply` gets the aggregate's
/// events in order, snapshot chunks included; a redacted event is skipped and a snapshot's
/// closing event is verified, not applied. A store with no log replays nothing. Inputs: the
/// connection, the aggregate, the last seq already applied and the projection. Output: the
/// last seq read, or the first refusal, `apply`'s included.
pub fn replay(
    db: &Connection,
    aggregate: Aggregate,
    after: u64,
    mut apply: impl FnMut(&Logged) -> Result<(), EngineError>,
) -> Result<u64, EngineError> {
    let (mut last, mut revision) = (after, 0);
    loop {
        let events = batch(db, last, revision)?;
        for event in events
            .iter()
            .filter(|event| event.aggregate == aggregate && event.body.as_slice() != TOMBSTONE)
        {
            if event.kind == SNAPSHOT_DONE {
                verify_snapshot(db, event.seq)?;
            } else {
                apply(event)?;
            }
        }
        let Some(tail) = events.last() else {
            return Ok(last);
        };
        (last, revision) = (tail.seq, tail.revision);
        if events.len() < REPLAY_BATCH_MAX {
            return Ok(last);
        }
    }
}

/// The table and rows a snapshot chunk event carries, for a projection rebuilding from a
/// snapshot run. Input: the event. Output: the chunk, or `state_log_replay_chunk` for an
/// event that is not a chunk of its own aggregate.
pub fn chunk(event: &Logged) -> Result<Chunk, EngineError> {
    let refused = || error("state_log_replay_chunk");
    if event.kind != SNAPSHOT_CHUNK {
        return Err(refused());
    }
    let chunk: SnapshotChunk = serde_json::from_slice(&event.body).map_err(|_| refused())?;
    if chunk.aggregate != event.aggregate {
        return Err(refused());
    }
    Ok(Chunk {
        table: chunk.table,
        rows: chunk.rows,
    })
}

/// Redacts one logged event in place for an owner delete: its body becomes [`TOMBSTONE`]
/// while its seq, revision, aggregate, kind and time stay, so replay stays contiguous and
/// skips it. The old body is overwritten with zeros (`secure_delete` on for the update, then
/// put back); the caller checkpoints the WAL after commit. Snapshot events are refused until
/// compaction (L1b) can rewrite their run. The delete's fence is checked first, as for an
/// append. Inputs: the delete's transaction, the seq and the fence. Output: Ok, a fence
/// refusal, or `state_log_redact_missing` or `state_log_redact_snapshot`.
pub fn redact(tx: &Transaction<'_>, seq: u64, fence: &Fence) -> Result<(), EngineError> {
    fence::check(tx, fence)?;
    let missing = || error("state_log_redact_missing");
    let seq = i64::try_from(seq).map_err(|_| missing())?;
    if !logged(tx)? {
        return Err(missing());
    }
    let kind: Option<String> = tx
        .query_row("SELECT kind FROM state_log WHERE seq=?1", [seq], |row| {
            row.get(0)
        })
        .optional()
        .map_err(error)?;
    match kind.as_deref() {
        None => return Err(missing()),
        Some(SNAPSHOT_CHUNK | SNAPSHOT_DONE) => return Err(error("state_log_redact_snapshot")),
        Some(_) => {}
    }
    let secure: i64 = tx
        .query_row("PRAGMA secure_delete", [], |row| row.get(0))
        .map_err(error)?;
    tx.execute_batch("PRAGMA secure_delete=ON;")
        .map_err(error)?;
    let updated = tx
        .execute(
            "UPDATE state_log SET body=?1 WHERE seq=?2",
            params![TOMBSTONE, seq],
        )
        .map_err(error);
    tx.execute_batch(&format!("PRAGMA secure_delete={secure};"))
        .map_err(error)?;
    updated.map(|_| ())
}

/// Every row of one table, ordered by all its columns, so a live table and its rebuild
/// compare equal whatever order their rows were written in. Meant for replay rows in tests.
/// Inputs: the connection and the table name (1 to 64 ASCII letters, digits and `_`).
/// Output: the rows, or `state_log_replay_table`.
pub fn rows(db: &Connection, table: &str) -> Result<Vec<Vec<Value>>, EngineError> {
    if table.is_empty()
        || table.len() > TABLE_BYTES_MAX
        || !table
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(error("state_log_replay_table"));
    }
    let columns = db
        .prepare(&format!("SELECT * FROM \"{table}\""))
        .map_err(error)?
        .column_count();
    let order = (1..=columns)
        .map(|column| column.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut statement = db
        .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY {order}"))
        .map_err(error)?;
    let rows = statement
        .query_map([], |row| {
            (0..columns)
                .map(|column| row.get::<_, Value>(column))
                .collect::<rusqlite::Result<Vec<Value>>>()
        })
        .map_err(error)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(error)
}

/// Reads the next events after `after`, at most [`REPLAY_BATCH_MAX`], checking each `seq`
/// is the previous plus one, no revision is lower than the one before, and every column is
/// in range, a body over [`BODY_BYTES_MAX`] refused before it is loaded. Inputs: the connection, the last seq read and its revision (0 for
/// none). Output: the events, or `state_log_replay_gap`, `state_log_replay_revision`,
/// `state_log_replay_aggregate` or `state_log_replay_row`.
fn batch(db: &Connection, after: u64, revision: u64) -> Result<Vec<Logged>, EngineError> {
    let gap = || error("state_log_replay_gap");
    let from = i64::try_from(after).map_err(|_| gap())?;
    if !logged(db)? {
        return Ok(Vec::new());
    }
    let mut statement = db
        .prepare(
            "SELECT seq, revision, aggregate, kind, schema, at_ms,
            CASE WHEN length(body) <= ?3 THEN body END FROM state_log
            WHERE seq>?1 ORDER BY seq LIMIT ?2",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(
            params![from, REPLAY_BATCH_MAX as i64, BODY_BYTES_MAX as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<Vec<u8>>>(6)?,
                ))
            },
        )
        .map_err(error)?;
    let (mut previous, mut floor) = (after, revision);
    let mut events = Vec::new();
    for row in rows {
        let (seq, revision, aggregate_name, kind, schema, at_ms, body) = row.map_err(error)?;
        let seq = u64::try_from(seq)
            .ok()
            .filter(|seq| Some(*seq) == previous.checked_add(1))
            .ok_or_else(gap)?;
        let revision = u64::try_from(revision)
            .ok()
            .filter(|revision| *revision >= floor)
            .ok_or_else(|| error("state_log_replay_revision"))?;
        let aggregate =
            parse(&aggregate_name).ok_or_else(|| error("state_log_replay_aggregate"))?;
        let row_error = || error("state_log_replay_row");
        let schema = u16::try_from(schema).map_err(|_| row_error())?;
        let at_ms = u64::try_from(at_ms).map_err(|_| row_error())?;
        let body = body.ok_or_else(row_error)?;
        (previous, floor) = (seq, revision);
        events.push(Logged {
            seq,
            revision,
            aggregate,
            kind,
            schema,
            at_ms,
            body,
        });
    }
    Ok(events)
}

/// The aggregate a stored name names, read through its serde name, which
/// [`Aggregate::name`] matches. Input: the name. Output: the aggregate, none for another.
fn parse(name: &str) -> Option<Aggregate> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}

/// Whether the store holds the log table yet, read without creating it. Input: the
/// connection. Output: true once a transaction created it.
fn logged(db: &Connection) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='state_log')",
        [],
        |row| row.get(0),
    )
    .map_err(error)
}
