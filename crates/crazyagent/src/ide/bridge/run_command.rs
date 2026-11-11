use super::reveal::REVEAL_WAIT;
use super::{Grants, IdeEvent};
use crate::engine::EngineError;
use crate::engine::owner::{OwnerRuntimeHandle, failure};
use crate::protocol::{ProjectId, RequestId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// The most bytes one command typed into a project terminal may hold.
pub const COMMAND_BYTES_MAX: usize = 512;
/// The refusal of a blank command.
pub const EMPTY: &str = "ide_run_empty";
/// The refusal of a command over `COMMAND_BYTES_MAX` bytes.
pub const TOO_LONG: &str = "ide_run_too_long";
/// The refusal of a command with a line break.
pub const NOT_ONE_LINE: &str = "ide_run_not_one_line";
/// The refusal of a command with a control, bidirectional or invisible character.
pub const CONTROL: &str = "ide_run_control_character";
/// The refusal of a command that chains, pipes, redirects, substitutes or recalls history.
pub const CHAINED: &str = "ide_run_chained";
/// The refusal of a run while the project's setting is Off.
pub const OFF: &str = "ide_run_off";
/// The refusal of a run the project's IDE did not acknowledge within `REVEAL_WAIT`.
pub const UNANSWERED: &str = "ide_run_unanswered";
const CHAINING: [char; 7] = [';', '|', '&', '`', '<', '>', '!'];
const REQUEST_PREFIX: &str = "run-";
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ide_run_settings (
    project TEXT PRIMARY KEY,
    mode TEXT NOT NULL CHECK(mode IN ('type_and_run','type_only','off'))
);";

/// What "Run in project terminal" does for a project: types the command and presses Enter,
/// types it and leaves Enter to the owner, or is off so only Copy is offered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    #[default]
    TypeAndRun,
    TypeOnly,
    Off,
}

impl RunMode {
    /// The mode's name in the settings table and on the wire. Output: the name.
    fn name(self) -> &'static str {
        match self {
            Self::TypeAndRun => "type_and_run",
            Self::TypeOnly => "type_only",
            Self::Off => "off",
        }
    }

    /// The mode a saved name stands for. Input: the name. Output: the mode, or None for a
    /// name no mode has.
    fn named(name: &str) -> Option<Self> {
        [Self::TypeAndRun, Self::TypeOnly, Self::Off]
            .into_iter()
            .find(|mode| mode.name() == name)
    }
}

/// Says why a command may not be typed into a project terminal, or that it may. The text is
/// checked as written, with no shell parsing, so a quoted `;` is refused too.
/// Input: the command as the owner saw it. Output: None for one plain single command, else
/// the refusal code.
pub fn problem(command: &str) -> Option<&'static str> {
    if command.trim().is_empty() {
        return Some(EMPTY);
    }
    if command.len() > COMMAND_BYTES_MAX {
        return Some(TOO_LONG);
    }
    if command.contains(['\n', '\r']) {
        return Some(NOT_ONE_LINE);
    }
    if command.chars().any(hidden) {
        return Some(CONTROL);
    }
    if command.contains(CHAINING) || command.contains("$(") || command.trim_start().starts_with('^')
    {
        return Some(CHAINED);
    }
    assert!(command.len() <= COMMAND_BYTES_MAX);
    assert!(!command.contains('\n'));
    None
}

/// Whether a character can hide or reorder what the owner reads: a control character, a
/// bidirectional mark, a line or paragraph separator, or an invisible format character.
/// Input: the character. Output: the answer.
fn hidden(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{ad}'
                | '\u{61c}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
                | '\u{e0000}'..='\u{e007f}'
        )
}

/// Reads a project's Run setting. Inputs: the engine database and the project.
/// Output: the saved mode, Type and run when none is saved, or `ide_unavailable`.
pub fn mode(db: &Connection, project: &ProjectId) -> Result<RunMode, EngineError> {
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    let saved: Option<String> = db
        .query_row(
            "SELECT mode FROM ide_run_settings WHERE project=?1",
            params![project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    match saved {
        None => Ok(RunMode::default()),
        Some(name) => RunMode::named(&name).ok_or_else(|| failure("ide_unavailable")),
    }
}

/// Saves a project's Run setting. Inputs: the engine database, the project and the mode.
/// Output: Ok once the project's one row holds the mode, or `ide_unavailable`.
pub fn set_mode(db: &Connection, project: &ProjectId, mode: RunMode) -> Result<(), EngineError> {
    assert!(RunMode::named(mode.name()) == Some(mode));
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    let changed = db
        .execute(
            "INSERT INTO ide_run_settings(project, mode) VALUES (?1,?2)
             ON CONFLICT(project) DO UPDATE SET mode=excluded.mode",
            params![project.as_str(), mode.name()],
        )
        .map_err(unavailable)?;
    assert!(changed == 1, "one project has one Run setting row");
    Ok(())
}

/// Reads a project's Run setting from the running engine's database, off the async
/// threads. Inputs: the owner runtime and the project. Output: the mode.
pub(crate) async fn saved_mode(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
) -> Result<RunMode, EngineError> {
    let root = runtime.ide()?.root;
    let asked = project.clone();
    tokio::task::spawn_blocking(move || mode(&crate::landing::open(&root)?, &asked))
        .await
        .map_err(|_| failure("ide_unavailable"))?
}

/// Saves a project's Run setting in the running engine's database, off the async threads.
/// Inputs: the owner runtime, the project and the mode. Output: Ok once saved.
pub(crate) async fn save_mode(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    mode: RunMode,
) -> Result<(), EngineError> {
    let root = runtime.ide()?.root;
    let asked = project.clone();
    tokio::task::spawn_blocking(move || set_mode(&crate::landing::open(&root)?, &asked, mode))
        .await
        .map_err(|_| failure("ide_unavailable"))?
}

/// Types one command the owner confirmed into a new terminal of the project's open IDE:
/// refuses while the setting is Off, refuses a command that breaks the single-command
/// rule, needs exactly one IDE listening so the command is typed once, sends one
/// `run_command` event, marks the project's IDE tab for the desktop, and waits up to
/// `REVEAL_WAIT` for the extension to say it typed it. Inputs: the owner runtime, the
/// project and the command. Output: the run's id, or `ide_run_off`, a rule refusal,
/// `ide_not_running`, `ide_reveal_busy` or `ide_run_unanswered`.
pub(crate) async fn run(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    command: &str,
) -> Result<RequestId, EngineError> {
    let press_enter = match saved_mode(runtime, project).await? {
        RunMode::Off => return Err(failure(OFF)),
        RunMode::TypeAndRun => true,
        RunMode::TypeOnly => false,
    };
    if let Some(code) = problem(command) {
        return Err(failure(code));
    }
    let grants = runtime.ide_grants();
    match grants.listeners(project) {
        0 => return Err(failure("ide_not_running")),
        1 => {}
        _ => return Err(failure("ide_reveal_busy")),
    }
    let random = crate::engine::owner::random_id()?;
    let request = RequestId::new(format!("{REQUEST_PREFIX}{}", &random[..24]))
        .map_err(|_| failure("ide_unavailable"))?;
    let acked = grants.reveals.wait(project, &request)?;
    assert!(
        problem(command).is_none(),
        "only a command the rule passes is sent"
    );
    assert!(request.as_str().starts_with(REQUEST_PREFIX));
    grants.publish(
        project,
        IdeEvent::RunCommand {
            request: request.clone(),
            command: command.to_owned(),
            press_enter,
        },
    );
    let now_ms = u64::try_from(crate::team::now_ms()).unwrap_or(0);
    grants.reveals.mark(project, &request, now_ms);
    let answered = tokio::time::timeout(REVEAL_WAIT, acked).await;
    grants.reveals.forget(&request);
    match answered {
        Ok(Ok(())) => Ok(request),
        _ => Err(failure(UNANSWERED)),
    }
}

/// Answers `ide.run_ack`: the project's extension typed the command. Only a run's own id
/// is taken, so this call never completes a reveal. Inputs: the owner runtime, the
/// capability's project and the run. Output: null data, or `ide_reveal_unknown` for a run
/// this project is not waiting on.
pub(super) fn acked(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    request: &RequestId,
) -> Result<serde_json::Value, EngineError> {
    if !request.as_str().starts_with(REQUEST_PREFIX) {
        return Err(failure("ide_reveal_unknown"));
    }
    runtime.ide_grants().reveals.ack(project, request)?;
    Ok(serde_json::Value::Null)
}

impl Grants {
    /// How many event subscriptions a project's IDE holds right now: none means the
    /// desktop has no IDE open for it, and more than one would type a command more than
    /// once. Input: the project. Output: the count, zero when the grants cannot be read.
    fn listeners(&self, project: &ProjectId) -> usize {
        self.live.lock().map_or(0, |live| {
            live.iter()
                .filter(|grant| &grant.project == project)
                .map(|grant| grant.events.receiver_count())
                .sum()
        })
    }
}

/// The refusal of a Run setting read or write the engine database failed.
fn unavailable(_: rusqlite::Error) -> EngineError {
    failure("ide_unavailable")
}

#[cfg(test)]
mod tests;
