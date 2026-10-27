use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

pub const PLAN_BYTES_MAX: usize = 64;
pub const FAILURE_BYTES_MAX: usize = 64;
pub const RESET_MS_MAX: i64 = 32_503_680_000_000;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS account_limits (
    account TEXT PRIMARY KEY,
    plan TEXT,
    five_hour_percent REAL CHECK(five_hour_percent BETWEEN 0 AND 100),
    five_hour_resets_ms INTEGER,
    weekly_percent REAL CHECK(weekly_percent BETWEEN 0 AND 100),
    weekly_resets_ms INTEGER,
    five_hour_reset_override_ms INTEGER,
    weekly_reset_override_ms INTEGER,
    source TEXT CHECK(source IN ('poll','transcript')),
    observed_ms INTEGER,
    checked_ms INTEGER,
    failure TEXT
);";

const COLUMNS: &str = "account,plan,five_hour_percent,five_hour_resets_ms,weekly_percent,\
    weekly_resets_ms,five_hour_reset_override_ms,weekly_reset_override_ms,source,observed_ms,\
    checked_ms,failure";

/// One provider limit window as last observed: the share used, 0 to 100, and when the
/// window resets in milliseconds since the epoch, when the provider said.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitWindow {
    pub used_percent: f64,
    pub resets_at_ms: Option<i64>,
}

/// Which of an account's two limit windows a value is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    FiveHour,
    Weekly,
}

/// Where an account's limit windows were last read from: the provider's usage call the
/// engine polls, or the limit data a harness wrote into a run's own record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitSource {
    Poll,
    Transcript,
}

/// What one read of an account's limits found. A window the read did not report stays
/// as it was stored.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Observation {
    pub plan: Option<String>,
    pub five_hour: Option<LimitWindow>,
    pub weekly: Option<LimitWindow>,
}

/// An account's stored limits: the last observed windows and plan, the owner's manual
/// reset times, where and when they were read, and the plain code of the last failed poll.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountLimits {
    pub account: CredentialRef,
    pub plan: Option<String>,
    pub five_hour: Option<LimitWindow>,
    pub weekly: Option<LimitWindow>,
    pub five_hour_reset_override_ms: Option<i64>,
    pub weekly_reset_override_ms: Option<i64>,
    pub source: Option<LimitSource>,
    pub observed_ms: Option<i64>,
    pub checked_ms: Option<i64>,
    pub failure: Option<String>,
}

impl AccountLimits {
    /// The 5-hour window as it stands at `now_ms`; see `window_at`.
    pub fn five_hour_at(&self, now_ms: i64) -> Option<LimitWindow> {
        window_at(self.five_hour, self.five_hour_reset_override_ms, now_ms)
    }

    /// The weekly window as it stands at `now_ms`; see `window_at`.
    pub fn weekly_at(&self, now_ms: i64) -> Option<LimitWindow> {
        window_at(self.weekly, self.weekly_reset_override_ms, now_ms)
    }
}

/// Applies the owner's manual reset to an observed window and ages it to `now_ms`: a manual
/// reset replaces the observed reset time, and a window whose reset time has passed counts
/// as unused with its next reset unknown. Output: the window, or None when none was observed.
pub fn window_at(
    observed: Option<LimitWindow>,
    override_ms: Option<i64>,
    now_ms: i64,
) -> Option<LimitWindow> {
    let observed = observed?;
    assert!((0.0..=100.0).contains(&observed.used_percent));
    let resets_at_ms = override_ms.or(observed.resets_at_ms);
    if resets_at_ms.is_some_and(|at| at <= now_ms) {
        return Some(LimitWindow {
            used_percent: 0.0,
            resets_at_ms: None,
        });
    }
    Some(LimitWindow {
        used_percent: observed.used_percent,
        resets_at_ms,
    })
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

fn storage(_: impl std::fmt::Display) -> EngineError {
    failure("usage_unavailable")
}

/// Opens the usage store in the engine database, adding its tables when missing.
/// Input: the engine root holding `engine.db`. Output: the connection.
pub fn open(runtime: &Path) -> Result<Connection, EngineError> {
    if !runtime.is_absolute() {
        return Err(failure("usage_unavailable"));
    }
    let db = Connection::open(runtime.join("engine.db")).map_err(storage)?;
    db.busy_timeout(Duration::from_secs(5)).map_err(storage)?;
    initialize(&db)?;
    Ok(db)
}

/// Adds the usage tables to a database when missing. Input: the connection.
/// Output: done, or `usage_unavailable`.
pub fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(storage)
}

fn checked_window(window: Option<LimitWindow>) -> Result<Option<LimitWindow>, EngineError> {
    let Some(window) = window else {
        return Ok(None);
    };
    if !window.used_percent.is_finite() || !(0.0..=100.0).contains(&window.used_percent) {
        return Err(failure("usage_limits_invalid"));
    }
    if window
        .resets_at_ms
        .is_some_and(|at| !(1..=RESET_MS_MAX).contains(&at))
    {
        return Err(failure("usage_limits_invalid"));
    }
    Ok(Some(window))
}

fn checked_text(text: Option<&str>, max: usize) -> Result<Option<&str>, EngineError> {
    match text {
        Some(text)
            if text.is_empty()
                || text.len() > max
                || !text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_. ".contains(&b)) =>
        {
            Err(failure("usage_limits_invalid"))
        }
        other => Ok(other),
    }
}

/// Stores what a read of an account's limits found, keeping any window it did not report
/// and the plan when it reported none. A reported window replaces the owner's manual reset
/// for it; a poll clears the last failure.
/// Inputs: the store, the account, the observation, its source and the time.
/// Output: done, `usage_limits_invalid` for a share outside 0..=100 or a bad time, or
/// `usage_unavailable`.
pub fn record_observation(
    db: &Connection,
    account: &CredentialRef,
    observation: &Observation,
    source: LimitSource,
    now_ms: i64,
) -> Result<(), EngineError> {
    let five_hour = checked_window(observation.five_hour)?;
    let weekly = checked_window(observation.weekly)?;
    let plan = checked_text(observation.plan.as_deref(), PLAN_BYTES_MAX)?;
    let checked = matches!(source, LimitSource::Poll).then_some(now_ms);
    db.execute(
        "INSERT INTO account_limits (account,plan,five_hour_percent,five_hour_resets_ms,\
            weekly_percent,weekly_resets_ms,source,observed_ms,checked_ms,failure)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,NULL)
         ON CONFLICT(account) DO UPDATE SET
            plan=COALESCE(excluded.plan,plan),
            five_hour_percent=CASE WHEN ?10 THEN excluded.five_hour_percent ELSE five_hour_percent END,
            five_hour_resets_ms=CASE WHEN ?10 THEN excluded.five_hour_resets_ms ELSE five_hour_resets_ms END,
            five_hour_reset_override_ms=CASE WHEN ?10 THEN NULL ELSE five_hour_reset_override_ms END,
            weekly_percent=CASE WHEN ?11 THEN excluded.weekly_percent ELSE weekly_percent END,
            weekly_resets_ms=CASE WHEN ?11 THEN excluded.weekly_resets_ms ELSE weekly_resets_ms END,
            weekly_reset_override_ms=CASE WHEN ?11 THEN NULL ELSE weekly_reset_override_ms END,
            source=excluded.source,
            observed_ms=excluded.observed_ms,
            checked_ms=COALESCE(excluded.checked_ms,checked_ms),
            failure=CASE WHEN excluded.checked_ms IS NULL THEN failure ELSE NULL END",
        params![
            account.as_str(),
            plan,
            five_hour.map(|w| w.used_percent),
            five_hour.and_then(|w| w.resets_at_ms),
            weekly.map(|w| w.used_percent),
            weekly.and_then(|w| w.resets_at_ms),
            source_name(source),
            now_ms,
            checked,
            five_hour.is_some(),
            weekly.is_some(),
        ],
    )
    .map_err(storage)?;
    Ok(())
}

/// Stores that a poll of an account's limits failed, keeping what was read before.
/// Inputs: the store, the account, the plain failure code and the time.
/// Output: done, `usage_limits_invalid` for a bad code, or `usage_unavailable`.
pub fn record_failure(
    db: &Connection,
    account: &CredentialRef,
    code: &str,
    now_ms: i64,
) -> Result<(), EngineError> {
    let code = checked_text(Some(code), FAILURE_BYTES_MAX)?;
    db.execute(
        "INSERT INTO account_limits (account,checked_ms,failure) VALUES (?1,?2,?3)
         ON CONFLICT(account) DO UPDATE SET checked_ms=excluded.checked_ms,failure=excluded.failure",
        params![account.as_str(), now_ms, code],
    )
    .map_err(storage)?;
    Ok(())
}

/// Sets or clears the owner's manual reset time for one window of an account.
/// Inputs: the store, the account, the window and the reset time, None to clear it.
/// Output: done, `usage_limits_invalid` for a time outside the supported range, or
/// `usage_unavailable`.
pub fn set_reset_override(
    db: &Connection,
    account: &CredentialRef,
    window: WindowKind,
    at_ms: Option<i64>,
) -> Result<(), EngineError> {
    if at_ms.is_some_and(|at| !(1..=RESET_MS_MAX).contains(&at)) {
        return Err(failure("usage_limits_invalid"));
    }
    let column = match window {
        WindowKind::FiveHour => "five_hour_reset_override_ms",
        WindowKind::Weekly => "weekly_reset_override_ms",
    };
    db.execute(
        &format!(
            "INSERT INTO account_limits (account,{column}) VALUES (?1,?2)
             ON CONFLICT(account) DO UPDATE SET {column}=excluded.{column}"
        ),
        params![account.as_str(), at_ms],
    )
    .map_err(storage)?;
    Ok(())
}

fn source_name(source: LimitSource) -> &'static str {
    match source {
        LimitSource::Poll => "poll",
        LimitSource::Transcript => "transcript",
    }
}

fn window(row: &Row<'_>, percent: usize) -> rusqlite::Result<Option<LimitWindow>> {
    let used: Option<f64> = row.get(percent)?;
    let resets: Option<i64> = row.get(percent + 1)?;
    Ok(used.map(|used_percent| LimitWindow {
        used_percent,
        resets_at_ms: resets,
    }))
}

fn conversion(row: usize) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(row, rusqlite::types::Type::Text, "usage".into())
}

fn limits_row(row: &Row<'_>) -> rusqlite::Result<AccountLimits> {
    let source = match row.get::<_, Option<String>>(8)?.as_deref() {
        None => None,
        Some("poll") => Some(LimitSource::Poll),
        Some("transcript") => Some(LimitSource::Transcript),
        Some(_) => return Err(conversion(8)),
    };
    Ok(AccountLimits {
        account: CredentialRef::new(row.get::<_, String>(0)?).map_err(|_| conversion(0))?,
        plan: row.get(1)?,
        five_hour: window(row, 2)?,
        weekly: window(row, 4)?,
        five_hour_reset_override_ms: row.get(6)?,
        weekly_reset_override_ms: row.get(7)?,
        source,
        observed_ms: row.get(9)?,
        checked_ms: row.get(10)?,
        failure: row.get(11)?,
    })
}

/// Reads one account's stored limits. Inputs: the store and the account.
/// Output: the limits, None when nothing was ever stored for it, or `usage_unavailable`.
pub fn limits(
    db: &Connection,
    account: &CredentialRef,
) -> Result<Option<AccountLimits>, EngineError> {
    db.query_row(
        &format!("SELECT {COLUMNS} FROM account_limits WHERE account=?1"),
        params![account.as_str()],
        limits_row,
    )
    .optional()
    .map_err(storage)
}

/// Reads every account's stored limits, ordered by account. Input: the store.
/// Output: the limits, or `usage_unavailable`.
pub fn all_limits(db: &Connection) -> Result<Vec<AccountLimits>, EngineError> {
    let mut statement = db
        .prepare(&format!(
            "SELECT {COLUMNS} FROM account_limits ORDER BY account"
        ))
        .map_err(storage)?;
    statement
        .query_map([], limits_row)
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)
}

#[cfg(test)]
mod tests;
