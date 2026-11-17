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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(test)]
mod tests;

/// The most events replay reads from the log at once (v2.5 plan, tiger-style bounds).
pub const REPLAY_BATCH_MAX: usize = 4_096;
/// The kind of the event a writer appends when a process it started exits, so projections
/// learn of the exit from the log and never from the live process table.
pub const PROCESS_EXITED: &str = "ProcessExited";
/// The most bytes a table name given to [`rows`] may hold.
const TABLE_BYTES_MAX: usize = 64;
/// The most times [`checkpoint`] tries to empty the WAL after a redact.
pub const CHECKPOINT_ATTEMPTS_MAX: usize = 4;
/// How long [`checkpoint`] waits between two tries, beside the connection's busy wait.
const CHECKPOINT_RETRY_WAIT: Duration = Duration::from_millis(100);

const _: () = assert!(CHECKPOINT_ATTEMPTS_MAX > 1);

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
/// put back by its keyword, since SQLite reads the number 2 as ON and not FAST); the caller
/// runs [`checkpoint`] after commit. Snapshot events are refused until compaction (L1b) can
/// rewrite their run. The delete's fence is checked first, as for an append. Inputs: the
/// delete's transaction, the seq and the fence. Output: Ok, a fence refusal, or
/// `state_log_redact_missing`, `state_log_redact_snapshot` or
/// `state_log_redact_secure_delete` for a mode SQLite does not name.
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
    let mode = match secure {
        0 => "OFF",
        1 => "ON",
        2 => "FAST",
        _ => return Err(error("state_log_redact_secure_delete")),
    };
    tx.execute_batch("PRAGMA secure_delete=ON;")
        .map_err(error)?;
    let updated = tx
        .execute(
            "UPDATE state_log SET body=?1 WHERE seq=?2",
            params![TOMBSTONE, seq],
        )
        .map_err(error);
    tx.execute_batch(&format!("PRAGMA secure_delete={mode};"))
        .map_err(error)?;
    updated.map(|_| ())
}

/// Moves every WAL frame into the database file and empties the WAL after a redact
/// committed, so no old body stays in the WAL. A reader still in the WAL makes a try busy;
/// it is tried again, at most [`CHECKPOINT_ATTEMPTS_MAX`] times, each try within the
/// connection's busy wait. Input: the connection.
/// Output: true once the WAL is empty, false when every try was busy.
pub fn checkpoint(db: &Connection) -> bool {
    checkpoint_with(db, || std::thread::sleep(CHECKPOINT_RETRY_WAIT))
}

/// [`checkpoint`] with the wait between two tries given.
/// Inputs: the connection and the wait.
/// Output: true once the WAL is empty, false when every try was busy.
fn checkpoint_with(db: &Connection, mut wait: impl FnMut()) -> bool {
    for attempt in 1..=CHECKPOINT_ATTEMPTS_MAX {
        let busy = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            row.get::<_, i64>(0)
        });
        if matches!(busy, Ok(0)) {
            return true;
        }
        if attempt < CHECKPOINT_ATTEMPTS_MAX {
            wait();
        }
    }
    false
}

/// Redacts, for an owner delete, every event of the given aggregates, and of the given kinds
/// when a list is given, whose body is a JSON object holding the top-level `field` as the
/// string `value`, except the delete's own event `keep`, each through [`redact`]. Events are
/// found by the quoted value's bytes, at most [`REPLAY_BATCH_MAX`] at a time, then matched
/// exactly; `value` holds no character JSON escapes, so it is logged as its own bytes.
/// Inputs: the delete's transaction, the aggregates, the kinds (none for all), the field, the
/// value, the kept seq and the fence. Output: how many events were redacted, a fence or
/// [`redact`] refusal, `state_log_redact_match` or `state_log_replay_row`.
pub fn redact_matching(
    tx: &Transaction<'_>,
    aggregates: &[Aggregate],
    kinds: Option<&[&str]>,
    field: &str,
    value: &str,
    keep: u64,
    fence: &Fence,
) -> Result<usize, EngineError> {
    fence::check(tx, fence)?;
    if aggregates.is_empty()
        || kinds.is_some_and(|kinds| kinds.is_empty())
        || field.is_empty()
        || value.is_empty()
        || value
            .chars()
            .any(|letter| letter == '"' || letter == '\\' || letter.is_control())
    {
        return Err(error("state_log_redact_match"));
    }
    if !logged(tx)? {
        return Ok(0);
    }
    let needle = format!("\"{value}\"");
    let (mut after, mut redacted) = (0, 0);
    loop {
        let found = hits(tx, after, needle.as_bytes())?;
        for (seq, aggregate, kind, body) in &found {
            let listed = aggregates
                .iter()
                .any(|wanted| wanted.name() == aggregate.as_str())
                && kinds.is_none_or(|kinds| kinds.iter().any(|wanted| *wanted == kind.as_str()));
            let named = serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(body)
                .is_ok_and(|object| {
                    object.get(field).and_then(serde_json::Value::as_str) == Some(value)
                });
            if *seq != keep && listed && named {
                redact(tx, *seq, fence)?;
                redacted += 1;
            }
        }
        match found.last() {
            Some((seq, _, _, _)) if found.len() == REPLAY_BATCH_MAX => after = *seq,
            _ => return Ok(redacted),
        }
    }
}

/// The events after `after` whose body holds the needle's bytes, at most
/// [`REPLAY_BATCH_MAX`], in seq order, a body over [`BODY_BYTES_MAX`] refused before it is
/// loaded. Inputs: the transaction, the last seq read and the needle. Output: each event's
/// seq, aggregate name, kind and body, or `state_log_replay_row`.
fn hits(
    tx: &Transaction<'_>,
    after: u64,
    needle: &[u8],
) -> Result<Vec<(u64, String, String, Vec<u8>)>, EngineError> {
    let row_error = || error("state_log_replay_row");
    let from = i64::try_from(after).map_err(|_| row_error())?;
    let mut statement = tx
        .prepare(
            "SELECT seq, aggregate, kind, CASE WHEN length(body) <= ?4 THEN body END
            FROM state_log WHERE seq>?1 AND instr(body, ?2) > 0 ORDER BY seq LIMIT ?3",
        )
        .map_err(error)?;
    let rows = statement
        .query_map(
            params![from, needle, REPLAY_BATCH_MAX as i64, BODY_BYTES_MAX as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                ))
            },
        )
        .map_err(error)?;
    let mut found = Vec::new();
    for row in rows {
        let (seq, aggregate, kind, body) = row.map_err(error)?;
        let seq = u64::try_from(seq).map_err(|_| row_error())?;
        found.push((seq, aggregate, kind, body.ok_or_else(row_error)?));
    }
    Ok(found)
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
