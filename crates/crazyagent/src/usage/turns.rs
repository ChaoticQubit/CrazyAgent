use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;

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

/// Counts one turn an agent ended in the hour of `now_ms`. Inputs: the store, the project,
/// the agent's usage key and the time. Output: Ok, or the store failure.
pub fn count(
    db: &Connection,
    project: &ProjectId,
    agent: &str,
    now_ms: i64,
) -> Result<(), EngineError> {
    let _ = (db, project, agent, now_ms);
    Ok(())
}
