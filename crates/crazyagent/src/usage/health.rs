use super::{failure, storage};
use crate::engine::EngineError;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS usage_health (
    step TEXT PRIMARY KEY CHECK(step IN ('poll','team','runs','recount')),
    panics INTEGER NOT NULL CHECK(panics >= 1),
    last_ms INTEGER NOT NULL,
    recovered_ms INTEGER CHECK(recovered_ms IS NULL OR recovered_ms >= last_ms)
);";

/// One part of a usage upkeep step: checking the accounts' limits, counting the team
/// agents' records, counting the research runs' records, or recounting history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageStep {
    Poll,
    Team,
    Runs,
    Recount,
}

impl UsageStep {
    /// The stored name of the step. Output: `poll`, `team`, `runs` or `recount`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Poll => "poll",
            Self::Team => "team",
            Self::Runs => "runs",
            Self::Recount => "recount",
        }
    }

    fn named(name: &str) -> Option<Self> {
        [Self::Poll, Self::Team, Self::Runs, Self::Recount]
            .into_iter()
            .find(|step| step.name() == name)
    }
}

/// How many times one kind of usage step panicked since the engine started, when it last
/// did, and when that kind first completed again after its last panic, none while it has
/// not, so the usage pages can say counting is degraded or has recovered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepHealth {
    pub step: UsageStep,
    pub panics: u64,
    pub last_ms: i64,
    pub recovered_ms: Option<i64>,
}

/// Drops a table an older build made, whose CHECK refuses the recount step. Its rows last one
/// engine run, so nothing kept is lost. Input: the store. Output: done, or `usage_unavailable`.
pub(super) fn upgrade(db: &Connection) -> Result<(), EngineError> {
    let older: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='usage_health' AND sql NOT LIKE '%recount%')",
            [],
            |row| row.get(0),
        )
        .map_err(storage)?;
    if older {
        db.execute_batch("DROP TABLE usage_health")
            .map_err(storage)?;
    }
    Ok(())
}

/// Counts one panic of a usage step kind, which is then no longer recovered. Inputs: the
/// store, the step that was running and the time. Output: done, or `usage_unavailable`.
pub fn record_panic(db: &Connection, step: UsageStep, now_ms: i64) -> Result<(), EngineError> {
    db.execute(
        "INSERT INTO usage_health (step,panics,last_ms) VALUES (?1,1,?2)
         ON CONFLICT(step) DO UPDATE SET panics=panics+1,last_ms=MAX(last_ms,excluded.last_ms),
            recovered_ms=NULL",
        params![step.name(), now_ms],
    )
    .map_err(storage)?;
    Ok(())
}

/// Records that a usage step kind completed again after its last panic, keeping the first
/// such time; a kind that never panicked is left without a row. Inputs: the store, the
/// step and the time. Output: done, or `usage_unavailable`.
pub fn record_recovered(db: &Connection, step: UsageStep, now_ms: i64) -> Result<(), EngineError> {
    db.execute(
        "UPDATE usage_health SET recovered_ms=MAX(?2,last_ms)
         WHERE step=?1 AND recovered_ms IS NULL",
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
        .prepare("SELECT step,panics,last_ms,recovered_ms FROM usage_health")
        .map_err(storage)?;
    let mut rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })
        .map_err(storage)?
        .map(|row| {
            let (step, panics, last_ms, recovered_ms) = row.map_err(storage)?;
            Ok(StepHealth {
                step: UsageStep::named(&step).ok_or_else(|| failure("usage_unavailable"))?,
                panics: u64::try_from(panics).map_err(storage)?,
                last_ms,
                recovered_ms,
            })
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    rows.sort_by_key(|row| row.step);
    Ok(rows)
}
