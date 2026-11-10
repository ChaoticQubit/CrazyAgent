use super::capture::{Attribution, Format, Tokens};
use super::health::{StepHealth, health};
use super::money::{MoneyView, PriceSource};
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
pub const HARNESSES_MAX: usize = 32;
const HOUR_MS: i64 = 60 * 60 * 1000;
const DAY_MS: i64 = 24 * HOUR_MS;
const OFFSET_MINUTES_MAX: u32 = 14 * 60;
pub(super) const SHARES: usize = 8;
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
    #[serde(default)]
    pub cost_nanos: i64,
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
/// limits, the harnesses that ran in the range without reporting usage, each usage step
/// kind that panicked since the engine started, and how many agent homes a scan in the
/// range left records of uncounted.
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
    #[serde(default)]
    pub health: Vec<StepHealth>,
    pub partial_homes: u64,
    #[serde(default)]
    pub money: MoneyView,
}

/// Tells whether the engine counts a harness's usage: from its session records (Claude
/// Code, Codex) or from the usage frames its host reports (RCLI, which runs only as a
/// CrazyAgent master). Input: the harness id. Output: the answer.
pub fn reports_usage(harness: &str) -> bool {
    Format::of(harness).is_some()
        || crate::harnesses::spec(harness).is_some_and(|spec| spec.id == "rcli")
}

/// Marks that a run of a harness the engine has no usage reader for worked in the hour of
/// `now_ms`, so the report says its usage is not reported instead of showing zero. Inputs:
/// the store, the attribution and the time. Output: done, `usage_attribution_invalid` for a
/// harness whose usage is counted or a missing or oversized agent or harness, or
/// `usage_unavailable`.
pub fn mark_unreported(
    db: &Connection,
    attribution: &Attribution,
    now_ms: i64,
) -> Result<(), EngineError> {
    if attribution.agent.is_empty()
        || attribution.agent.len() > AGENT_BYTES_MAX
        || attribution.harness.is_empty()
        || attribution.harness.len() > HARNESS_BYTES_MAX
        || reports_usage(&attribution.harness)
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
/// first, and adds a row for each such key the top rows left out, after them, with the key's
/// full count, so a key outside the top rows never shows as zero when it counted tokens. The
/// left-out keys are counted together in one read of the range. Inputs: the store, the rows,
/// the grouping column, whether the unattributed `''` key is listed, as for the rows, the
/// project or none and the range start. Output: the rows, at most `2 * ROWS_MAX`.
fn flag_unreported(
    db: &Connection,
    mut rows: Vec<UsageRow>,
    column: &str,
    unattributed: bool,
    project: Option<&str>,
    since_ms: i64,
) -> Result<Vec<UsageRow>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT {column},harness,MAX(last_ms) FROM usage_unreported
             WHERE hour_ms>=?1 AND (?2 IS NULL OR project=?2) AND (?4 OR {column}<>'')
             GROUP BY {column},harness ORDER BY MAX(last_ms) DESC,{column},harness LIMIT ?3"
        ))
        .map_err(storage)?;
    let marks: Vec<(String, String, i64)> = statement
        .query_map(
            params![since_ms, project, (2 * ROWS_MAX) as i64, unattributed],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(storage)?
        .collect::<Result<_, _>>()
        .map_err(storage)?;
    let mut left_out: Vec<&str> = Vec::new();
    for (key, _, _) in &marks {
        let listed = rows.iter().any(|row| row.key == *key) || left_out.contains(&key.as_str());
        if !listed && left_out.len() < ROWS_MAX {
            left_out.push(key);
        }
    }
    let mut found = self::rows(db, column, unattributed, project, since_ms, Some(&left_out))?;
    assert!(found.len() <= left_out.len());
    let counted = rows.len();
    for (key, harness, last_ms) in &marks {
        match rows.iter_mut().find(|row| row.key == *key) {
            Some(row) => {
                row.not_reported_by
                    .get_or_insert_with(|| harness_name(harness));
                row.last_ms = row.last_ms.max(*last_ms);
            }
            None if left_out.contains(&key.as_str()) => {
                let mut row = match found.iter().position(|row| row.key == *key) {
                    Some(at) => found.swap_remove(at),
                    None => UsageRow {
                        key: key.clone(),
                        tokens: Tokens::default(),
                        runs: 0,
                        last_ms: *last_ms,
                        not_reported_by: None,
                        cost_nanos: 0,
                    },
                };
                row.not_reported_by = Some(harness_name(harness));
                row.last_ms = row.last_ms.max(*last_ms);
                rows.push(row);
            }
            None => {}
        }
    }
    assert!(found.is_empty());
    assert!(rows.len() == counted + left_out.len() && rows.len() <= counted + ROWS_MAX);
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

pub(super) fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Reads four `TOTAL()` token sums, which never overflow, saturating each into a count.
/// Inputs: the row and its first sum's column. Output: the tokens.
pub(super) fn tokens(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<Tokens> {
    let amount = |column: usize| row.get::<_, f64>(column).map(|total| total as u64);
    Ok(Tokens {
        input: amount(first)?,
        output: amount(first + 1)?,
        cache_read: amount(first + 2)?,
        cache_write: amount(first + 3)?,
    })
}

/// The counted rows of one grouping column in the range, the largest headline first
/// (`Tokens::headline`, cache re-reads not counted): every key, or
/// only the given keys, at most `ROWS_MAX` of them, all read in one pass over the range.
/// Inputs: the store, the column, whether the unattributed `''` key is listed, the project
/// or none, the range start and the keys or none for all. Output: at most `ROWS_MAX` rows,
/// one a key; none for an empty key list.
fn rows(
    db: &Connection,
    column: &str,
    unattributed: bool,
    project: Option<&str>,
    since_ms: i64,
    keys: Option<&[&str]>,
) -> Result<Vec<UsageRow>, EngineError> {
    let only = match keys {
        Some([]) => return Ok(Vec::new()),
        Some(keys) => {
            assert!(keys.len() <= ROWS_MAX);
            let marks: Vec<String> = (0..keys.len()).map(|at| format!("?{}", at + 5)).collect();
            format!("AND {column} IN ({})", marks.join(","))
        }
        None => String::new(),
    };
    let mut statement = db
        .prepare(&format!(
            "SELECT {column},TOTAL(input),TOTAL(output),TOTAL(cache_read),TOTAL(cache_write),\
                COUNT(DISTINCT agent||'/'||session),MAX(hour_ms),TOTAL(cost_nanos)
             FROM usage_tokens WHERE hour_ms>=?1 AND (?3 OR {column}<>'')
                AND (?4 IS NULL OR project=?4) {only}
             GROUP BY {column}
             ORDER BY TOTAL(input)+TOTAL(output)+TOTAL(cache_write) DESC, {column}
             LIMIT ?2"
        ))
        .map_err(storage)?;
    let limit = ROWS_MAX as i64;
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&since_ms, &limit, &unattributed, &project];
    bound.extend(
        keys.unwrap_or_default()
            .iter()
            .map(|key| key as &dyn rusqlite::ToSql),
    );
    statement
        .query_map(bound.as_slice(), |row| {
            Ok(UsageRow {
                key: row.get(0)?,
                tokens: tokens(row, 1)?,
                runs: count(row.get(5)?),
                last_ms: row.get(6)?,
                not_reported_by: None,
                cost_nanos: row.get::<_, f64>(7)? as i64,
            })
        })
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

pub(super) fn has_table(db: &Connection, name: &str) -> Result<bool, EngineError> {
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
/// rows, oldest days and last accounts' limits first. Projects and agents, whose flagged
/// rows follow their top rows up to `2 * ROWS_MAX`, take their cap from their own length
/// when it passes `ROWS_MAX`. Input: the report.
/// Output: the report that fits, or `usage_unavailable`.
fn fit(mut report: UsageReport) -> Result<UsageReport, EngineError> {
    let projects = report.projects.len().max(ROWS_MAX);
    let agents = report.agents.len().max(ROWS_MAX);
    assert!(projects <= 2 * ROWS_MAX && agents <= 2 * ROWS_MAX);
    for share in (0..=SHARES).rev() {
        let kept = |cap: usize| cap * share / SHARES;
        report.projects.truncate(kept(projects));
        report.agents.truncate(kept(agents));
        report.pools.truncate(kept(ROWS_MAX));
        report.accounts.truncate(kept(ROWS_MAX));
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
    for row in &mut limits {
        row.forget_unread_windows(now_ms);
    }
    let money = super::money::view(db, project, since)?;
    let mut report = fit(UsageReport {
        since_ms,
        total,
        runs,
        projects: flag_unreported(
            db,
            rows(db, "project", true, project, since, None)?,
            "project",
            true,
            project,
            since,
        )?,
        pools: rows(db, "pool", false, project, since, None)?,
        accounts: rows(db, "account", true, project, since, None)?,
        agents: flag_unreported(
            db,
            rows(db, "agent", false, project, since, None)?,
            "agent",
            false,
            project,
            since,
        )?,
        days,
        lines: lines(db, project, since)?,
        agent_lines: agent_lines(db, project, since)?,
        limits,
        not_reported_by: unreported_harnesses(db, project, since)?,
        health: health(db)?,
        partial_homes,
        money,
    })?;
    if report.money.settings.prices == PriceSource::Off {
        report
            .projects
            .iter_mut()
            .chain(report.pools.iter_mut())
            .chain(report.accounts.iter_mut())
            .chain(report.agents.iter_mut())
            .for_each(|row| row.cost_nanos = 0);
    }
    assert!(report.projects.len() <= 2 * ROWS_MAX && report.days.len() <= DAYS_MAX);
    assert!(report.agents.len() <= 2 * ROWS_MAX && report.agent_lines.len() <= ROWS_MAX);
    assert!(report.not_reported_by.len() <= HARNESSES_MAX);
    Ok(report)
}

#[cfg(all(test, unix))]
pub(crate) mod tests;
