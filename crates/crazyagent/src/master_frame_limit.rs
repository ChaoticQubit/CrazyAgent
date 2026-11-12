use crate::engine::EngineError;
use crate::engine::owner::OwnerData;
use crate::protocol::Outcome;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// The largest frame the engine writes to a configured master's harness under any option.
pub const BYTES_MAX: usize = 1_048_576;

#[cfg(any(unix, test))]
const TABLE: &str = "CREATE TABLE IF NOT EXISTS configured_master_frames (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    frame_limit TEXT NOT NULL CHECK(frame_limit IN ('kib64', 'kib256', 'mib1'))
)";

/// The largest one message, one JSON line, the engine writes to a Claude or Codex master's
/// harness: the thread start that carries the master's instructions and each turn's
/// prompt. 64 KiB is the engine's own wire frame; what the harness writes back is bounded
/// by `adapters::LINE_BYTES_MAX` whatever this is, and rcli keeps 64 KiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameSize {
    #[default]
    Kib64,
    Kib256,
    Mib1,
}

impl FrameSize {
    /// Every choice, smallest first, in the order the settings show them.
    pub const ALL: [Self; 3] = [Self::Kib64, Self::Kib256, Self::Mib1];

    /// The bound the choice puts on one frame. Input: the choice. Output: the bytes, from
    /// the engine wire frame to `BYTES_MAX`.
    pub fn bytes(self) -> usize {
        let bytes = match self {
            Self::Kib64 => 65_536,
            Self::Kib256 => 262_144,
            Self::Mib1 => 1_048_576,
        };
        assert!((crate::protocol::FRAME_BYTES_MAX..=BYTES_MAX).contains(&bytes));
        bytes
    }
}

/// The owner's app-wide choice of the master frame size limit (F39). Before the first save
/// it is its default, 64 KiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterFrameSettings {
    pub limit: FrameSize,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn invalid() -> EngineError {
    error("master_frame_settings_invalid")
}

/// Creates the settings table when it is missing. Input: the engine database. Output: Ok,
/// or the database error.
#[cfg(any(unix, test))]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(error)
}

/// Saves the owner's master frame size limit over the stored one. Inputs: the open
/// transaction and the settings. Output: `Applied`, or the database error.
pub(crate) fn save(
    connection: &Connection,
    settings: &MasterFrameSettings,
) -> Result<Outcome, EngineError> {
    let limit = match serde_json::to_value(settings.limit).map_err(error)? {
        serde_json::Value::String(name) => name,
        _ => return Err(invalid()),
    };
    let stored = connection
        .execute(
            "INSERT INTO configured_master_frames(singleton,frame_limit) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET frame_limit=excluded.frame_limit",
            rusqlite::params![limit],
        )
        .map_err(error)?;
    assert_eq!(stored, 1);
    Ok(Outcome::Applied)
}

/// Loads the owner's master frame size limit. Input: the engine database, writable or
/// read-only. Output: the stored setting, the default before the first save, or
/// `master_frame_settings_invalid` for a stored value this engine does not know, so the
/// engine acts on no guess.
pub fn load(connection: &Connection) -> Result<MasterFrameSettings, EngineError> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT frame_limit FROM configured_master_frames WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(limit) = stored else {
        return Ok(MasterFrameSettings::default());
    };
    let limit = serde_json::from_value(serde_json::Value::String(limit)).map_err(|_| invalid())?;
    Ok(MasterFrameSettings { limit })
}

/// The owner read of the master frame size limit. Input: the open transaction. Output: the
/// setting as owner data.
pub(crate) fn read(connection: &Connection) -> Result<OwnerData, EngineError> {
    Ok(OwnerData::MasterFrameSettings {
        settings: load(connection)?,
    })
}

#[cfg(test)]
mod tests;
