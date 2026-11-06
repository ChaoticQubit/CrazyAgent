use super::capture::Tokens;
use super::{AccountLimits, all_limits, failure, storage};
use crate::engine::EngineError;
use crate::protocol::{CredentialRef, ProjectId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const ROWS_MAX: usize = 25;
pub const DAYS_MAX: usize = 60;
pub const LIMITS_MAX: usize = 64;
/// The most bytes an encoded report takes, leaving room in one reply frame for the
/// reply that carries it.
pub const REPORT_BYTES_MAX: usize = crate::protocol::FRAME_BYTES_MAX - 256;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const OFFSET_MINUTES_MAX: u32 = 14 * 60;
const SHARES: usize = 8;

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
/// accounts and agents, tokens per local day, lines changed per project, every account's
/// limits, and how many agent homes a scan in the range left records of uncounted.
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
    pub partial_homes: u64,
}

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Reads four `TOTAL()` token sums, which never overflow, saturating each into a count.
/// Inputs: the row and its first sum's column. Output: the tokens.
fn tokens(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<Tokens> {
    let amount = |column: usize| row.get::<_, f64>(column).map(|total| total as u64);
    Ok(Tokens {
        input: amount(first)?,
        output: amount(first + 1)?,
        cache_read: amount(first + 2)?,
        cache_write: amount(first + 3)?,
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
            "SELECT {column},TOTAL(input),TOTAL(output),TOTAL(cache_read),TOTAL(cache_write),\
                COUNT(DISTINCT agent||'/'||session),MAX(hour_ms)
             FROM usage_tokens WHERE hour_ms>=?1 AND (?3 OR {column}<>'')
                AND (?4 IS NULL OR project=?4)
             GROUP BY {column}
             ORDER BY TOTAL(input)+TOTAL(output)+TOTAL(cache_read)+TOTAL(cache_write) DESC, {column}
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

/// The saved accounts that have stored limits. Input: the store. Output: their ids, none
/// in a store without an accounts table, or `usage_unavailable`.
fn saved_accounts(db: &Connection) -> Result<BTreeSet<String>, EngineError> {
    if !has_table(db, "accounts")? {
        return Ok(BTreeSet::new());
    }
    let mut statement = db
        .prepare("SELECT id FROM accounts WHERE id IN (SELECT account FROM account_limits)")
        .map_err(storage)?;
    statement
        .query_map([], |row| row.get(0))
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

/// Keeps a report within `REPORT_BYTES_MAX` encoded bytes so its reply fits one frame:
/// while it is larger, every group keeps a smaller share of its cap, losing its least-used
/// rows, oldest days and last accounts' limits first. Input: the report.
/// Output: the report that fits, or `usage_unavailable`.
fn fit(mut report: UsageReport) -> Result<UsageReport, EngineError> {
    for share in (0..=SHARES).rev() {
        let kept = |cap: usize| cap * share / SHARES;
        for rows in [
            &mut report.projects,
            &mut report.pools,
            &mut report.accounts,
            &mut report.agents,
        ] {
            rows.truncate(kept(ROWS_MAX));
        }
        report.lines.truncate(kept(ROWS_MAX));
        report.agent_lines.truncate(kept(ROWS_MAX));
        let oldest = report.days.len().saturating_sub(kept(DAYS_MAX));
        report.days.drain(..oldest);
        report.limits.truncate(kept(LIMITS_MAX));
        let encoded = serde_json::to_vec(&report).map_err(|_| failure("usage_unavailable"))?;
        if encoded.len() <= REPORT_BYTES_MAX {
            return Ok(report);
        }
    }
    Err(failure("usage_unavailable"))
}

/// Builds the usage page's numbers for the range starting at `since_ms`, None for all
/// time, with days cut at local midnight `offset_minutes` east of UTC, for one project or
/// every project, with limits only for accounts that exist now: saved in the accounts
/// table or `held` by the live config. Inputs: the store, the project, the range start,
/// the viewer's offset, the time and the held accounts. Output: the report, at most
/// `ROWS_MAX` rows a group, the latest `DAYS_MAX` days and `LIMITS_MAX` accounts, trimmed
/// by `fit` to `REPORT_BYTES_MAX`; `usage_request_invalid` for an offset beyond 14 hours,
/// or `usage_unavailable`.
pub fn report(
    db: &Connection,
    project: Option<&ProjectId>,
    since_ms: Option<i64>,
    offset_minutes: i32,
    now_ms: i64,
    held: &[CredentialRef],
) -> Result<UsageReport, EngineError> {
    if offset_minutes.unsigned_abs() > OFFSET_MINUTES_MAX
        || since_ms.is_some_and(|since| since > now_ms)
    {
        return Err(failure("usage_request_invalid"));
    }
    let since = since_ms.unwrap_or(i64::MIN);
    let project = project.map(ProjectId::as_str);
    let (total, runs) = db
        .query_row(
            "SELECT TOTAL(input),TOTAL(output),TOTAL(cache_read),TOTAL(cache_write),\
                COUNT(DISTINCT agent||'/'||session)
             FROM usage_tokens WHERE hour_ms>=?1 AND (?2 IS NULL OR project=?2)",
            params![since, project],
            |row| Ok((tokens(row, 0)?, count(row.get(4)?))),
        )
        .map_err(storage)?;
    let offset_ms = i64::from(offset_minutes) * 60_000;
    let mut days: Vec<DayTokens> = {
        let mut statement = db
            .prepare(
                "SELECT ((hour_ms+?2)/?3)*?3-?2 AS day,TOTAL(input),TOTAL(output),\
                    TOTAL(cache_read),TOTAL(cache_write)
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
    let partial_homes = db
        .query_row(
            "SELECT COUNT(*) FROM usage_cut WHERE cut_ms>=?1 AND (?2 IS NULL OR project=?2)",
            params![since, project],
            |row| row.get(0),
        )
        .map(count)
        .map_err(storage)?;
    let saved = saved_accounts(db)?;
    let mut limits = all_limits(db)?;
    limits.retain(|row| held.contains(&row.account) || saved.contains(row.account.as_str()));
    limits.truncate(LIMITS_MAX);
    let report = fit(UsageReport {
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
        partial_homes,
    })?;
    assert!(report.projects.len() <= ROWS_MAX && report.days.len() <= DAYS_MAX);
    assert!(report.agent_lines.len() <= ROWS_MAX);
    Ok(report)
}

#[cfg(all(test, unix))]
pub(crate) mod tests;
