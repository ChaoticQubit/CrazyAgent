use super::capture::Tokens;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// The tokens of one chart bar: everything counted from `start_ms` until the next bar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bucket {
    pub start_ms: i64,
    pub tokens: Tokens,
}

/// One run: one agent session, with its tokens in the range, when it last used any, and
/// the latest team task that ran in it (its id, the prompt's first line and its status),
/// when one did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRow {
    pub agent: String,
    pub session: String,
    pub task: Option<i64>,
    pub title: Option<String>,
    pub status: Option<String>,
    pub last_ms: i64,
    pub tokens: Tokens,
}

/// The tokens one account or harness used in the range, with how many agents and runs used
/// it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupRow {
    pub key: String,
    pub tokens: Tokens,
    pub agents: u64,
    pub runs: u64,
}

/// What the Metrics page counts besides the usage report: team tasks running now, waiting
/// now, and done or failed in the range; the tokens chart in bars of `bucket_ms`; and the
/// breakdown by run, account and harness.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metrics {
    pub running: u64,
    pub tasks_waiting: u64,
    pub tasks_done: u64,
    pub tasks_failed: u64,
    pub bucket_ms: i64,
    pub series: Vec<Bucket>,
    pub runs: Vec<RunRow>,
    pub accounts: Vec<GroupRow>,
    pub harnesses: Vec<GroupRow>,
}

/// Reads the Metrics page's counts for one project, None for every project, over the range
/// from `since_ms` to `now_ms`. Inputs: the store, the project, the range start and now.
/// Output: the metrics.
pub fn read(
    db: &Connection,
    project: Option<&ProjectId>,
    since_ms: i64,
    now_ms: i64,
) -> Result<Metrics, EngineError> {
    let _ = (db, project, since_ms, now_ms);
    Ok(Metrics::default())
}

#[cfg(all(test, unix))]
mod tests;
