use super::capture::Tokens;
use super::report::{REPORT_BYTES_MAX, ROWS_MAX, SHARES, count, has_table, tokens};
use super::{failure, storage};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::team::events::name_reaches_running;
use crate::team::{AGENT_COLUMNS, AgentRole, AgentStatus, agent_row};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const HOUR_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;
const SERIES_MAX: usize = 64;
const TITLE_BYTES_MAX: usize = 120;
const NAMES_MAX: usize = 8;

/// The tokens of one chart bar: everything counted from `start_ms` until the next bar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bucket {
    pub start_ms: i64,
    pub tokens: Tokens,
}

/// One run: one agent session, with its tokens in the range, when they were last counted
/// (the start of its last usage hour for a run counted before that time was kept), and the
/// latest team task its agent ran while it used them (its id, the prompt's first line and
/// its status), when one did, and the name clients show for its agent, when the store has it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRow {
    pub project: String,
    pub agent: String,
    pub label: Option<String>,
    pub session: String,
    pub task: Option<i64>,
    pub title: Option<String>,
    pub status: Option<String>,
    pub last_ms: i64,
    pub tokens: Tokens,
}

/// The tokens one project, account or harness used in the range, with how many agents and
/// runs used it, the first `NAMES_MAX` projects they ran in, once each in name order, with
/// how many more there were, and for a project row the merges of the range as the Merges
/// tile counts them (zero for an account or harness row).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupRow {
    pub key: String,
    pub tokens: Tokens,
    pub agents: u64,
    pub runs: u64,
    pub projects: Vec<String>,
    pub more_projects: u64,
    #[serde(default)]
    pub merges: u64,
}

/// The tokens one agent used in the range, with the name clients show for it when the store
/// has it, and the first `NAMES_MAX` harnesses and accounts it used, once each in name order,
/// with how many more there were.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRow {
    pub project: String,
    pub agent: String,
    pub label: Option<String>,
    pub harnesses: Vec<String>,
    pub more_harnesses: u64,
    pub accounts: Vec<String>,
    pub more_accounts: u64,
    pub tokens: Tokens,
}

/// What the Metrics page counts besides the usage report: team tasks running now, waiting
/// now, and done or failed in the range; the merges in the range, those into main and those
/// undone; the tokens chart in bars of `bucket_ms`; and the breakdown by run, project, agent,
/// account and harness.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metrics {
    pub running: u64,
    pub tasks_waiting: u64,
    pub tasks_done: u64,
    pub tasks_failed: u64,
    #[serde(default)]
    pub merges: u64,
    #[serde(default)]
    pub merges_into_main: u64,
    #[serde(default)]
    pub merges_undone: u64,
    pub bucket_ms: i64,
    pub series: Vec<Bucket>,
    pub runs: Vec<RunRow>,
    pub projects: Vec<GroupRow>,
    pub agents: Vec<AgentRow>,
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

/// Counts the merges in the range (a lead's merge of a sub-agent's work and the master's merge
/// into main), those into main and those undone, from `project_activity`, the only record of a
/// lead's merge. That table keeps a project's newest 9,000 engine and owner events
/// (`activity::record`), so past that many in the range the oldest merges drop out of the count.
/// Inputs: the store, the project or None and the range start. Output: the counts, or zeros.
fn merge_counts(
    db: &Connection,
    project: Option<&str>,
    since_ms: i64,
) -> Result<[u64; 3], EngineError> {
    if !has_table(db, "project_activity")? {
        return Ok([0; 3]);
    }
    db.query_row(
        "SELECT COALESCE(SUM(kind IN ('lead_merged','merged_into_main')),0),
            COALESCE(SUM(kind='merged_into_main'),0),
            COALESCE(SUM(kind='merged_into_main' AND undone=1),0)
         FROM project_activity WHERE at_ms>=?2 AND (?1 IS NULL OR project=?1)",
        params![project, since_ms],
        |row| Ok([count(row.get(0)?), count(row.get(1)?), count(row.get(2)?)]),
    )
    .map_err(storage)
}

/// Gives each project row its merges in the range as `merge_counts` counts them, and adds a row
/// with no tokens, after the rows that used tokens and in name order, for a project with merges
/// and no tokens in the range, up to `ROWS_MAX` (25) rows. The rows add up to the Merges tile
/// only while every project with merges has a row: past `ROWS_MAX` such projects, or when `fit`
/// trims the rows to fit one reply, the tile counts merges that no row shows.
/// Inputs: the store, the project or None, the range start and the rows. Output: none.
fn project_merges(
    db: &Connection,
    project: Option<&str>,
    since_ms: i64,
    rows: &mut Vec<GroupRow>,
) -> Result<(), EngineError> {
    if !has_table(db, "project_activity")? {
        return Ok(());
    }
    let mut statement = db
        .prepare(
            "SELECT project,COUNT(*) FROM project_activity
             WHERE kind IN ('lead_merged','merged_into_main') AND at_ms>=?2
               AND (?1 IS NULL OR project=?1)
             GROUP BY project ORDER BY project",
        )
        .map_err(storage)?;
    let counts: Vec<(String, u64)> = statement
        .query_map(params![project, since_ms], |row| {
            Ok((row.get::<_, String>(0)?, count(row.get(1)?)))
        })
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)?;
    let index: HashMap<String, usize> = rows
        .iter()
        .enumerate()
        .map(|(at, row)| (row.key.clone(), at))
        .collect();
    for (key, merges) in counts {
        if let Some(&at) = index.get(&key) {
            rows[at].merges = merges;
        } else if rows.len() < ROWS_MAX {
            rows.push(GroupRow {
                projects: vec![key.clone()],
                key,
                merges,
                ..GroupRow::default()
            });
        }
    }
    assert!(rows.len() <= ROWS_MAX);
    Ok(())
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

/// The latest team task the run's agent ran while the run used tokens: started by the
/// run's last counted tokens, or by the end of its last usage hour for a run counted before
/// last-use times were kept, and not finished before its first usage hour. The engine keeps
/// no link from a task to its agent's harness session, only the agent and the task's start
/// and finish. A master run, or any agent key other than `team-<id>`, has none. Inputs: the
/// store, the run, the start of its first usage hour and the end of its window. Output: the
/// task's id, title and status, or None.
fn run_task(
    db: &Connection,
    run: &RunRow,
    first_ms: i64,
    until_ms: i64,
) -> Result<Option<(i64, String, String)>, EngineError> {
    assert!(first_ms <= until_ms);
    assert!(run.last_ms <= until_ms);
    let Some(agent) = run
        .agent
        .strip_prefix("team-")
        .and_then(|id| id.parse::<i64>().ok())
    else {
        return Ok(None);
    };
    db.query_row(
        "SELECT id,prompt,status FROM team_tasks
         WHERE project=?1 AND agent=?2 AND started_ms IS NOT NULL AND started_ms<=?4
            AND (finished_ms IS NULL OR finished_ms>=?3)
         ORDER BY started_ms DESC,id DESC LIMIT 1",
        params![run.project, agent, first_ms, until_ms],
        |row| Ok((row.get(0)?, title(&row.get::<_, String>(1)?), row.get(2)?)),
    )
    .optional()
    .map_err(storage)
}

/// The name every client shows for a usage agent key, read in this one read so the app page
/// needs no team status per project: "Master" for a project's master, a sub-agent's kept
/// persona and role handle or its number, the number only for a finished sub-agent whose
/// name a running one now holds (DEC-111 Q19), and None for an agent the store no longer has.
/// Inputs: the store, the project and the agent key. Output: the name, or None.
fn agent_name(db: &Connection, project: &str, key: &str) -> Result<Option<String>, EngineError> {
    if key.starts_with("master-") {
        return Ok(Some(AgentRole::Master.agent_label(0)));
    }
    let Some(id) = key
        .strip_prefix("team-")
        .and_then(|id| id.parse::<i64>().ok())
    else {
        return Ok(None);
    };
    if !has_table(db, "team_agents")? || !has_table(db, "team_names")? {
        return Ok(None);
    }
    let Some(agent) = db
        .query_row(
            &format!("SELECT {AGENT_COLUMNS} FROM team_agents WHERE id=?1 AND project=?2"),
            params![id, project],
            agent_row,
        )
        .optional()
        .map_err(storage)?
    else {
        return Ok(None);
    };
    let finished = matches!(agent.status, AgentStatus::Stopped | AgentStatus::Failed);
    let reused = agent.role == AgentRole::Sub
        && finished
        && match ProjectId::new(project) {
            Ok(project) => name_reaches_running(db, &project, id)?,
            Err(_) => false,
        };
    Ok(Some(if reused {
        AgentRole::Sub.agent_label(id)
    } else {
        agent.label
    }))
}

fn runs(db: &Connection, project: Option<&str>, since_ms: i64) -> Result<Vec<RunRow>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT u.project,u.agent,u.session,\
                MAX(MAX(u.hour_ms),COALESCE(MAX(s.last_ms),0)) AS last,MIN(u.hour_ms),\
                TOTAL(u.input),TOTAL(u.output),TOTAL(u.cache_read),TOTAL(u.cache_write),\
                MAX(s.last_ms)
             FROM usage_tokens u LEFT JOIN usage_seen s
                ON s.project=u.project AND s.agent=u.agent AND s.session=u.session
             WHERE u.hour_ms>=?2 AND (?1 IS NULL OR u.project=?1)
             GROUP BY u.project,u.agent,u.session ORDER BY last DESC,u.agent,u.session LIMIT ?3",
        )
        .map_err(storage)?;
    let mut runs: Vec<(RunRow, i64, i64)> = statement
        .query_map(params![project, since_ms, ROWS_MAX as i64], |row| {
            let last_ms: i64 = row.get(3)?;
            let seen: Option<i64> = row.get(9)?;
            Ok((
                RunRow {
                    project: row.get(0)?,
                    agent: row.get(1)?,
                    session: row.get(2)?,
                    task: None,
                    label: None,
                    title: None,
                    status: None,
                    last_ms,
                    tokens: tokens(row, 5)?,
                },
                row.get(4)?,
                seen.map_or(last_ms.saturating_add(HOUR_MS - 1), |_| last_ms),
            ))
        })
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)?;
    if has_table(db, "team_tasks")? {
        for (run, first_ms, until_ms) in &mut runs {
            if let Some((task, title, status)) = run_task(db, run, *first_ms, *until_ms)? {
                run.task = Some(task);
                run.title = Some(title);
                run.status = Some(status);
            }
        }
    }
    for (run, _, _) in &mut runs {
        run.label = agent_name(db, &run.project, &run.agent)?;
    }
    Ok(runs.into_iter().map(|(run, _, _)| run).collect())
}

/// The first `NAMES_MAX` names of a list in name order, once each, and how many more there
/// were. Input: the names. Output: the names kept and the count of the rest.
fn capped(mut names: Vec<String>) -> (Vec<String>, u64) {
    names.sort();
    names.dedup();
    let more = names.len().saturating_sub(NAMES_MAX);
    names.truncate(NAMES_MAX);
    (names, more as u64)
}

fn groups(
    db: &Connection,
    column: &str,
    project: Option<&str>,
    since_ms: i64,
) -> Result<Vec<GroupRow>, EngineError> {
    assert!(matches!(column, "project" | "account" | "harness"));
    let mut statement = db
        .prepare(&format!(
            "SELECT {column},TOTAL(input),TOTAL(output),TOTAL(cache_read),TOTAL(cache_write),\
                COUNT(DISTINCT agent),COUNT(DISTINCT agent||'/'||session)
             FROM usage_tokens WHERE hour_ms>=?2 AND (?1 IS NULL OR project=?1)
             GROUP BY {column}
             ORDER BY TOTAL(input)+TOTAL(output)+TOTAL(cache_write) DESC,{column} LIMIT ?3"
        ))
        .map_err(storage)?;
    let mut rows: Vec<GroupRow> = statement
        .query_map(params![project, since_ms, ROWS_MAX as i64], |row| {
            Ok(GroupRow {
                key: row.get(0)?,
                tokens: tokens(row, 1)?,
                agents: count(row.get(5)?),
                runs: count(row.get(6)?),
                projects: Vec::new(),
                more_projects: 0,
                merges: 0,
            })
        })
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)?;
    let index: HashMap<String, usize> = rows
        .iter()
        .enumerate()
        .map(|(at, row)| (row.key.clone(), at))
        .collect();
    let mut statement = db
        .prepare(&format!(
            "SELECT DISTINCT {column},project FROM usage_tokens
             WHERE hour_ms>=?2 AND (?1 IS NULL OR project=?1) ORDER BY project"
        ))
        .map_err(storage)?;
    let pairs = statement
        .query_map(params![project, since_ms], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(storage)?;
    for pair in pairs {
        let (key, name) = pair.map_err(storage)?;
        if let Some(&at) = index.get(&key) {
            rows[at].projects.push(name);
        }
    }
    for row in &mut rows {
        (row.projects, row.more_projects) = capped(std::mem::take(&mut row.projects));
    }
    Ok(rows)
}

/// The largest `ROWS_MAX` agents by the headline, each with the harnesses and accounts it used,
/// once each in name order. Inputs: the store, the project or None, and the range start.
/// Output: the rows.
fn agents(
    db: &Connection,
    project: Option<&str>,
    since_ms: i64,
) -> Result<Vec<AgentRow>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT project,agent,TOTAL(input),TOTAL(output),TOTAL(cache_read),TOTAL(cache_write)
             FROM usage_tokens WHERE hour_ms>=?2 AND (?1 IS NULL OR project=?1)
             GROUP BY project,agent
             ORDER BY TOTAL(input)+TOTAL(output)+TOTAL(cache_write) DESC,project,agent LIMIT ?3",
        )
        .map_err(storage)?;
    let mut rows: Vec<AgentRow> = statement
        .query_map(params![project, since_ms, ROWS_MAX as i64], |row| {
            Ok(AgentRow {
                project: row.get(0)?,
                agent: row.get(1)?,
                label: None,
                harnesses: Vec::new(),
                more_harnesses: 0,
                accounts: Vec::new(),
                more_accounts: 0,
                tokens: tokens(row, 2)?,
            })
        })
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)?;
    let index: HashMap<(String, String), usize> = rows
        .iter()
        .enumerate()
        .map(|(at, row)| ((row.project.clone(), row.agent.clone()), at))
        .collect();
    let mut statement = db
        .prepare(
            "SELECT DISTINCT project,agent,harness,account FROM usage_tokens
             WHERE hour_ms>=?2 AND (?1 IS NULL OR project=?1)",
        )
        .map_err(storage)?;
    let used = statement
        .query_map(params![project, since_ms], |row| {
            Ok((
                (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(storage)?;
    for entry in used {
        let (key, harness, account) = entry.map_err(storage)?;
        if let Some(&at) = index.get(&key) {
            rows[at].harnesses.push(harness);
            rows[at].accounts.push(account);
        }
    }
    for row in &mut rows {
        (row.harnesses, row.more_harnesses) = capped(std::mem::take(&mut row.harnesses));
        (row.accounts, row.more_accounts) = capped(std::mem::take(&mut row.accounts));
        row.label = agent_name(db, &row.project, &row.agent)?;
    }
    Ok(rows)
}

/// Keeps the metrics within `REPORT_BYTES_MAX` encoded bytes so their reply fits one frame:
/// while they are larger, runs, projects, agents, accounts and harnesses each keep a smaller
/// share of `ROWS_MAX`, losing the oldest runs and the least-used rows first.
/// Input: the metrics. Output: the metrics that fit, or `usage_unavailable`.
fn fit(mut metrics: Metrics) -> Result<Metrics, EngineError> {
    for share in (0..=SHARES).rev() {
        let kept = ROWS_MAX * share / SHARES;
        metrics.runs.truncate(kept);
        metrics.projects.truncate(kept);
        metrics.agents.truncate(kept);
        metrics.accounts.truncate(kept);
        metrics.harnesses.truncate(kept);
        let encoded = serde_json::to_vec(&metrics).map_err(|_| failure("usage_unavailable"))?;
        if encoded.len() <= REPORT_BYTES_MAX {
            return Ok(metrics);
        }
    }
    Err(failure("usage_unavailable"))
}

/// Reads the Metrics page's counts for one project, None for every project, over the range
/// from `since_ms` to `now_ms`: tasks running and waiting now, done and failed in the range,
/// the tokens chart in bars `bucket_for` the range, anchored at its start, the newest
/// `ROWS_MAX` runs, and the largest `ROWS_MAX` projects, agents, accounts and harnesses by the
/// headline, fewer of each when they would not fit one reply (`fit`).
/// Inputs: the store, the project, the range start and now. Output: the metrics;
/// `usage_request_invalid` for a range that starts before the epoch or after now.
pub fn read(
    db: &Connection,
    project: Option<&ProjectId>,
    since_ms: i64,
    now_ms: i64,
) -> Result<Metrics, EngineError> {
    if since_ms < 0 || since_ms > now_ms {
        return Err(failure("usage_request_invalid"));
    }
    let span_ms = now_ms
        .checked_sub(since_ms)
        .ok_or_else(|| failure("usage_request_invalid"))?;
    let project = project.map(ProjectId::as_str);
    let [running, tasks_waiting, tasks_done, tasks_failed] = task_counts(db, project, since_ms)?;
    let [merges, merges_into_main, merges_undone] = merge_counts(db, project, since_ms)?;
    let bucket_ms = bucket_for(span_ms);
    let mut projects = groups(db, "project", project, since_ms)?;
    project_merges(db, project, since_ms, &mut projects)?;
    let metrics = fit(Metrics {
        running,
        tasks_waiting,
        tasks_done,
        tasks_failed,
        merges,
        merges_into_main,
        merges_undone,
        bucket_ms,
        series: series(db, project, since_ms, bucket_ms)?,
        runs: runs(db, project, since_ms)?,
        projects,
        agents: agents(db, project, since_ms)?,
        accounts: groups(db, "account", project, since_ms)?,
        harnesses: groups(db, "harness", project, since_ms)?,
    })?;
    assert!(metrics.bucket_ms > 0 && metrics.series.len() <= SERIES_MAX);
    assert!(metrics.runs.len() <= ROWS_MAX);
    assert!(metrics.accounts.len() <= ROWS_MAX && metrics.harnesses.len() <= ROWS_MAX);
    assert!(metrics.projects.len() <= ROWS_MAX && metrics.agents.len() <= ROWS_MAX);
    assert!(metrics.agents.iter().all(|row| {
        row.harnesses.is_sorted() && row.accounts.is_sorted() && !row.harnesses.is_empty()
    }));
    assert!(
        metrics
            .agents
            .iter()
            .all(|row| { row.harnesses.len() <= NAMES_MAX && row.accounts.len() <= NAMES_MAX })
    );
    assert!(
        metrics
            .projects
            .iter()
            .chain(&metrics.accounts)
            .chain(&metrics.harnesses)
            .all(|row| row.projects.len() <= NAMES_MAX)
    );
    assert!(metrics.runs.iter().all(|run| {
        run.title
            .as_ref()
            .is_none_or(|title| title.len() <= TITLE_BYTES_MAX)
    }));
    Ok(metrics)
}

#[cfg(all(test, unix))]
mod tests;
