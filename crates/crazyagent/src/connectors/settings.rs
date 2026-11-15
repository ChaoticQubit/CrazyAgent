use crate::engine::EngineError;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS connector_settings (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    version INTEGER NOT NULL CHECK(version >= 0),
    body TEXT NOT NULL CHECK(length(body) BETWEEN 2 AND 512)
)";

const BODY_BYTES_MAX: usize = 512;

/// What a tool call that changes something at the vendor does: asks the owner every time,
/// asks once per project and connector, or is never offered. Until the approval step
/// exists the gateway hides every write tool whatever this says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritePolicy {
    AskEveryTime,
    AskOncePerProject,
    ReadOnly,
}

impl WritePolicy {
    pub const ALL: [Self; 3] = [Self::AskEveryTime, Self::AskOncePerProject, Self::ReadOnly];
}

/// How long a write waits for the owner's answer before it is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wait {
    Minutes2,
    Minutes10,
    Minutes30,
}

impl Wait {
    pub const ALL: [Self; 3] = [Self::Minutes2, Self::Minutes10, Self::Minutes30];

    /// The wait in minutes.
    pub fn minutes(self) -> u16 {
        match self {
            Self::Minutes2 => 2,
            Self::Minutes10 => 10,
            Self::Minutes30 => 30,
        }
    }
}

/// Whether a harness may also use the connectors of its own account, which the engine
/// neither filters nor logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeConnectors {
    Off,
    On,
}

impl NativeConnectors {
    pub const ALL: [Self; 2] = [Self::Off, Self::On];
}

/// The largest tool result an agent is handed; a larger one is cut with a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeCap {
    Kib64,
    Kib256,
    Mib1,
}

impl SizeCap {
    pub const ALL: [Self; 3] = [Self::Kib64, Self::Kib256, Self::Mib1];

    /// The cap in bytes.
    pub fn bytes(self) -> usize {
        match self {
            Self::Kib64 => 64 * 1024,
            Self::Kib256 => 256 * 1024,
            Self::Mib1 => 1024 * 1024,
        }
    }
}

/// How long the activity log keeps a connector call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepLog {
    Days7,
    Days30,
    Days90,
}

impl KeepLog {
    pub const ALL: [Self; 3] = [Self::Days7, Self::Days30, Self::Days90];

    /// The time kept, in days.
    pub fn days(self) -> u16 {
        match self {
            Self::Days7 => 7,
            Self::Days30 => 30,
            Self::Days90 => 90,
        }
    }
}

/// The app-wide connector settings: write actions, the approval wait, harness-native
/// connectors, the tool result size cap and how long the activity log is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorSettings {
    pub write: WritePolicy,
    pub wait: Wait,
    pub native: NativeConnectors,
    pub size_cap: SizeCap,
    pub keep_log: KeepLog,
}

impl ConnectorSettings {
    pub const DEFAULT: Self = Self {
        write: WritePolicy::AskEveryTime,
        wait: Wait::Minutes10,
        native: NativeConnectors::Off,
        size_cap: SizeCap::Kib256,
        keep_log: KeepLog::Days30,
    };
}

fn error() -> EngineError {
    EngineError("connectors_unavailable".into())
}

fn sqlite(_: rusqlite::Error) -> EngineError {
    error()
}

/// Reads the saved connector settings. Input: the engine database. Output: the settings
/// and their version, the defaults at version 0 when nothing was saved, or
/// `connectors_unavailable` for a database error or a stored value this build cannot read.
pub fn load(db: &Connection) -> Result<(ConnectorSettings, u64), EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)?;
    let row: Option<(i64, String)> = db
        .query_row(
            "SELECT version, body FROM connector_settings WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sqlite)?;
    let Some((version, body)) = row else {
        return Ok((ConnectorSettings::DEFAULT, 0));
    };
    Ok((
        serde_json::from_str(&body).map_err(|_| error())?,
        u64::try_from(version).map_err(|_| error())?,
    ))
}

/// Saves the connector settings when the owner's screen read the current version.
/// Inputs: the engine database, the settings and the version read. Output: the new
/// version, or the current version when the screen was stale, or `connectors_unavailable`.
pub fn save(
    db: &Connection,
    settings: &ConnectorSettings,
    version: u64,
) -> Result<Result<u64, u64>, EngineError> {
    let (_, current) = load(db)?;
    if current != version {
        return Ok(Err(current));
    }
    let body = serde_json::to_string(settings).map_err(|_| error())?;
    assert!(body.len() <= BODY_BYTES_MAX);
    let read = i64::try_from(version).map_err(|_| error())?;
    let next = read.checked_add(1).ok_or_else(error)?;
    let changed = db
        .execute(
            "INSERT INTO connector_settings (id, version, body) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET version=excluded.version, body=excluded.body
             WHERE connector_settings.version=?3",
            params![next, body, read],
        )
        .map_err(sqlite)?;
    if changed == 1 {
        return Ok(Ok(version + 1));
    }
    Ok(Err(load(db)?.1))
}
