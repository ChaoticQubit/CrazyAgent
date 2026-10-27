use super::IdeCall;
use super::changes::{agent_name, title};
use crate::config::ModelAccess;
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::landing::{self, ChangeSetId, ChangeState, FileStatus};
use crate::protocol::{Outcome, ProjectId};
use crate::team::TeamAction;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::path::PathBuf;

const SHOWN_CHARS_MAX: usize = 200;

/// Answers `ide.comment` on a blocking thread, as the other Changes calls run. Inputs: the
/// engine root holding `engine.db`, the project and the call. Output: null once the
/// comment is in the team conversation, or the refusal.
pub(crate) async fn answer(
    root: PathBuf,
    project: ProjectId,
    call: IdeCall,
) -> Result<serde_json::Value, EngineError> {
    let IdeCall::Comment {
        id,
        path,
        line,
        text,
    } = call
    else {
        return Err(failure("ide_request_invalid"));
    };
    tokio::task::spawn_blocking(move || {
        let db = landing::open(&root)?;
        comment(&db, &project, &id, &path, line, &text)?;
        Ok(serde_json::Value::Null)
    })
    .await
    .map_err(|_| failure("changes_unavailable"))?
}

/// Engine or agent text as one plain line inside the message: control characters become
/// U+FFFD and it is cut at `SHOWN_CHARS_MAX` characters. Input: the text. Output: the line.
fn shown(text: &str) -> String {
    text.chars()
        .take(SHOWN_CHARS_MAX)
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

/// The one message a comment becomes: whose changes, which ones, the file and line, then
/// the owner's words with Windows line ends made plain. Inputs: the agent, the set's title,
/// the path, the line and the comment. Output: the message text.
fn message(agent: &str, title: &str, path: &str, line: u32, text: &str) -> String {
    format!(
        "On {}'s changes \"{}\", {} line {line}:\n\n{}",
        shown(agent),
        shown(title),
        shown(path),
        text.replace("\r\n", "\n")
    )
}

/// Sends an owner's diff comment to a change set's agent as one owner message in the
/// project's team conversation, through the team Message path the owner's own messages
/// take, so its bounds and refusals hold; its frame quotes the agent's own text, so it is
/// never an owner turn a master may name as an answer. Only an open set of the IDE's
/// project and a file it changed that still exists take a comment. Inputs: the store, the
/// project, the set, the path, the line from 1 and the text. Output: Ok once stored, or
/// `changes_not_found`, `changes_closed` or the team's refusal.
pub(crate) fn comment(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    path: &str,
    line: u32,
    text: &str,
) -> Result<(), EngineError> {
    let view = landing::ide::view(db, project, id)?;
    if !matches!(
        view.state,
        ChangeState::Pending | ChangeState::Conflict | ChangeState::Updating
    ) {
        return Err(failure("changes_closed"));
    }
    let changed = landing::ide::files(db, project, id)?
        .iter()
        .any(|file| file.path == path && file.status != FileStatus::Deleted);
    if !changed {
        return Err(failure("changes_not_found"));
    }
    let action = TeamAction::Message {
        project_id: project.clone(),
        text: message(
            &agent_name(&view.agent.label),
            &title(&view.message),
            path,
            line,
            text,
        ),
    };
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)
        .map_err(|_| failure("changes_unavailable"))?;
    match crate::team::execute_framed(&tx, &ModelAccess::default(), &action)? {
        Outcome::Applied => {}
        Outcome::Rejected { code } => return Err(EngineError(code)),
        _ => return Err(failure("changes_outcome_unknown")),
    }
    tx.commit().map_err(|_| failure("changes_unavailable"))
}

#[cfg(all(test, unix))]
mod tests;
