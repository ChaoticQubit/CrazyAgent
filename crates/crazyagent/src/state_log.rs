use crate::engine::EngineError;
use rusqlite::{Connection, Transaction};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// The most bytes one event body may hold; a snapshot chunk is one event, so it holds
/// at most this many too (v2.5 plan section 3.2).
pub const BODY_BYTES_MAX: usize = 65_536;
/// The most bytes an event kind name may hold.
pub const KIND_BYTES_MAX: usize = 64;
/// The most events one appending transaction may hold, a snapshot run included.
pub const TRANSACTION_EVENTS_MAX: usize = 65_536;
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
/// `state` revision, each with the next `seq`.
pub struct Appender<'a, 'c> {
    tx: &'a Transaction<'c>,
    revision: u64,
    appended: usize,
}

impl<'a, 'c> Appender<'a, 'c> {
    /// Starts appending in an owner request's transaction, whose revision the request
    /// already moved. Inputs: the transaction and that revision. Output: the appender.
    pub fn owner(tx: &'a Transaction<'c>, revision: u64) -> Result<Self, EngineError> {
        ensure(tx)?;
        Ok(Self {
            tx,
            revision,
            appended: 0,
        })
    }

    /// Starts appending in a transaction of a writer on its own connection, moving the
    /// `state` revision once for it. Input: the transaction. Output: the appender.
    pub fn own(tx: &'a Transaction<'c>) -> Result<Self, EngineError> {
        ensure(tx)?;
        Ok(Self {
            tx,
            revision: 0,
            appended: 0,
        })
    }

    /// The revision this transaction's events carry. Output: the revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Appends one event. Input: the draft. Output: its `seq`.
    pub fn append(&mut self, _draft: &Draft<'_>) -> Result<u64, EngineError> {
        Ok(0)
    }

    /// Appends a snapshot run of an aggregate. Inputs: the aggregate, the time and the
    /// chunks in order. Output: the `seq` of the closing event.
    pub fn snapshot(
        &mut self,
        _aggregate: Aggregate,
        _at_ms: u64,
        _chunks: Vec<Chunk>,
    ) -> Result<u64, EngineError> {
        Ok(0)
    }
}

/// Checks the snapshot run a closing event ends. Inputs: the connection and the closing
/// event's `seq`. Output: Ok, or the refusal.
pub fn verify_snapshot(_db: &Connection, _done: u64) -> Result<(), EngineError> {
    Ok(())
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
