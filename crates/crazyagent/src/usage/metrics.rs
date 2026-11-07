use super::capture::Tokens;
use super::report::{ROWS_MAX, count, has_table, tokens};
use super::{failure, storage};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

const HOUR_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;
const SERIES_MAX: usize = 64;
const TITLE_BYTES_MAX: usize = 120;

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
    pub project: String,
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

/// The chart bar width for a range: two hours for up to a day, a day for up to eight days,
/// five days beyond. Input: the range length. Output: the width in milliseconds.
fn bucket_for(span_ms: i64) -> i64 {
    if span_ms <= DAY_MS {
        2 * HOUR_MS
    } else if span_ms <= 8 * DAY_MS {
        DAY_MS
    } else {
        5 * DAY_MS
    }
}

/// The first line of a task prompt, trimmed and cut to `TITLE_BYTES_MAX` at a character
/// boundary. Input: the prompt. Output: the title.
fn title(prompt: &str) -> String {
    let line = prompt.lines().next().unwrap_or_default().trim();
    let mut end = line.len().min(TITLE_BYTES_MAX);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_owned()
}

fn task_counts(
    db: &Connection,
    project: Option<&str>,
    since_ms: i64,
) -> Result<[u64; 4], EngineError> {
    if !has_table(db, "team_tasks")? {
        return Ok([0; 4]);
    }
    db.query_row(
        "SELECT COALESCE(SUM(status='running'),0),
            COALESCE(SUM(status IN ('queued','needs_pool')),0),
            COALESCE(SUM(status='done' AND finished_ms>=?2),0),
            COALESCE(SUM(status='failed' AND finished_ms>=?2),0)
         FROM team_tasks WHERE ?1 IS NULL OR project=?1",
        params![project, since_ms],
        |row| {
            Ok([
                count(row.get(0)?),
                count(row.get(1)?),
                count(row.get(2)?),
                count(row.get(3)?),
            ])
        },
    )
    .map_err(storage)
}

fn series(
    db: &Connection,
    project: Option<&str>,
    since_ms: i64,
    bucket_ms: i64,
) -> Result<Vec<Bucket>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT ?2+((hour_ms-?2)/?3)*?3 AS start,TOTAL(input),TOTAL(output),\
                TOTAL(cache_read),TOTAL(cache_write)
             FROM usage_tokens WHERE hour_ms>=?2 AND (?1 IS NULL OR project=?1)
             GROUP BY start ORDER BY start DESC LIMIT ?4",
        )
        .map_err(storage)?;
    let mut bars: Vec<Bucket> = statement
        .query_map(
            params![project, since_ms, bucket_ms, SERIES_MAX as i64],
            |row| {
                Ok(Bucket {
                    start_ms: row.get(0)?,
                    tokens: tokens(row, 1)?,
                })
            },
        )
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)?;
    bars.reverse();
    Ok(bars)
}

/// The latest team task of a run's agent that ran in its session: the session itself
/// (Claude) or the end of a Codex rollout name after a `-`. Inputs: the store and the run.
/// Output: its id, title and status, or None.
fn run_task(db: &Connection, run: &RunRow) -> Result<Option<(i64, String, String)>, EngineError> {
    db.query_row(
        "SELECT id,prompt,status FROM team_tasks
         WHERE project=?1 AND 'team-'||agent=?2 AND session IS NOT NULL AND session<>''
            AND (session=?3 OR substr(?3,-length(session)-1)='-'||session)
         ORDER BY id DESC LIMIT 1",
        params![run.project, run.agent, run.session],
        |row| Ok((row.get(0)?, title(&row.get::<_, String>(1)?), row.get(2)?)),
    )
    .optional()
    .map_err(storage)
}

fn runs(db: &Connection, project: Option<&str>, since_ms: i64) -> Result<Vec<RunRow>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT project,agent,session,MAX(hour_ms) AS last,TOTAL(input),TOTAL(output),\
                TOTAL(cache_read),TOTAL(cache_write)
             FROM usage_tokens WHERE hour_ms>=?2 AND (?1 IS NULL OR project=?1)
             GROUP BY project,agent,session ORDER BY last DESC,agent,session LIMIT ?3",
        )
        .map_err(storage)?;
    let mut runs: Vec<RunRow> = statement
        .query_map(params![project, since_ms, ROWS_MAX as i64], |row| {
            Ok(RunRow {
                project: row.get(0)?,
                agent: row.get(1)?,
                session: row.get(2)?,
                task: None,
                title: None,
                status: None,
                last_ms: row.get(3)?,
                tokens: tokens(row, 4)?,
            })
        })
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)?;
    if has_table(db, "team_tasks")? {
        for run in &mut runs {
            if let Some((task, title, status)) = run_task(db, run)? {
                run.task = Some(task);
                run.title = Some(title);
                run.status = Some(status);
            }
        }
    }
    Ok(runs)
}

fn groups(
    db: &Connection,
    column: &str,
    project: Option<&str>,
    since_ms: i64,
) -> Result<Vec<GroupRow>, EngineError> {
    assert!(matches!(column, "account" | "harness"));
    let mut statement = db
        .prepare(&format!(
            "SELECT {column},TOTAL(input),TOTAL(output),TOTAL(cache_read),TOTAL(cache_write),\
                COUNT(DISTINCT agent),COUNT(DISTINCT agent||'/'||session)
             FROM usage_tokens WHERE hour_ms>=?2 AND (?1 IS NULL OR project=?1)
             GROUP BY {column}
             ORDER BY TOTAL(input)+TOTAL(output)+TOTAL(cache_write) DESC,{column} LIMIT ?3"
        ))
        .map_err(storage)?;
    statement
        .query_map(params![project, since_ms, ROWS_MAX as i64], |row| {
            Ok(GroupRow {
                key: row.get(0)?,
                tokens: tokens(row, 1)?,
                agents: count(row.get(5)?),
                runs: count(row.get(6)?),
            })
        })
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)
}

/// Reads the Metrics page's counts for one project, None for every project, over the range
/// from `since_ms` to `now_ms`: tasks running and waiting now, done and failed in the range,
/// the tokens chart in bars `bucket_for` the range, anchored at its start, and the newest
/// `ROWS_MAX` runs and largest `ROWS_MAX` accounts and harnesses by the headline. Inputs:
/// the store, the project, the range start and now. Output: the metrics;
/// `usage_request_invalid` for a range that starts after now.
pub fn read(
    db: &Connection,
    project: Option<&ProjectId>,
    since_ms: i64,
    now_ms: i64,
) -> Result<Metrics, EngineError> {
    if since_ms > now_ms {
        return Err(failure("usage_request_invalid"));
    }
    let project = project.map(ProjectId::as_str);
    let [running, tasks_waiting, tasks_done, tasks_failed] = task_counts(db, project, since_ms)?;
    let bucket_ms = bucket_for(now_ms - since_ms);
    let metrics = Metrics {
        running,
        tasks_waiting,
        tasks_done,
        tasks_failed,
        bucket_ms,
        series: series(db, project, since_ms, bucket_ms)?,
        runs: runs(db, project, since_ms)?,
        accounts: groups(db, "account", project, since_ms)?,
        harnesses: groups(db, "harness", project, since_ms)?,
    };
    assert!(metrics.bucket_ms > 0 && metrics.series.len() <= SERIES_MAX);
    assert!(metrics.runs.len() <= ROWS_MAX);
    assert!(metrics.accounts.len() <= ROWS_MAX && metrics.harnesses.len() <= ROWS_MAX);
    assert!(metrics.runs.iter().all(|run| {
        run.title
            .as_ref()
            .is_none_or(|title| title.len() <= TITLE_BYTES_MAX)
    }));
    Ok(metrics)
}

#[cfg(all(test, unix))]
mod tests;
