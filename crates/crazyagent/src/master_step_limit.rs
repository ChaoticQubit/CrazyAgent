use crate::engine::EngineError;
use crate::engine::owner::OwnerData;
use crate::protocol::Outcome;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// How the cause of a master turn that used every step its limit allows starts; the number
/// of steps follows, as in `master_step_limit_50`, a plain code an owner window may show.
const CAUSE_PREFIX: &str = "master_step_limit_";

#[cfg(any(unix, test))]
const TABLE: &str = "CREATE TABLE IF NOT EXISTS master_step_settings (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    step_limit TEXT NOT NULL CHECK(step_limit IN ('no_limit', 'steps50', 'steps100', 'steps200'))
)";

/// How many steps a master with its tools on may take for one message before it stops and
/// waits for the owner: no limit, or 50, 100 or 200 steps. A step is one round in which
/// the master uses its tools, as Claude Code counts for `--max-turns`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepLimit {
    #[default]
    NoLimit,
    Steps50,
    Steps100,
    Steps200,
}

impl StepLimit {
    /// Every choice, in the order the settings show them.
    pub const ALL: [Self; 4] = [Self::NoLimit, Self::Steps50, Self::Steps100, Self::Steps200];

    /// The number of steps the choice allows. Input: the choice. Output: the number, or
    /// none for no limit.
    pub fn steps(self) -> Option<u16> {
        match self {
            Self::NoLimit => None,
            Self::Steps50 => Some(50),
            Self::Steps100 => Some(100),
            Self::Steps200 => Some(200),
        }
    }

    /// The choice that allows exactly this many steps, as written in a launch argument.
    /// Input: the number as text. Output: the choice, or none for any other text, so a
    /// command with a step count the owner cannot pick is never taken for the engine's own.
    pub fn of_steps(text: &str) -> Option<Self> {
        let found = Self::ALL
            .into_iter()
            .find(|limit| limit.steps().is_some_and(|steps| steps.to_string() == text));
        assert!(found != Some(Self::NoLimit));
        found
    }

    /// The cause a turn stopped by this limit ends with. Input: the choice. Output: the
    /// cause, or none for no limit, which stops no turn.
    pub fn cause(self) -> Option<String> {
        self.steps().map(|steps| format!("{CAUSE_PREFIX}{steps}"))
    }
}

/// The number of steps a stopped turn's cause names. Input: the cause. Output: the number,
/// or none for another cause or a number no limit allows.
pub fn steps_of_cause(cause: &str) -> Option<u16> {
    let limit = StepLimit::of_steps(cause.strip_prefix(CAUSE_PREFIX)?)?;
    let steps = limit.steps();
    assert!(steps.is_some());
    steps
}

/// The owner's app-wide choice of the master step limit ("Master step limit", OB-5b).
/// Before the first save it is its default, no limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasterStepSettings {
    pub limit: StepLimit,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn invalid() -> EngineError {
    error("master_step_settings_invalid")
}

/// Creates the settings table when it is missing. Input: the engine database. Output: Ok,
/// or the database error.
#[cfg(any(unix, test))]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(error)
}

/// Saves the owner's master step limit over the stored one. Inputs: the open transaction
/// and the settings. Output: `Applied`, or the database error.
pub(crate) fn save(
    connection: &Connection,
    settings: &MasterStepSettings,
) -> Result<Outcome, EngineError> {
    let limit = match serde_json::to_value(settings.limit).map_err(error)? {
        serde_json::Value::String(name) => name,
        _ => return Err(invalid()),
    };
    let stored = connection
        .execute(
            "INSERT INTO master_step_settings(singleton,step_limit) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET step_limit=excluded.step_limit",
            rusqlite::params![limit],
        )
        .map_err(error)?;
    assert_eq!(stored, 1);
    Ok(Outcome::Applied)
}

/// Loads the owner's master step limit. Input: the engine database, writable or read-only.
/// Output: the stored setting, the default before the first save, or
/// `master_step_settings_invalid` for a stored value this engine does not know, so the
/// engine acts on no guess.
pub fn load(connection: &Connection) -> Result<MasterStepSettings, EngineError> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT step_limit FROM master_step_settings WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(limit) = stored else {
        return Ok(MasterStepSettings::default());
    };
    let limit = serde_json::from_value(serde_json::Value::String(limit)).map_err(|_| invalid())?;
    Ok(MasterStepSettings { limit })
}

/// The owner read of the master step limit. Input: the open transaction. Output: the
/// setting as owner data.
pub(crate) fn read(connection: &Connection) -> Result<OwnerData, EngineError> {
    Ok(OwnerData::MasterStepSettings {
        settings: load(connection)?,
    })
}

#[cfg(test)]
mod tests;
