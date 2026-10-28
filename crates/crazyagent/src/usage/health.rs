use super::{failure, storage};
use crate::engine::EngineError;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS usage_health (
    step TEXT PRIMARY KEY CHECK(step IN ('poll','team','runs')),
    panics INTEGER NOT NULL CHECK(panics >= 1),
    last_ms INTEGER NOT NULL
);";

/// One part of a usage upkeep step: checking the accounts' limits, counting the team
/// agents' records, or counting the research runs' records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageStep {
    Poll,
    Team,
    Runs,
}

impl UsageStep {
    /// The stored name of the step. Output: `poll`, `team` or `runs`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Poll => "poll",
            Self::Team => "team",
            Self::Runs => "runs",
        }
    }

    fn named(name: &str) -> Option<Self> {
        [Self::Poll, Self::Team, Self::Runs]
            .into_iter()
            .find(|step| step.name() == name)
    }
}

/// How many times one kind of usage step panicked since the engine started, and when it
/// last did, so the usage pages can say counting is degraded instead of staying silent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepHealth {
    pub step: UsageStep,
    pub panics: u64,
    pub last_ms: i64,
}

/// Counts one panic of a usage step kind. Inputs: the store, the step that was running
/// and the time. Output: done, or `usage_unavailable`.
pub fn record_panic(db: &Connection, step: UsageStep, now_ms: i64) -> Result<(), EngineError> {
    db.execute(
        "INSERT INTO usage_health (step,panics,last_ms) VALUES (?1,1,?2)
         ON CONFLICT(step) DO UPDATE SET panics=panics+1,last_ms=MAX(last_ms,excluded.last_ms)",
        params![step.name(), now_ms],
    )
    .map_err(storage)?;
    Ok(())
}

/// Forgets every counted panic, as a new engine run starts. Input: the store. Output:
/// done, or `usage_unavailable`.
pub fn clear(db: &Connection) -> Result<(), EngineError> {
    db.execute("DELETE FROM usage_health", [])
        .map_err(storage)?;
    Ok(())
}

/// Reads every step kind that panicked since the engine started, in step order. Input:
/// the store. Output: the rows, or `usage_unavailable`.
pub fn health(db: &Connection) -> Result<Vec<StepHealth>, EngineError> {
    let mut statement = db
        .prepare("SELECT step,panics,last_ms FROM usage_health")
        .map_err(storage)?;
    let mut rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(storage)?
        .map(|row| {
            let (step, panics, last_ms) = row.map_err(storage)?;
            Ok(StepHealth {
                step: UsageStep::named(&step).ok_or_else(|| failure("usage_unavailable"))?,
                panics: u64::try_from(panics).map_err(storage)?,
                last_ms,
            })
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    rows.sort_by_key(|row| row.step);
    Ok(rows)
}
