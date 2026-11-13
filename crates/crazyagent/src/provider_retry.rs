use crate::coord::settings::HarnessKind;
use crate::engine::EngineError;
use crate::engine::owner::OwnerData;
use crate::protocol::Outcome;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[cfg(any(unix, test))]
const TABLE: &str = "CREATE TABLE IF NOT EXISTS provider_retry_settings (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    choice TEXT NOT NULL CHECK(choice IN ('stop_at_once', 'retry_briefly', 'harness_default'))
)";

/// The Codex setting that turns off its feature retrying a refused connect without end, so
/// its own retry counts bound the run (DEC-163 P1).
pub const CODEX_UNBOUNDED_OFF: &str = "features.unbounded_connection_retries=false";

/// The Claude Code variable that caps how many times it retries a model request.
pub const CLAUDE_MAX_RETRIES: &str = "CLAUDE_CODE_MAX_RETRIES";

/// How many times the engine's model gateway retries an RCLI model request under Retry
/// briefly.
pub const RCLI_BRIEF_RETRIES: u8 = 2;

/// What a harness does when its model provider fails ("When a model provider can't be
/// reached", DEC-163): no retries, the harness's own retries with no endless wait, or the
/// harness's own behaviour untouched. Before the first save it is Retry briefly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRetry {
    StopAtOnce,
    #[default]
    RetryBriefly,
    HarnessDefault,
}

impl ProviderRetry {
    /// Every option, in the order the setting lists them.
    pub const ALL: [Self; 3] = [Self::StopAtOnce, Self::RetryBriefly, Self::HarnessDefault];
}

/// The Codex provider table a run uses: a custom one the engine builds, named by its id,
/// or Codex's built-in `openai`, which refuses any setting under `model_providers.openai`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexProvider<'a> {
    Custom(&'a str),
    BuiltIn,
}

/// One line the setting shows for one option and one harness, saying what the option does
/// to that harness (DEC-141 decision 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryNote {
    pub option: ProviderRetry,
    pub harness: HarnessKind,
    pub line: String,
}

/// The owner's read of the setting: the chosen option and the line for every option and
/// harness, so desktop and TUI show the engine's words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRetryView {
    pub choice: ProviderRetry,
    pub notes: Vec<RetryNote>,
}

impl ProviderRetry {
    /// The `-c` settings a Codex run takes for this option. Input: the provider table the
    /// run uses. Output: the settings, in order; none under Harness default.
    pub fn codex_settings(self, provider: CodexProvider<'_>) -> Vec<String> {
        let mut settings = Vec::new();
        if self == Self::HarnessDefault {
            return settings;
        }
        settings.push(CODEX_UNBOUNDED_OFF.to_owned());
        if let (Self::StopAtOnce, CodexProvider::Custom(id)) = (self, provider) {
            assert!(
                id != "openai"
                    && !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
                "a custom Codex provider id is a plain name, never the built-in openai"
            );
            settings.push(format!("model_providers.{id}.request_max_retries=0"));
            settings.push(format!("model_providers.{id}.stream_max_retries=0"));
        }
        assert!(settings.len() <= 3);
        settings
    }

    /// The variable a Claude Code run takes for this option. Output: its name and value, or
    /// none, which leaves Claude Code's own retries.
    pub fn claude_env(self) -> Option<(&'static str, &'static str)> {
        (self == Self::StopAtOnce).then_some((CLAUDE_MAX_RETRIES, "0"))
    }

    /// How many times the model gateway retries one RCLI model request under this option.
    pub fn rcli_retries(self) -> u8 {
        match self {
            Self::RetryBriefly => RCLI_BRIEF_RETRIES,
            Self::StopAtOnce | Self::HarnessDefault => 0,
        }
    }

    /// The line the setting shows for this option and `harness`. Output: the line.
    pub fn harness_line(self, harness: HarnessKind) -> &'static str {
        match (self, harness) {
            (Self::StopAtOnce, HarnessKind::ClaudeCode) => "Claude Code: no retries.",
            (Self::StopAtOnce, HarnessKind::Codex) => {
                "Codex: no retries on an API key or address account. With a ChatGPT sign-in Codex takes no retry count, so it only stops waiting for the network without end."
            }
            (Self::StopAtOnce | Self::HarnessDefault, HarnessKind::Rcli) => "RCLI: one try.",
            (Self::RetryBriefly | Self::HarnessDefault, HarnessKind::ClaudeCode) => {
                "Claude Code: its own retries, up to 10 with longer waits each time."
            }
            (Self::RetryBriefly, HarnessKind::Codex) => {
                "Codex: its own retries, without waiting for the network without end."
            }
            (Self::RetryBriefly, HarnessKind::Rcli) => "RCLI: up to 2 retries.",
            (Self::HarnessDefault, HarnessKind::Codex) => {
                "Codex: its own retries. A provider that refuses connections keeps it waiting for the network without end."
            }
        }
    }
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError(value.to_string())
}

fn invalid() -> EngineError {
    error("provider_retry_invalid")
}

/// Creates the setting's table when it is missing. Input: the engine database. Output: Ok,
/// or the database error.
#[cfg(any(unix, test))]
pub(crate) fn initialize(connection: &Connection) -> Result<(), EngineError> {
    connection.execute_batch(TABLE).map_err(error)
}

/// Saves the owner's choice over the stored one. Inputs: the open transaction and the
/// choice. Output: `Applied`, or the database error.
pub(crate) fn save(connection: &Connection, choice: ProviderRetry) -> Result<Outcome, EngineError> {
    let serde_json::Value::String(name) = serde_json::to_value(choice).map_err(error)? else {
        return Err(invalid());
    };
    let stored = connection
        .execute(
            "INSERT INTO provider_retry_settings(singleton,choice) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET choice=excluded.choice",
            [name],
        )
        .map_err(error)?;
    assert_eq!(stored, 1);
    Ok(Outcome::Applied)
}

/// Loads the owner's choice. Input: the engine database, writable or read-only. Output: the
/// stored choice, Retry briefly before the first save, or `provider_retry_invalid` for a
/// stored value this engine does not know, so no run acts on a guess.
pub fn load(connection: &Connection) -> Result<ProviderRetry, EngineError> {
    let stored: Option<String> = connection
        .query_row(
            "SELECT choice FROM provider_retry_settings WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    let Some(name) = stored else {
        return Ok(ProviderRetry::default());
    };
    serde_json::from_value(serde_json::Value::String(name)).map_err(|_| invalid())
}

/// The option a harness launch runs under: the stored choice, or Harness default, which
/// sets nothing, when the database has no such table or holds a choice this engine does not
/// know, with the cause on stderr. Input: the engine database. Output: the option.
pub fn for_launch(connection: &Connection) -> ProviderRetry {
    load(connection).unwrap_or_else(|problem| {
        eprintln!("provider_retry_unread cause={}", problem.0);
        ProviderRetry::HarnessDefault
    })
}

/// [`for_launch`] on the engine database at `database`, opened read-only for this one read.
/// Input: the database path. Output: the option.
pub fn for_launch_at(database: &std::path::Path) -> ProviderRetry {
    match Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(connection) => for_launch(&connection),
        Err(problem) => {
            eprintln!("provider_retry_unread cause={problem}");
            ProviderRetry::HarnessDefault
        }
    }
}

/// The owner read of the setting. Input: the open transaction. Output: the choice and every
/// option's line per harness, as owner data.
pub(crate) fn read(connection: &Connection) -> Result<OwnerData, EngineError> {
    Ok(OwnerData::ProviderRetry {
        settings: view(load(connection)?),
    })
}

/// The view of `choice`: it, and the line for every option and harness in list order.
pub fn view(choice: ProviderRetry) -> ProviderRetryView {
    let notes: Vec<RetryNote> = ProviderRetry::ALL
        .into_iter()
        .flat_map(|option| {
            HarnessKind::ALL.into_iter().map(move |harness| RetryNote {
                option,
                harness,
                line: option.harness_line(harness).to_owned(),
            })
        })
        .collect();
    assert_eq!(
        notes.len(),
        ProviderRetry::ALL.len() * HarnessKind::ALL.len()
    );
    ProviderRetryView { choice, notes }
}

#[cfg(test)]
mod tests;
