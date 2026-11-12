use super::storage;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, params};

const HOUR_MS: i64 = 60 * 60 * 1000;
/// The most turns one agent's hour row keeps.
pub const HOUR_TURNS_MAX: i64 = 1_000_000;

/// The turns each agent ended, one row per hour, project and usage agent key (`master-<project>`
/// or `team-<id>`, the key its tokens count against), so the Metrics range sums them as it sums
/// the token rows.
pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS usage_turns (
    hour_ms INTEGER NOT NULL,
    project TEXT NOT NULL,
    agent TEXT NOT NULL,
    turns INTEGER NOT NULL CHECK(turns >= 0),
    PRIMARY KEY (hour_ms, project, agent)
);";

/// Counts one turn an agent ended in the hour of `now_ms`, at most `HOUR_TURNS_MAX` a row.
/// Inputs: the store, the project, the agent's usage key and the time. Output: Ok, or the
/// store failure.
pub fn count(
    db: &Connection,
    project: &ProjectId,
    agent: &str,
    now_ms: i64,
) -> Result<(), EngineError> {
    assert!(!agent.is_empty());
    let hour_ms = now_ms - now_ms.rem_euclid(HOUR_MS);
    db.execute(
        "INSERT INTO usage_turns(hour_ms,project,agent,turns) VALUES (?1,?2,?3,1)
         ON CONFLICT(hour_ms,project,agent) DO UPDATE SET turns=MIN(turns+1,?4)",
        params![hour_ms, project.as_str(), agent, HOUR_TURNS_MAX],
    )
    .map_err(storage)?;
    Ok(())
}
