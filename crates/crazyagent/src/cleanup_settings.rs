use crate::engine::EngineError;
use crate::engine::owner::OwnerData;
#[cfg(test)]
use crate::protocol::Outcome;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub const DAYS_MIN: u16 = 1;
pub const DAYS_MAX: u16 = 365;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS agent_cleanup_settings (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    mode TEXT NOT NULL CHECK(mode IN ('after_days', 'once_landed', 'manual')),
    days INTEGER NOT NULL CHECK(days BETWEEN 1 AND 365)
)";

/// When the engine removes a finished agent's worktree on its own: a number of days after
/// the agent finished, as soon as its work landed, or never, only when the owner clicks
/// Clean up. Every option removes only a worktree the cleanup checks let go.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupMode {
    #[default]
    AfterDays,
    OnceLanded,
    Manual,
}

/// The owner's app-wide choice for finished agents' worktrees ("Remove finished agents'
/// worktrees"): the mode and the days the first mode waits, kept while another mode is
/// picked. Before the first save every value is its default, 1 day after the agent finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupSettings {
    pub mode: CleanupMode,
    pub days: u16,
}

impl Default for CleanupSettings {
    fn default() -> Self {
        Self {
            mode: CleanupMode::AfterDays,
            days: DAYS_MIN,
        }
    }
}

impl CleanupSettings {
    /// How long a finished agent waits before the engine may remove its worktree on its own.
    /// Input: the settings. Output: the wait in milliseconds, zero once its work landed, or
    /// None when only the owner's Clean up removes one.
    pub fn wait_ms(&self) -> Option<i64> {
        assert!((DAYS_MIN..=DAYS_MAX).contains(&self.days));
        match self.mode {
            CleanupMode::AfterDays => Some(i64::from(self.days) * DAY_MS),
            CleanupMode::OnceLanded => Some(0),
            CleanupMode::Manual => None,
        }
    }
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn invalid() -> EngineError {
    error("cleanup_settings_invalid")
}

/// Creates the settings table when it is missing. Input: the engine database. Output: Ok,
/// or the database error.
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(error)
}

/// Checks the owner's finished agents setting before it is saved. Input: the settings.
/// Output: Ok, or `cleanup_settings_invalid` for days outside 1 to 365.
pub(crate) fn check(settings: &CleanupSettings) -> Result<(), EngineError> {
    if !(DAYS_MIN..=DAYS_MAX).contains(&settings.days) {
        return Err(invalid());
    }
    Ok(())
}

/// Stores the owner's finished agents setting for a test, checked, through the settings
/// log's projection; the engine saves it only through `settings_log::owner`, which logs the
/// save (v2.5 L10). Inputs: the connection and the settings. Output: `Applied`,
/// `cleanup_settings_invalid` for days outside 1 to 365, or the store's refusal.
#[cfg(test)]
pub(crate) fn save(
    connection: &Connection,
    settings: &CleanupSettings,
) -> Result<Outcome, EngineError> {
    check(settings)?;
    let event = crate::settings_log::Event::Cleanup(crate::settings_log::Saved {
        settings: *settings,
    });
    crate::settings_log::apply(connection, &event)?;
    Ok(Outcome::Applied)
}

/// Loads the owner's finished agents setting. Input: the engine database, writable or
/// read-only. Output: the stored setting, the default before the first save, or
/// `cleanup_settings_invalid` for a stored value this engine does not know, so the sweep
/// removes nothing rather than act on a guess.
pub fn load(connection: &Connection) -> Result<CleanupSettings, EngineError> {
    let stored: Option<(String, i64)> = connection
        .query_row(
            "SELECT mode,days FROM agent_cleanup_settings WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(error)?;
    let Some((mode, days)) = stored else {
        return Ok(CleanupSettings::default());
    };
    let mode = serde_json::from_value(serde_json::Value::String(mode)).map_err(|_| invalid())?;
    let days = u16::try_from(days)
        .ok()
        .filter(|days| (DAYS_MIN..=DAYS_MAX).contains(days))
        .ok_or_else(invalid)?;
    Ok(CleanupSettings { mode, days })
}

/// The owner read of the finished agents setting. Input: the open transaction. Output: the
/// setting as owner data.
pub(crate) fn read(connection: &Connection) -> Result<OwnerData, EngineError> {
    Ok(OwnerData::CleanupSettings {
        settings: load(connection)?,
    })
}

#[cfg(test)]
mod tests;
