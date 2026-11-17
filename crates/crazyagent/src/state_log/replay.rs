use super::{
    Aggregate, Chunk, SNAPSHOT_CHUNK, SNAPSHOT_DONE, SnapshotChunk, TOMBSTONE, error,
    verify_snapshot,
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
        0
    }

    /// Sets a manual clock. Input: ms since the Unix epoch. Output: Ok, or
    /// `state_log_clock_system` for the system clock.
    pub fn set_ms(&self, at_ms: u64) -> Result<(), EngineError> {
        let _ = at_ms;
        Ok(())
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
    let _ = (db, aggregate, &mut apply);
    Ok(after)
}

/// The table and rows a snapshot chunk event carries, for a projection rebuilding from a
/// snapshot run. Input: the event. Output: the chunk, or `state_log_replay_chunk` for an
/// event that is not a chunk of its own aggregate.
pub fn chunk(event: &Logged) -> Result<Chunk, EngineError> {
    let _ = event;
    Ok(Chunk {
        table: String::new(),
        rows: Vec::new(),
    })
}

/// Redacts one logged event in place for an owner delete: its body becomes [`TOMBSTONE`]
/// while its seq, revision, aggregate, kind and time stay, so replay stays contiguous and
/// skips it. The old body is overwritten with zeros (`secure_delete` on for the update, then
/// put back); the caller checkpoints the WAL after commit. Snapshot events are refused until
/// compaction (L1b) can rewrite their run. Inputs: the delete's transaction and the seq.
/// Output: Ok, or `state_log_redact_missing` or `state_log_redact_snapshot`.
pub fn redact(tx: &Transaction<'_>, seq: u64) -> Result<(), EngineError> {
    let _ = (tx, seq);
    Ok(())
}

/// Every row of one table, ordered by all its columns, so a live table and its rebuild
/// compare equal whatever order their rows were written in. Meant for replay rows in tests.
/// Inputs: the connection and the table name (1 to 64 ASCII letters, digits and `_`).
/// Output: the rows, or `state_log_replay_table`.
pub fn rows(db: &Connection, table: &str) -> Result<Vec<Vec<Value>>, EngineError> {
    let _ = (db, table);
    Ok(Vec::new())
}

/// Reads the next events after `after`, at most [`REPLAY_BATCH_MAX`], checking each `seq`
/// is the previous plus one, no revision is lower than the one before, and every column is
/// in range. Inputs: the connection, the last seq read and its revision (0 for
/// none). Output: the events, or `state_log_replay_gap`, `state_log_replay_revision`,
/// `state_log_replay_aggregate` or `state_log_replay_row`.
fn batch(db: &Connection, after: u64, revision: u64) -> Result<Vec<Logged>, EngineError> {
    let _ = (db, after, revision);
    Ok(Vec::new())
}

/// The aggregate a stored name names, read through its serde name, which
/// [`Aggregate::name`] matches. Input: the name. Output: the aggregate, none for another.
fn parse(name: &str) -> Option<Aggregate> {
    let _ = name;
    None
}

/// Whether the store holds the log table yet, read without creating it. Input: the
/// connection. Output: true once a transaction created it.
fn logged(db: &Connection) -> Result<bool, EngineError> {
    let _ = db;
    Ok(false)
}
