use crate::engine::EngineError;
use crate::engine::owner::OwnerData;
use crate::state_log::Aggregate;
use kernel_storage::state_log::compact::Compaction;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

const TABLE: &str = "CREATE TABLE IF NOT EXISTS log_compaction_settings (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    choice TEXT NOT NULL CHECK(choice IN ('off', 'on'))
)";

/// The row's label, the same words in desktop and TUI.
pub const LABEL: &str = "Compact the event log";

/// The row's help line, the same words in desktop and TUI.
pub const HELP: &str = "Dev builds only. Writes a snapshot every 10,000 events and clears the saved data of older log entries; the entries stay.";

/// Whether the engine compacts the state log, a dev-build switch: Off before the first save.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogCompaction {
    #[default]
    Off,
    On,
}

impl LogCompaction {
    /// Every option, in the order the row lists them.
    pub const ALL: [Self; 2] = [Self::Off, Self::On];
}

/// The owner's read of the switch: the stored choice and whether this build is a dev build,
/// so desktop and TUI show the row only where it acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogCompactionView {
    pub choice: LogCompaction,
    pub dev_build: bool,
}

/// Whether a workspace version is a dev build's: the release workflow stamps dev builds
/// `X.Y.Z-dev.<yyyymmddHHMM>`. Input: the version. Output: true for a dev stamp.
pub fn dev_build(version: &str) -> bool {
    version.contains("-dev.")
}

/// Whether this engine binary is a dev build. Output: [`dev_build`] of its own version.
pub fn this_build_is_dev() -> bool {
    dev_build(env!("CARGO_PKG_VERSION"))
}

/// The compaction an aggregate's write runs under: Off outside a dev build, Off for the
/// Plugins aggregate whatever is stored, since a namespace value inside a snapshot chunk
/// could never be redacted, else the stored choice. Inputs: the aggregate, whether the build
/// is a dev build and the stored choice. Output: the kernel switch.
pub fn compaction_for(aggregate: Aggregate, dev: bool, stored: LogCompaction) -> Compaction {
    if !dev || aggregate == Aggregate::Plugins || stored == LogCompaction::Off {
        return Compaction::Off;
    }
    Compaction::On
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

/// Creates the switch's table when it is missing. Input: the engine database. Output: Ok,
/// or the database error.
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(error)
}

/// Loads the stored choice. Input: the engine database. Output: the choice, Off before the
/// first save or when the store has no table, or `log_compaction_invalid` for a stored word
/// this engine does not know.
pub fn load(connection: &Connection) -> Result<LogCompaction, EngineError> {
    let stored: Option<String> = match connection
        .query_row(
            "SELECT choice FROM log_compaction_settings WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
    {
        Ok(stored) => stored,
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.starts_with("no such table") =>
        {
            None
        }
        Err(problem) => return Err(error(problem)),
    };
    let Some(word) = stored else {
        return Ok(LogCompaction::default());
    };
    serde_json::from_value(serde_json::Value::String(word))
        .map_err(|_| error("log_compaction_invalid"))
}

/// The owner read of the switch. Input: the open transaction. Output: the view as owner data.
pub(crate) fn read(connection: &Connection) -> Result<OwnerData, EngineError> {
    Ok(OwnerData::LogCompaction {
        settings: LogCompactionView {
            choice: load(connection)?,
            dev_build: this_build_is_dev(),
        },
    })
}
