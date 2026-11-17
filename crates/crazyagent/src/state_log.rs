use crate::engine::EngineError;
use fence::Fence;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The write fence on append, one per writer class, and the engine hosts this process runs
/// (v2.5 L3).
pub mod fence;
/// Replay of the log into projections, the compare of rebuilt tables, redaction in place, the
/// writers' clock and the process exit event (v2.5 L2).
pub mod replay;
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

/// The engine state groups the log orders events by (v2.5 plan section 3.1), a closed
/// set: each name is stored in the log's `aggregate` column and in snapshot bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggregate {
    Worktrees,
    Ide,
    Memory,
    Projects,
    Landing,
    Settings,
    Plugins,
    Sessions,
    Intake,
    Engine,
    Accounts,
    Team,
}

impl Aggregate {
    /// The aggregate's stored name, as its snake_case serde name. Output: the name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Worktrees => "worktrees",
            Self::Ide => "ide",
            Self::Memory => "memory",
            Self::Projects => "projects",
            Self::Landing => "landing",
            Self::Settings => "settings",
            Self::Plugins => "plugins",
            Self::Sessions => "sessions",
            Self::Intake => "intake",
            Self::Engine => "engine",
            Self::Accounts => "accounts",
            Self::Team => "team",
        }
    }
}

/// One event a writer appends: its aggregate, kind name, body schema version, the time
/// from the writer's clock and its body bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Draft<'b> {
    pub aggregate: Aggregate,
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

/// The body of one snapshot chunk event, in run order from 0.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotChunk {
    aggregate: Aggregate,
    table: String,
    seq: u32,
    rows: Vec<serde_json::Value>,
}

/// The body of the event that closes a snapshot run: its chunk count and the sha256 of
/// the chunk bodies in order.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotDone {
    aggregate: Aggregate,
    chunks: u32,
    sha256: String,
}

/// Appends the events of one write transaction to `state_log` at that transaction's
/// `state` revision, each with the next `seq`, behind the writer's fence.
pub struct Appender<'a, 'c> {
    tx: &'a Transaction<'c>,
    revision: u64,
    appended: usize,
    fence: Fence,
}

impl<'a, 'c> Appender<'a, 'c> {
    /// Starts appending in an owner request's transaction, whose revision the request
    /// already moved: it must be the `state` revision now and newer than the last logged
    /// one, so each appending transaction brings its own and none goes back. The fence is
    /// checked first, once for the transaction. Inputs: the transaction, that revision and
    /// the fence. Output: the appender, a fence refusal, `state_log_revision_stale` or
    /// `state_log_revision_not_newer`.
    pub fn owner(
        tx: &'a Transaction<'c>,
        revision: u64,
        fence: Fence,
    ) -> Result<Self, EngineError> {
        fence::check(tx, &fence)?;
        Self::begin(tx, revision, fence)
    }

    /// Starts appending in a transaction of a writer on its own connection, moving the
    /// `state` revision once for it after its fence is checked. Inputs: the transaction and
    /// the fence. Output: the appender, a fence refusal, or `state_log_revision_exhausted`
    /// when the revision is at its largest value.
    pub fn own(tx: &'a Transaction<'c>, fence: Fence) -> Result<Self, EngineError> {
        fence::check(tx, &fence)?;
        let moved = tx
            .execute(
                "UPDATE state SET revision=revision+1 WHERE id=1 AND revision<?1",
                [i64::MAX],
            )
            .map_err(error)?;
        if moved != 1 {
            return Err(error("state_log_revision_exhausted"));
        }
        Self::begin(tx, state_revision(tx)?, fence)
    }

    /// Runs one projection apply in the appender's transaction after checking the fence
    /// again, the pair of the check at the start. Input: the apply. Output: its result, or
    /// the fence refusal without running it.
    pub fn project<T>(
        &self,
        apply: impl FnOnce(&Transaction<'c>) -> Result<T, EngineError>,
    ) -> Result<T, EngineError> {
        fence::check(self.tx, &self.fence)?;
        apply(self.tx)
    }

    /// Opens the appender at a fenced revision: it must be the `state` revision now and
    /// newer than the last logged one. Inputs: the transaction, the revision and the fence.
    /// Output: the appender, or `state_log_revision_stale` or `state_log_revision_not_newer`.
    fn begin(tx: &'a Transaction<'c>, revision: u64, fence: Fence) -> Result<Self, EngineError> {
        ensure(tx)?;
        if revision != state_revision(tx)? {
            return Err(error("state_log_revision_stale"));
        }
        let last: Option<i64> = tx
            .query_row(
                "SELECT revision FROM state_log ORDER BY seq DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        if revision == 0 || last.is_some_and(|last| revision <= last as u64) {
            return Err(error("state_log_revision_not_newer"));
        }
        Ok(Self {
            tx,
            revision,
            appended: 0,
            fence,
        })
    }

    /// The revision this transaction's events carry. Output: the revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Appends one event; the snapshot kinds are written only by [`Self::snapshot`]. Input:
    /// the draft. Output: its `seq`, or `state_log_kind_reserved` or a bound's refusal.
    pub fn append(&mut self, draft: &Draft<'_>) -> Result<u64, EngineError> {
        if draft.kind == SNAPSHOT_CHUNK || draft.kind == SNAPSHOT_DONE {
            return Err(error("state_log_kind_reserved"));
        }
        self.push(draft)
    }

    /// Appends a snapshot run of an aggregate: one chunk event per chunk, in order from 0,
    /// closed by an event holding the chunk count and the sha256 of the chunk bodies in
    /// order. Every chunk is checked under [`BODY_BYTES_MAX`] before any is appended.
    /// Inputs: the aggregate, the time and the chunks. Output: the `seq` of the closing
    /// event, or `state_log_chunk_bounds` or `state_log_transaction_full`.
    pub fn snapshot(
        &mut self,
        aggregate: Aggregate,
        at_ms: u64,
        chunks: Vec<Chunk>,
    ) -> Result<u64, EngineError> {
        if chunks.len() >= TRANSACTION_EVENTS_MAX - self.appended {
            return Err(error("state_log_transaction_full"));
        }
        let count = chunks.len() as u32;
        let bodies = chunks
            .into_iter()
            .enumerate()
            .map(|(seq, chunk)| {
                serde_json::to_vec(&SnapshotChunk {
                    aggregate,
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
            self.push(&Draft {
                aggregate,
                kind: SNAPSHOT_CHUNK,
                schema: 1,
                at_ms,
                body,
            })?;
        }
        let done = serde_json::to_vec(&SnapshotDone {
            aggregate,
            chunks: count,
            sha256: format!("{:x}", digest.finalize()),
        })
        .map_err(error)?;
        self.push(&Draft {
            aggregate,
            kind: SNAPSHOT_DONE,
            schema: 1,
            at_ms,
            body: &done,
        })
    }

    /// Appends one checked event at the next `seq`, refusing one past the transaction's
    /// bound or past the last possible `seq`. Input: the draft. Output: its `seq`.
    fn push(&mut self, draft: &Draft<'_>) -> Result<u64, EngineError> {
        check(draft)?;
        if self.appended >= TRANSACTION_EVENTS_MAX {
            return Err(error("state_log_transaction_full"));
        }
        let previous: i64 = self
            .tx
            .prepare_cached("SELECT COALESCE(MAX(seq), 0) FROM state_log")
            .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
            .map_err(error)?;
        let seq = previous
            .checked_add(1)
            .ok_or_else(|| error("state_log_seq_exhausted"))?;
        self.tx
            .prepare_cached("INSERT INTO state_log VALUES (?1,?2,?3,?4,?5,?6,?7)")
            .and_then(|mut statement| {
                statement.execute(params![
                    seq,
                    self.revision as i64,
                    draft.aggregate.name(),
                    draft.kind,
                    draft.schema,
                    draft.at_ms as i64,
                    draft.body
                ])
            })
            .map_err(error)?;
        self.appended += 1;
        Ok(seq as u64)
    }
}

/// Checks the snapshot run a closing event ends: the sha256 of the bodies of the events
/// before it, as many as it counts, must match, and each must be a chunk of its aggregate
/// in order from 0. A missing, changed or reordered chunk fails the hash; a count no run
/// can hold is refused before any row is read, and a body over [`BODY_BYTES_MAX`] before
/// it is loaded, as a log made without the table's checks can hold one. Inputs: the
/// connection and the closing event's `seq`. Output: Ok, or `state_log_snapshot_missing`,
/// `state_log_snapshot_invalid`, `state_log_snapshot_hash` or `state_log_snapshot_chunks`.
pub fn verify_snapshot(db: &Connection, done: u64) -> Result<(), EngineError> {
    let missing = || error("state_log_snapshot_missing");
    let invalid = || error("state_log_snapshot_invalid");
    let seq = i64::try_from(done).map_err(|_| missing())?;
    let cap = BODY_BYTES_MAX as i64;
    let closing: Option<(String, Option<Vec<u8>>)> = db
        .query_row(
            "SELECT kind, CASE WHEN length(body) <= ?2 THEN body END FROM state_log WHERE seq=?1",
            [seq, cap],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((_, body)) = closing.filter(|(kind, _)| kind == SNAPSHOT_DONE) else {
        return Err(missing());
    };
    let closed = body
        .and_then(|body| serde_json::from_slice::<SnapshotDone>(&body).ok())
        .filter(|closed| (closed.chunks as usize) < TRANSACTION_EVENTS_MAX)
        .ok_or_else(invalid)?;
    let mut statement = db
        .prepare(
            "SELECT kind, CASE WHEN length(body) <= ?3 THEN body END FROM state_log
            WHERE seq>=?1 AND seq<?2 ORDER BY seq",
        )
        .map_err(error)?;
    let rows = statement
        .query_map([seq - i64::from(closed.chunks), seq, cap], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<Vec<u8>>>(1)?))
        })
        .map_err(error)?;
    let mut digest = Sha256::new();
    let mut count: u32 = 0;
    let mut in_order = true;
    for row in rows {
        let (kind, body) = row.map_err(error)?;
        let body = body.ok_or_else(invalid)?;
        digest.update(&body);
        let chunk = (kind == SNAPSHOT_CHUNK)
            .then(|| serde_json::from_slice::<SnapshotChunk>(&body).ok())
            .flatten();
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
fn check(draft: &Draft<'_>) -> Result<(), EngineError> {
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

/// Reads the store's `state` revision. Input: the connection. Output: the revision.
fn state_revision(db: &Connection) -> Result<u64, EngineError> {
    let revision: i64 = db
        .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
            row.get(0)
        })
        .map_err(error)?;
    u64::try_from(revision).map_err(error)
}

/// Creates the log table inside the caller's transaction when it is absent, so it
/// commits or rolls back with that transaction. Input: the connection. Output: any
/// SQLite failure.
fn ensure(db: &Connection) -> Result<(), EngineError> {
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

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}
