use crate::engine::EngineError;
use crate::engine::owner::OwnerData;
use crate::protocol::Outcome;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

pub const HOURS_MIN: u16 = 1;
pub const HOURS_MAX: u16 = 168;
pub const HOURS_DEFAULT: u16 = 24;
const HOUR: Duration = Duration::from_secs(60 * 60);
/// How far ahead a server's deadline is set while the owner keeps agents left running with
/// no time limit: past any engine's return, and within every platform's file times.
pub const NEVER_AHEAD: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

#[cfg(any(unix, test))]
const TABLE: &str = "CREATE TABLE IF NOT EXISTS agent_quit_settings (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    on_quit TEXT NOT NULL CHECK(on_quit IN ('keep_running', 'stop')),
    left_running TEXT NOT NULL CHECK(left_running IN ('after_hours', 'never')),
    hours INTEGER NOT NULL CHECK(hours BETWEEN 1 AND 168)
)";

/// What happens to running team agents when CrazyAgent quits: they keep running in their
/// terminals for the next engine to take over, or they stop as an owner Stop would.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnQuit {
    #[default]
    KeepRunning,
    Stop,
}

/// How long agents left running wait for CrazyAgent to come back before their terminals end:
/// a number of hours, or no limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftRunning {
    #[default]
    AfterHours,
    Never,
}

/// The owner's app-wide choice for agents when CrazyAgent quits ("Agents when CrazyAgent
/// quits", FBTMUX-P1): what quitting does to running team agents, and how long the ones left
/// running wait for the engine, with the hours kept while Never is picked. Before the first
/// save every value is its default: keep them running, end them after 24 hours away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuitSettings {
    pub on_quit: OnQuit,
    pub left_running: LeftRunning,
    pub hours: u16,
}

impl Default for QuitSettings {
    fn default() -> Self {
        Self {
            on_quit: OnQuit::KeepRunning,
            left_running: LeftRunning::AfterHours,
            hours: HOURS_DEFAULT,
        }
    }
}

impl QuitSettings {
    /// Whether team agents keep running when the engine quits. Input: the settings. Output:
    /// the answer.
    pub fn keeps_team_agents(&self) -> bool {
        self.on_quit == OnQuit::KeepRunning
    }

    /// The time a terminal server left by an engine that does not come back ends at, counted
    /// from `now`: the chosen hours later, or [`NEVER_AHEAD`] for no limit. Inputs: the
    /// settings and the current time. Output: the deadline.
    pub fn deadline(&self, now: SystemTime) -> SystemTime {
        assert!((HOURS_MIN..=HOURS_MAX).contains(&self.hours));
        let ahead = match self.left_running {
            LeftRunning::AfterHours => HOUR * u32::from(self.hours),
            LeftRunning::Never => NEVER_AHEAD,
        };
        let deadline = now + ahead;
        assert!(deadline > now);
        deadline
    }
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn invalid() -> EngineError {
    error("quit_settings_invalid")
}

/// Creates the settings table when it is missing. Input: the engine database. Output: Ok,
/// or the database error.
#[cfg(any(unix, test))]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(error)
}

/// Saves the owner's agents on quit setting over the stored one. Inputs: the open
/// transaction and the settings. Output: `Applied`, a `quit_settings_invalid` rejection for
/// hours outside 1 to 168 that changes nothing, or the database error.
pub(crate) fn save(
    connection: &Connection,
    settings: &QuitSettings,
) -> Result<Outcome, EngineError> {
    if !(HOURS_MIN..=HOURS_MAX).contains(&settings.hours) {
        return Ok(Outcome::Rejected {
            code: "quit_settings_invalid".into(),
        });
    }
    let on_quit = text(settings.on_quit)?;
    let left_running = text(settings.left_running)?;
    let stored = connection
        .execute(
            "INSERT INTO agent_quit_settings(singleton,on_quit,left_running,hours) VALUES(1,?1,?2,?3) ON CONFLICT(singleton) DO UPDATE SET on_quit=excluded.on_quit, left_running=excluded.left_running, hours=excluded.hours",
            rusqlite::params![on_quit, left_running, settings.hours],
        )
        .map_err(error)?;
    assert_eq!(stored, 1);
    Ok(Outcome::Applied)
}

/// The stored name of one choice. Input: the choice. Output: its snake case name, or
/// `quit_settings_invalid` when it does not serialize to text.
fn text(choice: impl Serialize) -> Result<String, EngineError> {
    match serde_json::to_value(choice).map_err(error)? {
        serde_json::Value::String(name) => Ok(name),
        _ => Err(invalid()),
    }
}

/// Loads the owner's agents on quit setting. Input: the engine database, writable or
/// read-only. Output: the stored setting, the default before the first save, or
/// `quit_settings_invalid` for a stored value this engine does not know, so the engine
/// acts on no guess.
pub fn load(connection: &Connection) -> Result<QuitSettings, EngineError> {
    let stored: Option<(String, String, i64)> = connection
        .query_row(
            "SELECT on_quit,left_running,hours FROM agent_quit_settings WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((on_quit, left_running, hours)) = stored else {
        return Ok(QuitSettings::default());
    };
    let on_quit =
        serde_json::from_value(serde_json::Value::String(on_quit)).map_err(|_| invalid())?;
    let left_running =
        serde_json::from_value(serde_json::Value::String(left_running)).map_err(|_| invalid())?;
    let hours = u16::try_from(hours)
        .ok()
        .filter(|hours| (HOURS_MIN..=HOURS_MAX).contains(hours))
        .ok_or_else(invalid)?;
    Ok(QuitSettings {
        on_quit,
        left_running,
        hours,
    })
}

/// The owner read of the agents on quit setting. Input: the open transaction. Output: the
/// setting as owner data.
pub(crate) fn read(connection: &Connection) -> Result<OwnerData, EngineError> {
    Ok(OwnerData::QuitSettings {
        settings: load(connection)?,
    })
}

#[cfg(test)]
mod tests;
