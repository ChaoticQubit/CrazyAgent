/// The validated aggregate name the log stores.
pub mod aggregate;

/// The appender of one write transaction, behind the writer's fence hook.
mod appender;

/// Compaction of the log: a snapshot run per aggregate every [`compact::COMPACT_EVERY`]
/// events, the bodies behind its second-newest run tombstoned, behind a switch that is Off
/// by default.
pub mod compact;

/// Replay of the log into projections, the compare of rebuilt tables, redaction in place and
/// the process exit event (v2.5 L2), reading names through the engine's set.
pub mod replay;

pub use aggregate::{AGGREGATE_BYTES_MAX, AggregateName};
pub use appender::{Appender, FenceCheck, KnownName};

use crate::StorageError;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
mod tests;

/// The most bytes one event body may hold; a snapshot chunk is one event, so it holds
/// at most this many too (v2.5 plan section 3.2).
pub const BODY_BYTES_MAX: usize = 65_536;
/// The most bytes an event kind name may hold.
pub const KIND_BYTES_MAX: usize = 64;
/// The most events one appending transaction may hold, a snapshot run included.
pub const TRANSACTION_EVENTS_MAX: usize = 65_536;
/// The body a redacted event keeps in place of its own, so its seq and revision stay and
/// replay skips it; no appended event may carry it.
pub const TOMBSTONE: &[u8] = b"\0redacted\0";
/// The kind of one chunk of a snapshot run.
pub const SNAPSHOT_CHUNK: &str = "SnapshotChunk";
/// The kind of the event that closes a snapshot run.
pub const SNAPSHOT_DONE: &str = "SnapshotDone";

/// One event a writer appends: its aggregate, kind name, body schema version, the time
/// from the writer's clock and its body bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Draft<'b> {
    pub aggregate: AggregateName,
    pub kind: &'b str,
    pub schema: u16,
    pub at_ms: u64,
    pub body: &'b [u8],
}

/// One chunk a snapshot run carries: rows of one table, as JSON values.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub table: String,
    pub rows: Vec<serde_json::Value>,
}

/// The body of one snapshot chunk event, in run order from 0. The aggregate is its stored
/// name, read back only when it has a name's shape.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotChunk {
    aggregate: String,
    table: String,
    seq: u32,
    rows: Vec<serde_json::Value>,
}

/// The body of the event that closes a snapshot run: its aggregate's stored name, its chunk
/// count and the sha256 of the chunk bodies in order.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotDone {
    aggregate: String,
    chunks: u32,
    sha256: String,
}

/// The bodies of a snapshot run of an aggregate: one chunk body per chunk, in order from 0,
/// and the closing body holding the chunk count and the sha256 of the chunk bodies in order.
/// Every chunk is checked under [`BODY_BYTES_MAX`] before the closing body is made. Inputs:
/// the aggregate and the chunks. Output: the chunk bodies and the closing body, or
/// `state_log_chunk_bounds`.
pub fn snapshot_bodies(
    aggregate: AggregateName,
    chunks: Vec<Chunk>,
) -> Result<(Vec<Vec<u8>>, Vec<u8>), StorageError> {
    let count = u32::try_from(chunks.len()).map_err(error)?;
    let bodies = chunks
        .into_iter()
        .enumerate()
        .map(|(seq, chunk)| {
            serde_json::to_vec(&SnapshotChunk {
                aggregate: aggregate.name().to_owned(),
                table: chunk.table,
                seq: seq as u32,
                rows: chunk.rows,
            })
            .map_err(error)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if bodies.iter().any(|body| body.len() > BODY_BYTES_MAX) {
        return Err(error("state_log_chunk_bounds"));
    }
    let mut digest = Sha256::new();
    for body in &bodies {
        digest.update(body);
    }
    let done = serde_json::to_vec(&SnapshotDone {
        aggregate: aggregate.name().to_owned(),
        chunks: count,
        sha256: format!("{:x}", digest.finalize()),
    })
    .map_err(error)?;
    assert_eq!(bodies.len(), count as usize);
    Ok((bodies, done))
}

/// The chunk a snapshot chunk body carries, when the body names `aggregate`, for a projection
/// rebuilding from a snapshot run. Inputs: the aggregate's stored name and the body. Output:
/// the chunk, or None for a body that is not a chunk of that aggregate.
pub fn read_chunk(aggregate: &str, body: &[u8]) -> Option<Chunk> {
    let chunk = serde_json::from_slice::<SnapshotChunk>(body).ok()?;
    (chunk.aggregate == aggregate).then_some(Chunk {
        table: chunk.table,
        rows: chunk.rows,
    })
}

/// Checks the snapshot run a closing event ends: the sha256 of the bodies of the events
/// before it, as many as it counts, must match, and each must be a chunk of its aggregate
/// in order from 0. A missing, changed or reordered chunk fails the hash; a count no run
/// can hold is refused before any row is read, and a body over [`BODY_BYTES_MAX`] before
/// it is loaded, as a log made without the table's checks can hold one. Every body must name
/// an aggregate of a name's shape, the same as its own row's `aggregate` column. Inputs: the
/// connection and the closing event's `seq`. Output: Ok, or `state_log_snapshot_missing`,
/// `state_log_snapshot_invalid`, `state_log_snapshot_hash` or `state_log_snapshot_chunks`.
pub fn verify_snapshot(db: &Connection, done: u64) -> Result<(), StorageError> {
    let missing = || error("state_log_snapshot_missing");
    let invalid = || error("state_log_snapshot_invalid");
    let seq = i64::try_from(done).map_err(|_| missing())?;
    let cap = BODY_BYTES_MAX as i64;
    let closing: Option<(String, String, Option<Vec<u8>>)> = db
        .query_row(
            "SELECT kind, aggregate, CASE WHEN length(body) <= ?2 THEN body END
            FROM state_log WHERE seq=?1",
            [seq, cap],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((_, column, body)) = closing.filter(|(kind, _, _)| kind == SNAPSHOT_DONE) else {
        return Err(missing());
    };
    let closed = body
        .and_then(|body| serde_json::from_slice::<SnapshotDone>(&body).ok())
        .filter(|closed| (closed.chunks as usize) < TRANSACTION_EVENTS_MAX)
        .filter(|closed| AggregateName::shaped(&closed.aggregate) && closed.aggregate == column)
        .ok_or_else(invalid)?;
    let mut statement = db
        .prepare(
            "SELECT kind, aggregate, CASE WHEN length(body) <= ?3 THEN body END FROM state_log
            WHERE seq>=?1 AND seq<?2 ORDER BY seq",
        )
        .map_err(error)?;
    let rows = statement
        .query_map([seq - i64::from(closed.chunks), seq, cap], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<Vec<u8>>>(2)?,
            ))
        })
        .map_err(error)?;
    let mut digest = Sha256::new();
    let mut count: u32 = 0;
    let mut in_order = true;
    for row in rows {
        let (kind, column, body) = row.map_err(error)?;
        let body = body.ok_or_else(invalid)?;
        digest.update(&body);
        let chunk = (kind == SNAPSHOT_CHUNK)
            .then(|| serde_json::from_slice::<SnapshotChunk>(&body).ok())
            .flatten();
        if chunk.as_ref().is_some_and(|chunk| {
            !AggregateName::shaped(&chunk.aggregate) || chunk.aggregate != column
        }) {
            return Err(invalid());
        }
        in_order &=
            chunk.is_some_and(|chunk| chunk.aggregate == closed.aggregate && chunk.seq == count);
        count = count.saturating_add(1);
    }
    if format!("{:x}", digest.finalize()) != closed.sha256 {
        return Err(error("state_log_snapshot_hash"));
    }
    if !in_order || count != closed.chunks {
        return Err(error("state_log_snapshot_chunks"));
    }
    Ok(())
}

/// Refuses a draft out of the log's bounds: a body over [`BODY_BYTES_MAX`] or equal to
/// [`TOMBSTONE`], a kind that is not 1 to [`KIND_BYTES_MAX`] ASCII letters and digits,
/// schema 0, or a time past the largest `i64`. Input: the draft. Output: Ok, or the refusal.
pub fn check(draft: &Draft<'_>) -> Result<(), StorageError> {
    if draft.body.len() > BODY_BYTES_MAX {
        return Err(error("state_log_body_bounds"));
    }
    if draft.body == TOMBSTONE {
        return Err(error("state_log_body_reserved"));
    }
    if draft.kind.is_empty()
        || draft.kind.len() > KIND_BYTES_MAX
        || !draft.kind.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(error("state_log_kind"));
    }
    if draft.schema == 0 {
        return Err(error("state_log_schema"));
    }
    if draft.at_ms > i64::MAX as u64 {
        return Err(error("state_log_at_ms"));
    }
    Ok(())
}

/// Creates the log table inside the caller's transaction when it is absent, so it
/// commits or rolls back with that transaction. Input: the connection. Output: any
/// SQLite failure.
pub fn ensure(db: &Connection) -> Result<(), StorageError> {
    db.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS state_log (
            seq INTEGER PRIMARY KEY,
            revision INTEGER NOT NULL CHECK(revision >= 1),
            aggregate TEXT NOT NULL,
            kind TEXT NOT NULL CHECK(length(kind) BETWEEN 1 AND {KIND_BYTES_MAX}),
            schema INTEGER NOT NULL CHECK(schema >= 1),
            at_ms INTEGER NOT NULL CHECK(at_ms >= 0),
            body BLOB NOT NULL CHECK(length(body) <= {BODY_BYTES_MAX}))"
    ))
    .map_err(error)
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
    pub fn set_ms(&self, at_ms: u64) -> Result<(), StorageError> {
        match self {
            Self::System => Err(error("state_log_clock_system")),
            Self::Manual(now) => {
                now.store(at_ms, Ordering::SeqCst);
                Ok(())
            }
        }
    }
}

/// Builds a log error. Input: the cause. Output: the error with its text.
fn error(value: impl std::fmt::Display) -> StorageError {
    StorageError(value.to_string())
}
