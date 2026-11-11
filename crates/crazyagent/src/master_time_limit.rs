use crate::engine::EngineError;
use crate::engine::owner::OwnerData;
use crate::protocol::Outcome;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How the cause of a master turn its time limit ended starts; the minutes follow, as in
/// `master_time_limit_10`, a plain code an owner window may show.
const CAUSE_PREFIX: &str = "master_time_limit_";

/// The longest wall clock a master host takes: the longest limit the owner can pick.
pub const RUN_TIME_MAX: Duration = Duration::from_secs(60 * 60);

/// The longest a master host with no wall clock waits for its harness to start. It bounds
/// the start only, never the turn: a turn the owner cannot stop yet must not wait for ever.
pub const START_TIME_MAX: Duration = Duration::from_secs(10 * 60);

/// How far ahead a turn with no time limit has its sign-in copy checked when it starts:
/// the day a team agent's copy is checked for. A copy that lapses sooner is renewed while
/// the turn runs, however long that is.
pub const SIGN_IN_AHEAD: Duration = Duration::from_secs(24 * 60 * 60);

#[cfg(any(unix, test))]
const TABLE: &str = "CREATE TABLE IF NOT EXISTS master_time_settings (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    time_limit TEXT NOT NULL CHECK(time_limit IN ('no_limit', 'minutes10', 'minutes30', 'minutes60'))
)";

/// How long a master may work on one message before it stops and waits for the owner: no
/// limit, or 10, 30 or 60 minutes. It is the wall clock of the master's host; with no
/// limit the host has none, and the owner's Stop ends a turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeLimit {
    #[default]
    NoLimit,
    Minutes10,
    Minutes30,
    Minutes60,
}

impl TimeLimit {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 4] = [
        Self::NoLimit,
        Self::Minutes10,
        Self::Minutes30,
        Self::Minutes60,
    ];

    /// The minutes the choice allows. Input: the choice. Output: the minutes, or none for
    /// no limit.
    pub fn minutes(self) -> Option<u16> {
        match self {
            Self::NoLimit => None,
            Self::Minutes10 => Some(10),
            Self::Minutes30 => Some(30),
            Self::Minutes60 => Some(60),
        }
    }

    /// The wall clock a master host runs under. Input: the choice. Output: the time, or
    /// none for no limit, which gives the host no clock.
    pub fn run_time(self) -> Option<Duration> {
        let time = self
            .minutes()
            .map(|minutes| Duration::from_secs(u64::from(minutes) * 60));
        assert!(time.is_none_or(|time| !time.is_zero() && time <= RUN_TIME_MAX));
        assert_eq!(time.is_none(), self == Self::NoLimit);
        time
    }

    /// How far ahead a turn's sign-in copy is checked when it starts: the limit, or
    /// [`SIGN_IN_AHEAD`] for no limit. Input: the choice. Output: the time.
    pub fn sign_in_ahead(self) -> Duration {
        let ahead = self.run_time().unwrap_or(SIGN_IN_AHEAD);
        assert!(!ahead.is_zero());
        assert!(ahead <= SIGN_IN_AHEAD);
        ahead
    }

    /// The cause a turn stopped by this limit ends with. Input: the choice. Output: the
    /// cause, or none for no limit, which stops no turn.
    pub fn cause(self) -> Option<String> {
        self.minutes()
            .map(|minutes| format!("{CAUSE_PREFIX}{minutes}"))
    }
}

/// The minutes a stopped turn's cause names. Input: the cause. Output: the minutes, or
/// none for another cause or minutes no limit allows.
pub fn minutes_of_cause(cause: &str) -> Option<u16> {
    let text = cause.strip_prefix(CAUSE_PREFIX)?;
    let minutes = TimeLimit::ALL
        .into_iter()
        .filter_map(TimeLimit::minutes)
        .find(|minutes| minutes.to_string() == text);
    assert!(minutes.is_none_or(|minutes| (10..=60).contains(&minutes)));
    minutes
}

/// The owner's app-wide choice of the master time limit ("Master time limit", OB-5c).
/// Before the first save it is its default, no limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterTimeSettings {
    pub limit: TimeLimit,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn invalid() -> EngineError {
    error("master_time_settings_invalid")
}

/// Creates the settings table when it is missing. Input: the engine database. Output: Ok,
/// or the database error.
#[cfg(any(unix, test))]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(error)
}

/// Saves the owner's master time limit over the stored one. Inputs: the open transaction
/// and the settings. Output: `Applied`, or the database error.
pub(crate) fn save(
    connection: &Connection,
    settings: &MasterTimeSettings,
) -> Result<Outcome, EngineError> {
    let limit = match serde_json::to_value(settings.limit).map_err(error)? {
        serde_json::Value::String(name) => name,
        _ => return Err(invalid()),
    };
    let stored = connection
        .execute(
            "INSERT INTO master_time_settings(singleton,time_limit) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET time_limit=excluded.time_limit",
            rusqlite::params![limit],
        )
        .map_err(error)?;
    assert_eq!(stored, 1);
    Ok(Outcome::Applied)
}

/// Loads the owner's master time limit. Input: the engine database, writable or read-only.
/// Output: the stored setting, the default before the first save, or
/// `master_time_settings_invalid` for a stored value this engine does not know, so the
/// engine acts on no guess.
pub fn load(connection: &Connection) -> Result<MasterTimeSettings, EngineError> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT time_limit FROM master_time_settings WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(limit) = stored else {
        return Ok(MasterTimeSettings::default());
    };
    let limit = serde_json::from_value(serde_json::Value::String(limit)).map_err(|_| invalid())?;
    Ok(MasterTimeSettings { limit })
}

/// The owner read of the master time limit. Input: the open transaction. Output: the
/// setting as owner data.
pub(crate) fn read(connection: &Connection) -> Result<OwnerData, EngineError> {
    Ok(OwnerData::MasterTimeSettings {
        settings: load(connection)?,
    })
}

#[cfg(test)]
mod tests;
