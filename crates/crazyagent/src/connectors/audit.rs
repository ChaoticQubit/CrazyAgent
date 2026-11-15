//! The connector activity log: one row per tool list and tool call an agent made through
//! the gateway, with who, what, how long, how large and how it ended. It never holds a
//! call's arguments or a result.

use crate::connectors::settings::KeepLog;
use crate::engine::EngineError;
use rusqlite::Connection;

/// The most rows the log keeps, whatever the keep setting says.
pub const ROWS_MAX: i64 = 100_000;

/// What a row is about: a tool the gateway counts as read or write, a tool list, or a
/// call that named no tool the gateway knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    Read,
    Write,
    List,
    Unknown,
}

/// One row of the log, as it is written and read: sizes in bytes, times in milliseconds,
/// and the outcome as `ok` or a refusal code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRow {
    pub connector: String,
    pub tool: String,
    pub kind: CallKind,
    pub project: String,
    pub agent: String,
    pub started_ms: i64,
    pub ms: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub outcome: String,
}

/// Writes one row and drops what the keep setting and the row cap no longer hold. Inputs:
/// the engine database, the row and the keep setting. Output: Ok, or
/// `connectors_unavailable`.
pub fn record(_db: &Connection, _row: &CallRow, _keep: KeepLog) -> Result<(), EngineError> {
    Err(EngineError("connectors_unavailable".into()))
}

/// Reads the newest rows. Inputs: the engine database and how many, at most 500. Output:
/// the rows, newest first, or `connectors_unavailable`.
pub fn recent(_db: &Connection, _limit: usize) -> Result<Vec<CallRow>, EngineError> {
    Err(EngineError("connectors_unavailable".into()))
}
