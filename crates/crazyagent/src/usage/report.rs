use super::capture::Tokens;
use super::{AccountLimits, all_limits, failure, storage};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub const ROWS_MAX: usize = 25;
pub const DAYS_MAX: usize = 60;
pub const LIMITS_MAX: usize = 64;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const OFFSET_MINUTES_MAX: i32 = 14 * 60;

/// Tokens and runs counted against one project, pool, account or agent, and when it last
/// used any.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageRow {
    pub key: String,
    pub tokens: Tokens,
    pub runs: u64,
    pub last_ms: i64,
}

/// Tokens counted on one local day, which starts at `day_ms`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DayTokens {
    pub day_ms: i64,
    pub tokens: Tokens,
}

/// Lines one project's agents changed: across every change set not discarded, and across
/// the accepted ones, each landed as one commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinesRow {
    pub project: String,
    pub change_sets: u64,
    pub added: u64,
    pub removed: u64,
    pub commits: u64,
    pub committed_added: u64,
    pub committed_removed: u64,
}

/// Lines one agent changed, keyed like its token rows: `team-<id>` for a sub-agent and
/// `master-<project>` for the team master or an engine-driven master session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentLinesRow {
    pub agent: String,
    pub change_sets: u64,
    pub added: u64,
    pub removed: u64,
    pub commits: u64,
    pub committed_added: u64,
    pub committed_removed: u64,
}

/// The usage page's numbers for one range: app totals, the largest projects, pools,
/// accounts and agents, tokens per local day, lines changed per project, and every
/// account's limits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageReport {
    pub since_ms: Option<i64>,
    pub total: Tokens,
    pub runs: u64,
    pub projects: Vec<UsageRow>,
    pub pools: Vec<UsageRow>,
    pub accounts: Vec<UsageRow>,
    pub agents: Vec<UsageRow>,
    pub days: Vec<DayTokens>,
    pub lines: Vec<LinesRow>,
    pub agent_lines: Vec<AgentLinesRow>,
    pub limits: Vec<AccountLimits>,
}

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn tokens(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<Tokens> {
    Ok(Tokens {
        input: count(row.get(first)?),
        output: count(row.get(first + 1)?),
        cache_read: count(row.get(first + 2)?),
        cache_write: count(row.get(first + 3)?),
    })
}

fn rows(
    db: &Connection,
    column: &str,
    unattributed: bool,
    project: Option<&str>,
    since_ms: i64,
) -> Result<Vec<UsageRow>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT {column},SUM(input),SUM(output),SUM(cache_read),SUM(cache_write),\
                COUNT(DISTINCT agent||'/'||session),MAX(hour_ms)
             FROM usage_tokens WHERE hour_ms>=?1 AND (?3 OR {column}<>'')
                AND (?4 IS NULL OR project=?4)
             GROUP BY {column}
             ORDER BY SUM(input)+SUM(output)+SUM(cache_read)+SUM(cache_write) DESC, {column}
             LIMIT ?2"
        ))
        .map_err(storage)?;
    statement
        .query_map(
            params![since_ms, ROWS_MAX as i64, unattributed, project],
            |row| {
                Ok(UsageRow {
                    key: row.get(0)?,
                    tokens: tokens(row, 1)?,
                    runs: count(row.get(5)?),
                    last_ms: row.get(6)?,
                })
            },
        )
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)
}

fn has_table(db: &Connection, name: &str) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        params![name],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .map(|found| found.is_some())
    .map_err(storage)
}

fn lines(
    db: &Connection,
    project: Option<&str>,
    since_ms: i64,
) -> Result<Vec<LinesRow>, EngineError> {
    if !has_table(db, "landing_change_sets")? {
        return Ok(Vec::new());
    }
    let mut statement = db
        .prepare(
            "SELECT project,COUNT(*),SUM(added),SUM(removed),
                SUM(state='accepted'),
                SUM(CASE WHEN state='accepted' THEN added ELSE 0 END),
                SUM(CASE WHEN state='accepted' THEN removed ELSE 0 END)
             FROM landing_change_sets WHERE state<>'discarded' AND updated_ms>=?1
                AND (?3 IS NULL OR project=?3)
             GROUP BY project ORDER BY SUM(added)+SUM(removed) DESC, project LIMIT ?2",
        )
        .map_err(storage)?;
    statement
        .query_map(params![since_ms, ROWS_MAX as i64, project], |row| {
            Ok(LinesRow {
                project: row.get(0)?,
                change_sets: count(row.get(1)?),
                added: count(row.get(2)?),
                removed: count(row.get(3)?),
                commits: count(row.get(4)?),
                committed_added: count(row.get(5)?),
                committed_removed: count(row.get(6)?),
            })
        })
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)
}

fn agent_lines(
    db: &Connection,
    project: Option<&str>,
    since_ms: i64,
) -> Result<Vec<AgentLinesRow>, EngineError> {
    if !has_table(db, "landing_change_sets")? {
        return Ok(Vec::new());
    }
    let team = if has_table(db, "team_agents")? {
        "LEFT JOIN team_agents t ON l.agent_kind='sub' AND t.id=CAST(l.agent_id AS INTEGER)"
    } else {
        "LEFT JOIN (SELECT NULL AS id, NULL AS role) t ON 0"
    };
    let mut statement = db
        .prepare(&format!(
            "SELECT CASE WHEN l.agent_kind='master' OR t.role='master' THEN 'master-'||l.project
                    ELSE 'team-'||l.agent_id END AS agent,
                COUNT(*),SUM(l.added),SUM(l.removed),SUM(l.state='accepted'),
                SUM(CASE WHEN l.state='accepted' THEN l.added ELSE 0 END),
                SUM(CASE WHEN l.state='accepted' THEN l.removed ELSE 0 END)
             FROM landing_change_sets l {team}
             WHERE l.state<>'discarded' AND l.updated_ms>=?1 AND (?3 IS NULL OR l.project=?3)
             GROUP BY agent ORDER BY SUM(l.added)+SUM(l.removed) DESC, agent LIMIT ?2"
        ))
        .map_err(storage)?;
    statement
        .query_map(params![since_ms, ROWS_MAX as i64, project], |row| {
            Ok(AgentLinesRow {
                agent: row.get(0)?,
                change_sets: count(row.get(1)?),
                added: count(row.get(2)?),
                removed: count(row.get(3)?),
                commits: count(row.get(4)?),
                committed_added: count(row.get(5)?),
                committed_removed: count(row.get(6)?),
            })
        })
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)
}

/// Builds the usage page's numbers for the range starting at `since_ms`, None for all
/// time, with days cut at local midnight `offset_minutes` east of UTC, for one project or
/// every project. Inputs: the store, the project, the range start, the viewer's offset and
/// the time. Output: the report, at most `ROWS_MAX` rows a group, the latest `DAYS_MAX`
/// days and `LIMITS_MAX` accounts; `usage_request_invalid` for an offset beyond 14 hours,
/// or `usage_unavailable`.
pub fn report(
    db: &Connection,
    project: Option<&ProjectId>,
    since_ms: Option<i64>,
    offset_minutes: i32,
    now_ms: i64,
) -> Result<UsageReport, EngineError> {
    if offset_minutes.abs() > OFFSET_MINUTES_MAX || since_ms.is_some_and(|since| since > now_ms) {
        return Err(failure("usage_request_invalid"));
    }
    let since = since_ms.unwrap_or(i64::MIN);
    let project = project.map(ProjectId::as_str);
    let (total, runs) = db
        .query_row(
            "SELECT COALESCE(SUM(input),0),COALESCE(SUM(output),0),COALESCE(SUM(cache_read),0),\
                COALESCE(SUM(cache_write),0),COUNT(DISTINCT agent||'/'||session)
             FROM usage_tokens WHERE hour_ms>=?1 AND (?2 IS NULL OR project=?2)",
            params![since, project],
            |row| Ok((tokens(row, 0)?, count(row.get(4)?))),
        )
        .map_err(storage)?;
    let offset_ms = i64::from(offset_minutes) * 60_000;
    let mut days: Vec<DayTokens> = {
        let mut statement = db
            .prepare(
                "SELECT ((hour_ms+?2)/?3)*?3-?2 AS day,SUM(input),SUM(output),SUM(cache_read),\
                    SUM(cache_write)
                 FROM usage_tokens WHERE hour_ms>=?1 AND (?5 IS NULL OR project=?5)
                 GROUP BY day ORDER BY day DESC LIMIT ?4",
            )
            .map_err(storage)?;
        statement
            .query_map(
                params![since, offset_ms, DAY_MS, DAYS_MAX as i64, project],
                |row| {
                    Ok(DayTokens {
                        day_ms: row.get(0)?,
                        tokens: tokens(row, 1)?,
                    })
                },
            )
            .map_err(storage)?
            .collect::<Result<_, _>>()
            .map_err(storage)?
    };
    days.reverse();
    let mut limits = all_limits(db)?;
    limits.truncate(LIMITS_MAX);
    let report = UsageReport {
        since_ms,
        total,
        runs,
        projects: rows(db, "project", true, project, since)?,
        pools: rows(db, "pool", false, project, since)?,
        accounts: rows(db, "account", true, project, since)?,
        agents: rows(db, "agent", false, project, since)?,
        days,
        lines: lines(db, project, since)?,
        agent_lines: agent_lines(db, project, since)?,
        limits,
    };
    assert!(report.projects.len() <= ROWS_MAX && report.days.len() <= DAYS_MAX);
    assert!(report.agent_lines.len() <= ROWS_MAX);
    Ok(report)
}

#[cfg(all(test, unix))]
mod tests;
