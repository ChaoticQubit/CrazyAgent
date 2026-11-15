//! The connector activity log: one row per tool list and tool call an agent made through
//! the gateway, with who, what, how long, how large and how it ended. It never holds a
//! call's arguments or a result.

use crate::connectors::settings::KeepLog;
use crate::engine::EngineError;
use rusqlite::{Connection, params};

/// The most rows the log keeps, whatever the keep setting says.
pub const ROWS_MAX: i64 = 100_000;
/// The most rows one read returns.
pub const READ_MAX: usize = 500;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS connector_calls (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    connector TEXT NOT NULL CHECK(length(connector) <= 40),
    tool TEXT NOT NULL CHECK(length(tool) <= 128),
    kind TEXT NOT NULL CHECK(kind IN ('read','write','list','unknown')),
    project TEXT NOT NULL CHECK(length(project) <= 256),
    agent TEXT NOT NULL CHECK(length(agent) BETWEEN 1 AND 128),
    started_ms INTEGER NOT NULL,
    ms INTEGER NOT NULL CHECK(ms >= 0),
    bytes_in INTEGER NOT NULL CHECK(bytes_in >= 0),
    bytes_out INTEGER NOT NULL CHECK(bytes_out >= 0),
    outcome TEXT NOT NULL CHECK(length(outcome) BETWEEN 1 AND 64)
);
CREATE INDEX IF NOT EXISTS connector_calls_started ON connector_calls(started_ms);";

/// What a row is about: a tool the gateway counts as read or write, a tool list, or a
/// call that named no tool the gateway knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    Read,
    Write,
    List,
    Unknown,
}

impl CallKind {
    const ALL: [Self; 4] = [Self::Read, Self::Write, Self::List, Self::Unknown];

    /// The kind's stored name. Input: the kind. Output: the fixed name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::List => "list",
            Self::Unknown => "unknown",
        }
    }
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

fn sqlite(_: rusqlite::Error) -> EngineError {
    EngineError("connectors_unavailable".into())
}

/// Writes one row and drops what the keep setting and the row cap no longer hold. Inputs:
/// the engine database, the row and the keep setting. Output: Ok, or
/// `connectors_unavailable` when the row does not fit the log or the database fails.
pub fn record(db: &Connection, row: &CallRow, keep: KeepLog) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)?;
    let added = db
        .execute(
            "INSERT INTO connector_calls
                 (connector, tool, kind, project, agent, started_ms, ms, bytes_in, bytes_out, outcome)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                row.connector,
                row.tool,
                row.kind.name(),
                row.project,
                row.agent,
                row.started_ms,
                row.ms,
                row.bytes_in,
                row.bytes_out,
                row.outcome
            ],
        )
        .map_err(sqlite)?;
    if added != 1 {
        return Err(sqlite(rusqlite::Error::QueryReturnedNoRows));
    }
    let cutoff = crate::semantic::now_ms() - i64::from(keep.days()) * DAY_MS;
    db.execute(
        "DELETE FROM connector_calls WHERE started_ms < ?1",
        [cutoff],
    )
    .map_err(sqlite)?;
    db.execute(
        "DELETE FROM connector_calls
         WHERE id <= (SELECT MAX(id) FROM connector_calls) - ?1",
        [ROWS_MAX],
    )
    .map_err(sqlite)?;
    Ok(())
}

/// Reads the newest rows. Inputs: the engine database and how many, at most `READ_MAX`.
/// Output: the rows, newest first, or `connectors_unavailable`.
pub fn recent(db: &Connection, limit: usize) -> Result<Vec<CallRow>, EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)?;
    let limit = limit.min(READ_MAX);
    let mut statement = db
        .prepare(
            "SELECT connector, tool, kind, project, agent, started_ms, ms, bytes_in, bytes_out, outcome
             FROM connector_calls ORDER BY id DESC LIMIT ?1",
        )
        .map_err(sqlite)?;
    let rows = statement
        .query_map([limit as i64], |row| {
            let kind: String = row.get(2)?;
            Ok((
                kind,
                CallRow {
                    connector: row.get(0)?,
                    tool: row.get(1)?,
                    kind: CallKind::Unknown,
                    project: row.get(3)?,
                    agent: row.get(4)?,
                    started_ms: row.get(5)?,
                    ms: row.get(6)?,
                    bytes_in: row.get(7)?,
                    bytes_out: row.get(8)?,
                    outcome: row.get(9)?,
                },
            ))
        })
        .map_err(sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite)?;
    let mut found = Vec::with_capacity(rows.len());
    for (kind, mut row) in rows {
        row.kind = CallKind::ALL
            .into_iter()
            .find(|known| known.name() == kind)
            .ok_or_else(|| EngineError("connectors_unavailable".into()))?;
        found.push(row);
    }
    assert!(found.len() <= READ_MAX);
    Ok(found)
}
