use super::capture::{Attribution, Format, Tokens};
use super::{AccountLimits, all_limits, failure, storage};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub const ROWS_MAX: usize = 25;
pub const DAYS_MAX: usize = 60;
pub const LIMITS_MAX: usize = 64;
pub const HARNESSES_MAX: usize = 32;
const HOUR_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;
const OFFSET_MINUTES_MAX: i32 = 14 * 60;
const AGENT_BYTES_MAX: usize = 192;
const HARNESS_BYTES_MAX: usize = 128;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS usage_unreported (
    hour_ms INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('master','agent','research')),
    project TEXT NOT NULL,
    agent TEXT NOT NULL,
    harness TEXT NOT NULL,
    last_ms INTEGER NOT NULL,
    PRIMARY KEY (hour_ms, kind, project, agent, harness)
);
CREATE INDEX IF NOT EXISTS usage_unreported_hour ON usage_unreported(hour_ms);";

/// Tokens and runs counted against one project, pool, account or agent, and when it last
/// used any. `not_reported_by` names a harness that also ran for it but reports no usage,
/// so its tokens are missing from the row rather than zero.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageRow {
    pub key: String,
    pub tokens: Tokens,
    pub runs: u64,
    pub last_ms: i64,
    #[serde(default)]
    pub not_reported_by: Option<String>,
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
/// limits, and the harnesses that ran in the range without reporting usage.
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
    #[serde(default)]
    pub not_reported_by: Vec<String>,
}

/// Marks that a run of a harness the engine has no usage reader for worked in the hour of
/// `now_ms`, so the report says its usage is not reported instead of showing zero. Inputs:
/// the store, the attribution and the time. Output: done, `usage_attribution_invalid` for a
/// harness with a reader or a missing or oversized agent or harness, or `usage_unavailable`.
pub fn mark_unreported(
    db: &Connection,
    attribution: &Attribution,
    now_ms: i64,
) -> Result<(), EngineError> {
    if attribution.agent.is_empty()
        || attribution.agent.len() > AGENT_BYTES_MAX
        || attribution.harness.is_empty()
        || attribution.harness.len() > HARNESS_BYTES_MAX
        || Format::of(&attribution.harness).is_some()
    {
        return Err(failure("usage_attribution_invalid"));
    }
    db.execute(
        "INSERT INTO usage_unreported (hour_ms,kind,project,agent,harness,last_ms)
         VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(hour_ms,kind,project,agent,harness)
         DO UPDATE SET last_ms=MAX(last_ms,excluded.last_ms)",
        params![
            now_ms - now_ms.rem_euclid(HOUR_MS),
            attribution.kind.name(),
            attribution.project.as_ref().map_or("", ProjectId::as_str),
            attribution.agent,
            attribution.harness,
            now_ms,
        ],
    )
    .map_err(storage)?;
    Ok(())
}

/// The name a harness id shows under: its catalog name, or the id itself.
fn harness_name(id: &str) -> String {
    crate::harnesses::spec(id).map_or_else(|| id.to_owned(), |spec| spec.name.to_owned())
}

/// Flags the rows whose key ran a harness without a reader in the range, newest harness
/// first, and adds a row for each such key that counted no tokens, after the counted rows.
/// Inputs: the store, the rows, the grouping column, the project or none, the range start.
/// Output: the rows, at most `2 * ROWS_MAX`.
fn flag_unreported(
    db: &Connection,
    mut rows: Vec<UsageRow>,
    column: &str,
    project: Option<&str>,
    since_ms: i64,
) -> Result<Vec<UsageRow>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT {column},harness,MAX(last_ms) FROM usage_unreported
             WHERE hour_ms>=?1 AND (?2 IS NULL OR project=?2)
             GROUP BY {column},harness ORDER BY MAX(last_ms) DESC,{column},harness LIMIT ?3"
        ))
        .map_err(storage)?;
    let marks: Vec<(String, String, i64)> = statement
        .query_map(params![since_ms, project, (2 * ROWS_MAX) as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)?;
    let counted = rows.len();
    for (key, harness, last_ms) in marks {
        match rows.iter_mut().find(|row| row.key == key) {
            Some(row) => {
                row.not_reported_by
                    .get_or_insert_with(|| harness_name(&harness));
                row.last_ms = row.last_ms.max(last_ms);
            }
            None if rows.len() < counted + ROWS_MAX => rows.push(UsageRow {
                key,
                tokens: Tokens::default(),
                runs: 0,
                last_ms,
                not_reported_by: Some(harness_name(&harness)),
            }),
            None => {}
        }
    }
    assert!(rows.len() <= counted + ROWS_MAX);
    assert!(
        rows[counted..]
            .iter()
            .all(|row| row.not_reported_by.is_some())
    );
    Ok(rows)
}

/// The names of the harnesses without a reader that ran in the range, sorted, at most
/// `HARNESSES_MAX`. Inputs: the store, the project or none and the range start.
fn unreported_harnesses(
    db: &Connection,
    project: Option<&str>,
    since_ms: i64,
) -> Result<Vec<String>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT DISTINCT harness FROM usage_unreported
             WHERE hour_ms>=?1 AND (?2 IS NULL OR project=?2) ORDER BY harness LIMIT ?3",
        )
        .map_err(storage)?;
    let ids: Vec<String> = statement
        .query_map(params![since_ms, project, HARNESSES_MAX as i64], |row| {
            row.get(0)
        })
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)?;
    let mut names: Vec<String> = ids.iter().map(|id| harness_name(id)).collect();
    names.sort();
    names.dedup();
    assert!(names.len() <= HARNESSES_MAX);
    Ok(names)
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
                    not_reported_by: None,
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
    let mut limits = all_limits(db)?;
    limits.truncate(LIMITS_MAX);
    let report = UsageReport {
        since_ms,
        total,
        runs,
        projects: flag_unreported(
            db,
            rows(db, "project", true, project, since)?,
            "project",
            project,
            since,
        )?,
        pools: rows(db, "pool", false, project, since)?,
        accounts: rows(db, "account", true, project, since)?,
        agents: flag_unreported(
            db,
            rows(db, "agent", false, project, since)?,
            "agent",
            project,
            since,
        )?,
        days,
        lines: lines(db, project, since)?,
        agent_lines: agent_lines(db, project, since)?,
        limits,
        not_reported_by: unreported_harnesses(db, project, since)?,
    };
    assert!(report.projects.len() <= 2 * ROWS_MAX && report.days.len() <= DAYS_MAX);
    assert!(report.agents.len() <= 2 * ROWS_MAX && report.agent_lines.len() <= ROWS_MAX);
    assert!(report.not_reported_by.len() <= HARNESSES_MAX);
    Ok(report)
}

#[cfg(all(test, unix))]
mod tests;
