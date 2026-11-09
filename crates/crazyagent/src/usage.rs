use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

pub mod capture;
pub mod health;
pub mod metrics;
pub mod poll;
pub mod report;
mod rules;
pub mod service;
pub mod turns;

pub use rules::THIRD_PARTY_NOTICES;

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
    failure TEXT,
    five_hour_polled_percent REAL CHECK(five_hour_polled_percent BETWEEN 0 AND 100),
    five_hour_polled_resets_ms INTEGER,
    weekly_polled_percent REAL CHECK(weekly_polled_percent BETWEEN 0 AND 100),
    weekly_polled_resets_ms INTEGER,
    five_hour_left_out TEXT CHECK(five_hour_left_out IN ('claude','codex')),
    weekly_left_out TEXT CHECK(weekly_left_out IN ('claude','codex'))
);";

const ADD_POLLED: &str = "
    ALTER TABLE account_limits ADD COLUMN five_hour_polled_percent REAL
        CHECK(five_hour_polled_percent BETWEEN 0 AND 100);
    ALTER TABLE account_limits ADD COLUMN five_hour_polled_resets_ms INTEGER;
    ALTER TABLE account_limits ADD COLUMN weekly_polled_percent REAL
        CHECK(weekly_polled_percent BETWEEN 0 AND 100);
    ALTER TABLE account_limits ADD COLUMN weekly_polled_resets_ms INTEGER;
    UPDATE account_limits SET five_hour_polled_percent=five_hour_percent,
        five_hour_polled_resets_ms=five_hour_resets_ms,weekly_polled_percent=weekly_percent,
        weekly_polled_resets_ms=weekly_resets_ms
    WHERE source='poll';";

const ADD_LEFT_OUT: &str = "
    ALTER TABLE account_limits ADD COLUMN five_hour_left_out TEXT
        CHECK(five_hour_left_out IN ('claude','codex'));
    ALTER TABLE account_limits ADD COLUMN weekly_left_out TEXT
        CHECK(weekly_left_out IN ('claude','codex'));";

const COLUMNS: &str = "account,plan,five_hour_percent,five_hour_resets_ms,weekly_percent,\
    weekly_resets_ms,five_hour_reset_override_ms,weekly_reset_override_ms,source,observed_ms,\
    checked_ms,failure";

const POLLED_COLUMNS: &str = "five_hour_polled_percent,five_hour_polled_resets_ms,weekly_polled_percent,weekly_polled_resets_ms";

const POLLED_BEFORE_COPIES: &str = "CASE WHEN source='poll' THEN five_hour_percent END,\
    CASE WHEN source='poll' THEN five_hour_resets_ms END,\
    CASE WHEN source='poll' THEN weekly_percent END,\
    CASE WHEN source='poll' THEN weekly_resets_ms END";

const LEFT_OUT_COLUMNS: &str = "five_hour_left_out,weekly_left_out";

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

/// What one read of an account's limits found, and whose usage call a poll made. A window
/// a poll left out is cleared and marked left out by that provider; one a run's own record
/// left out stays as it was stored.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Observation {
    pub plan: Option<String>,
    pub five_hour: Option<LimitWindow>,
    pub weekly: Option<LimitWindow>,
    pub provider: Option<poll::Provider>,
}

/// An account's stored limits: the last observed windows and plan, the same windows as
/// the last successful poll read them, the owner's manual reset times, where and when they
/// were last read, the plain code of the last failed poll, and the provider whose good poll
/// left each window out until a read reports it again, with the sentence the cards show
/// for that window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountLimits {
    pub account: CredentialRef,
    pub plan: Option<String>,
    pub five_hour: Option<LimitWindow>,
    pub weekly: Option<LimitWindow>,
    pub polled_five_hour: Option<LimitWindow>,
    pub polled_weekly: Option<LimitWindow>,
    pub five_hour_reset_override_ms: Option<i64>,
    pub weekly_reset_override_ms: Option<i64>,
    pub source: Option<LimitSource>,
    pub observed_ms: Option<i64>,
    pub checked_ms: Option<i64>,
    pub failure: Option<String>,
    pub five_hour_left_out: Option<poll::Provider>,
    pub weekly_left_out: Option<poll::Provider>,
    pub five_hour_left_out_label: Option<String>,
    pub weekly_left_out_label: Option<String>,
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

    /// The 5-hour window as the last successful poll read it, as it stands at `now_ms`.
    pub fn polled_five_hour_at(&self, now_ms: i64) -> Option<LimitWindow> {
        window_at(
            self.polled_five_hour,
            self.five_hour_reset_override_ms,
            now_ms,
        )
    }

    /// The weekly window as the last successful poll read it, as it stands at `now_ms`.
    pub fn polled_weekly_at(&self, now_ms: i64) -> Option<LimitWindow> {
        window_at(self.polled_weekly, self.weekly_reset_override_ms, now_ms)
    }

    /// Drops each window whose reset has passed when the last read of this account failed:
    /// nothing has read the window that came next, so 0% would be a guess and the report
    /// says not read instead. Scheduling keeps `window_at`. Input: the time now. Output:
    /// none; the windows are cleared on this row.
    fn forget_unread_windows(&mut self, now_ms: i64) {
        if self.failure.is_none() {
            return;
        }
        let passed = |window: Option<LimitWindow>, override_ms: Option<i64>| {
            window
                .and_then(|window| override_ms.or(window.resets_at_ms))
                .is_some_and(|at| at <= now_ms)
        };
        if passed(self.five_hour, self.five_hour_reset_override_ms) {
            self.five_hour = None;
        }
        if passed(self.weekly, self.weekly_reset_override_ms) {
            self.weekly = None;
        }
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

/// The sentence the cards show for a window a good poll left out: Codex has no such limit on
/// the account's plan, and Claude has no such window running now. Inputs: the provider and
/// the window. Output: the sentence.
pub fn left_out_label(provider: poll::Provider, kind: WindowKind) -> &'static str {
    match (provider, kind) {
        (poll::Provider::Codex, WindowKind::FiveHour) => {
            "Codex reports no 5-hour limit for this plan."
        }
        (poll::Provider::Codex, WindowKind::Weekly) => {
            "Codex reports no weekly limit for this plan."
        }
        (poll::Provider::Claude, WindowKind::FiveHour) => {
            "Claude reports no 5-hour window right now."
        }
        (poll::Provider::Claude, WindowKind::Weekly) => {
            "Claude reports no weekly window right now."
        }
    }
}

/// The key a removed project's usage rows move to, so totals by account and by day stay
/// whole and a project added again under the same id starts from nothing. It fails
/// `validate_identity`, so no project can have it as its id.
pub const REMOVED_PROJECTS: &str = "<removed>";

/// Moves a removed project's usage to `REMOVED_PROJECTS`: its token rows add into the rows
/// already kept there for the same hour and run, its unreported marks and seen sessions keep
/// the latest time, its turn rows add into the rows kept there for the same hour and agent,
/// and its runs and cuts keep counting there. A table this store lacks is skipped. Inputs:
/// the owner's transaction and the project. Output: Ok, or the store failure.
pub(crate) fn forget_project(db: &Connection, project: &str) -> Result<(), EngineError> {
    assert_ne!(project, REMOVED_PROJECTS);
    let has = |table: &str| -> Result<bool, EngineError> {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )
        .map_err(storage)
    };
    let moves = [
        (
            "usage_tokens",
            format!(
                "INSERT INTO usage_tokens(hour_ms,kind,project,pool,account,agent,harness,session,model,input,output,cache_read,cache_write,cost_nanos)
                 SELECT hour_ms,kind,?2,pool,account,agent,harness,session,model,input,output,cache_read,cache_write,cost_nanos
                 FROM usage_tokens WHERE project=?1
                 ON CONFLICT(hour_ms,kind,project,pool,account,agent,harness,session,model) DO UPDATE SET
                 input=MIN(input+excluded.input,{max}), output=MIN(output+excluded.output,{max}),
                 cache_read=MIN(cache_read+excluded.cache_read,{max}),
                 cache_write=MIN(cache_write+excluded.cache_write,{max}),
                 cost_nanos=MIN(cost_nanos+excluded.cost_nanos,{max})",
                max = capture::ROW_TOKENS_MAX
            ),
        ),
        (
            "usage_unreported",
            "INSERT INTO usage_unreported(hour_ms,kind,project,agent,harness,last_ms)
             SELECT hour_ms,kind,?2,agent,harness,last_ms FROM usage_unreported WHERE project=?1
             ON CONFLICT(hour_ms,kind,project,agent,harness) DO UPDATE SET last_ms=MAX(last_ms,excluded.last_ms)"
                .to_owned(),
        ),
        (
            "usage_seen",
            "INSERT INTO usage_seen(project,agent,session,last_ms)
             SELECT ?2,agent,session,last_ms FROM usage_seen WHERE project=?1
             ON CONFLICT(project,agent,session) DO UPDATE SET last_ms=MAX(last_ms,excluded.last_ms)"
                .to_owned(),
        ),
        (
            "usage_turns",
            format!(
                "INSERT INTO usage_turns(hour_ms,project,agent,turns)
                 SELECT hour_ms,?2,agent,turns FROM usage_turns WHERE project=?1
                 ON CONFLICT(hour_ms,project,agent) DO UPDATE SET
                 turns=MIN(turns+excluded.turns,{max})",
                max = turns::HOUR_TURNS_MAX
            ),
        ),
    ];
    for (table, add) in moves {
        if has(table)? {
            db.execute(&add, params![project, REMOVED_PROJECTS])
                .map_err(storage)?;
            db.execute(&format!("DELETE FROM {table} WHERE project=?1"), [project])
                .map_err(storage)?;
        }
    }
    for table in ["usage_runs", "usage_cut"] {
        if has(table)? {
            db.execute(
                &format!("UPDATE {table} SET project=?2 WHERE project=?1"),
                params![project, REMOVED_PROJECTS],
            )
            .map_err(storage)?;
        }
    }
    Ok(())
}

/// Adds the usage tables to a database when missing, and to a store made before them the
/// polled copies of the windows, filled from the rows a poll wrote last, and the marks of
/// windows a poll left out. Input: the connection. Output: done, or `usage_unavailable`.
pub fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(storage)?;
    add_columns(db, "weekly_polled_resets_ms", ADD_POLLED)?;
    add_columns(db, "weekly_left_out", ADD_LEFT_OUT)?;
    db.execute_batch(capture::SCHEMA).map_err(storage)?;
    db.execute_batch(health::SCHEMA).map_err(storage)?;
    db.execute_batch(report::SCHEMA).map_err(storage)?;
    db.execute_batch(turns::SCHEMA).map_err(storage)?;
    capture::count_rows(db)
}

fn add_columns(db: &Connection, last: &str, batch: &str) -> Result<(), EngineError> {
    if has_column(db, last)? {
        return Ok(());
    }
    db.execute_batch("SAVEPOINT usage_columns")
        .map_err(storage)?;
    let added = db.execute_batch(batch);
    let closed = match added {
        Ok(()) => db.execute_batch("RELEASE usage_columns"),
        Err(_) => db.execute_batch("ROLLBACK TO usage_columns; RELEASE usage_columns"),
    };
    closed.map_err(storage)?;
    if added.is_err() && !has_column(db, last)? {
        return Err(failure("usage_unavailable"));
    }
    Ok(())
}

fn has_column(db: &Connection, name: &str) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('account_limits') WHERE name=?1",
        params![name],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count == 1)
    .map_err(storage)
}

fn read_columns(db: &Connection) -> Result<String, EngineError> {
    let polled = if has_column(db, "weekly_polled_resets_ms")? {
        POLLED_COLUMNS
    } else {
        POLLED_BEFORE_COPIES
    };
    let left_out = if has_column(db, "weekly_left_out")? {
        LEFT_OUT_COLUMNS
    } else {
        "NULL,NULL"
    };
    Ok(format!("{COLUMNS},{polled},{left_out}"))
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

/// Stores what a read of an account's limits found, keeping the plan when it reported none.
/// A poll replaces each window, its polled copy and the owner's manual reset for it, clears
/// the last failure, and marks a window it left out as left out by its provider; a poll
/// must name its provider. A window any read reports clears that mark. A transcript reading
/// is advisory: it keeps any window it did not report, every manual reset, polled copy and
/// mark, and is ignored while a poll succeeded within `POLL_EVERY_MS`.
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
    let polled = matches!(source, LimitSource::Poll);
    assert!(
        !polled || observation.provider.is_some(),
        "a poll names whose usage call it made"
    );
    let checked = polled.then_some(now_ms);
    db.execute(
        "INSERT INTO account_limits (account,plan,five_hour_percent,five_hour_resets_ms,\
            weekly_percent,weekly_resets_ms,source,observed_ms,checked_ms,failure,\
            five_hour_polled_percent,five_hour_polled_resets_ms,weekly_polled_percent,\
            weekly_polled_resets_ms,five_hour_left_out,weekly_left_out)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,NULL,CASE WHEN ?12 THEN ?3 END,\
            CASE WHEN ?12 THEN ?4 END,CASE WHEN ?12 THEN ?5 END,CASE WHEN ?12 THEN ?6 END,\
            CASE WHEN ?12 AND NOT ?10 THEN ?14 END,CASE WHEN ?12 AND NOT ?11 THEN ?14 END)
         ON CONFLICT(account) DO UPDATE SET
            plan=COALESCE(excluded.plan,plan),
            five_hour_percent=CASE WHEN ?10 OR ?12 THEN excluded.five_hour_percent ELSE five_hour_percent END,
            five_hour_resets_ms=CASE WHEN ?10 OR ?12 THEN excluded.five_hour_resets_ms ELSE five_hour_resets_ms END,
            five_hour_reset_override_ms=CASE WHEN ?12 THEN NULL ELSE five_hour_reset_override_ms END,
            weekly_percent=CASE WHEN ?11 OR ?12 THEN excluded.weekly_percent ELSE weekly_percent END,
            weekly_resets_ms=CASE WHEN ?11 OR ?12 THEN excluded.weekly_resets_ms ELSE weekly_resets_ms END,
            weekly_reset_override_ms=CASE WHEN ?12 THEN NULL ELSE weekly_reset_override_ms END,
            five_hour_polled_percent=CASE WHEN ?12 THEN excluded.five_hour_percent ELSE five_hour_polled_percent END,
            five_hour_polled_resets_ms=CASE WHEN ?12 THEN excluded.five_hour_resets_ms ELSE five_hour_polled_resets_ms END,
            weekly_polled_percent=CASE WHEN ?12 THEN excluded.weekly_percent ELSE weekly_polled_percent END,
            weekly_polled_resets_ms=CASE WHEN ?12 THEN excluded.weekly_resets_ms ELSE weekly_polled_resets_ms END,
            five_hour_left_out=CASE WHEN ?10 THEN NULL WHEN ?12 THEN ?14 ELSE five_hour_left_out END,
            weekly_left_out=CASE WHEN ?11 THEN NULL WHEN ?12 THEN ?14 ELSE weekly_left_out END,
            source=excluded.source,
            observed_ms=excluded.observed_ms,
            checked_ms=COALESCE(excluded.checked_ms,checked_ms),
            failure=CASE WHEN excluded.checked_ms IS NULL THEN failure ELSE NULL END
         WHERE ?12 OR source IS NOT 'poll' OR observed_ms<=?8-?13",
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
            polled,
            service::POLL_EVERY_MS,
            observation.provider.map(provider_name),
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

/// Forgets one removed account's stored limits, so no limits card stays for it.
/// Inputs: the engine store or a transaction on it, and the account. Output: done, nothing
/// to do in a store that never kept limits, or `usage_unavailable`.
pub(crate) fn forget_limits(db: &Connection, account: &CredentialRef) -> Result<(), EngineError> {
    let kept: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='account_limits')",
            [],
            |row| row.get(0),
        )
        .map_err(storage)?;
    if kept {
        db.execute(
            "DELETE FROM account_limits WHERE account=?1",
            params![account.as_str()],
        )
        .map_err(storage)?;
    }
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

fn provider_name(provider: poll::Provider) -> &'static str {
    match provider {
        poll::Provider::Claude => "claude",
        poll::Provider::Codex => "codex",
    }
}

fn left_out(row: &Row<'_>, at: usize) -> rusqlite::Result<Option<poll::Provider>> {
    match row.get::<_, Option<String>>(at)?.as_deref() {
        None => Ok(None),
        Some("claude") => Ok(Some(poll::Provider::Claude)),
        Some("codex") => Ok(Some(poll::Provider::Codex)),
        Some(_) => Err(conversion(at)),
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
    let five_hour_left_out = left_out(row, 16)?;
    let weekly_left_out = left_out(row, 17)?;
    Ok(AccountLimits {
        account: CredentialRef::new(row.get::<_, String>(0)?).map_err(|_| conversion(0))?,
        plan: row.get(1)?,
        five_hour: window(row, 2)?,
        weekly: window(row, 4)?,
        polled_five_hour: window(row, 12)?,
        polled_weekly: window(row, 14)?,
        five_hour_reset_override_ms: row.get(6)?,
        weekly_reset_override_ms: row.get(7)?,
        source,
        observed_ms: row.get(9)?,
        checked_ms: row.get(10)?,
        failure: row.get(11)?,
        five_hour_left_out,
        weekly_left_out,
        five_hour_left_out_label: five_hour_left_out
            .map(|provider| left_out_label(provider, WindowKind::FiveHour).to_owned()),
        weekly_left_out_label: weekly_left_out
            .map(|provider| left_out_label(provider, WindowKind::Weekly).to_owned()),
    })
}

/// Reads one account's stored limits. A store made before the polled copies takes them
/// from a row a poll wrote last. Inputs: the store and the account.
/// Output: the limits, None when nothing was ever stored for it, or `usage_unavailable`.
pub fn limits(
    db: &Connection,
    account: &CredentialRef,
) -> Result<Option<AccountLimits>, EngineError> {
    let columns = read_columns(db)?;
    db.query_row(
        &format!("SELECT {columns} FROM account_limits WHERE account=?1"),
        params![account.as_str()],
        limits_row,
    )
    .optional()
    .map_err(storage)
}

/// Reads every account's stored limits, ordered by account, as `limits` does.
/// Input: the store. Output: the limits, or `usage_unavailable`.
pub fn all_limits(db: &Connection) -> Result<Vec<AccountLimits>, EngineError> {
    let columns = read_columns(db)?;
    let mut statement = db
        .prepare(&format!(
            "SELECT {columns} FROM account_limits ORDER BY account"
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
