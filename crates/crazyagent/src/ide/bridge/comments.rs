use super::changes::{agent_name, title};
use super::{COMMENT_BYTES_MAX, IdeCall, NOTE_PATH_BYTES_MAX, shown};
use crate::config::ModelAccess;
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::landing::{self, AgentKind, ChangeSetId, ChangeState, FileStatus};
use crate::protocol::{Outcome, ProjectId};
use crate::team::events::{SEND_BYTES_MAX, Said, send_to_agent};
use crate::team::{AgentStatus, MESSAGE_BYTES_MAX, TeamAction};
use crate::terminal::Host;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::path::PathBuf;
use std::sync::Arc;

const AGENT_BYTES_MAX: usize = 128;
const TITLE_BYTES_MAX: usize = 256;
const HEADER_BYTES_MAX: usize = AGENT_BYTES_MAX + TITLE_BYTES_MAX + NOTE_PATH_BYTES_MAX + 64;
const _: () = assert!(HEADER_BYTES_MAX + COMMENT_BYTES_MAX <= SEND_BYTES_MAX);
const _: () = assert!(SEND_BYTES_MAX <= MESSAGE_BYTES_MAX);

/// Answers `ide.comment` on a blocking thread, as the other Changes calls run. Inputs: the
/// engine root holding `engine.db`, the engine's terminal host when it has one, the project
/// and the call. Output: null once the comment reached the set's agent, or the refusal.
pub(crate) async fn answer(
    root: PathBuf,
    host: Option<Arc<dyn Host>>,
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
        comment(&db, host.as_deref(), &project, &id, &path, line, &text)?;
        Ok(serde_json::Value::Null)
    })
    .await
    .map_err(|_| failure("changes_unavailable"))?
}

/// The one message a comment becomes: whose changes, which ones, the file and line, then
/// the owner's words with Windows and old Mac line ends made plain. Inputs: the agent, the
/// set's title, the path, the line and the comment. Output: the message text.
fn message(agent: &str, title: &str, path: &str, line: u32, text: &str) -> String {
    let header = format!(
        "On {}'s changes \"{}\", {} line {line}:",
        shown(agent, AGENT_BYTES_MAX),
        shown(title, TITLE_BYTES_MAX),
        shown(path, NOTE_PATH_BYTES_MAX),
    );
    assert!(header.len() <= HEADER_BYTES_MAX);
    format!(
        "{header}\n\n{}",
        text.replace("\r\n", "\n").replace('\r', "\n")
    )
}

/// Sends an owner's diff comment to the agent that made the change set. A team master's
/// set takes it as one owner message in the project's team conversation through the team
/// Message path; its frame quotes the agent's own text, so it is never an owner turn a
/// master may name as an answer. A sub-agent's set types it into that agent's live terminal
/// as an engine notice in its Messages timeline. An owner conversation's set is refused,
/// since the IDE holds no owner admission to drive that conversation. Only an open set of
/// the IDE's project and a file it changed that still exists take a comment. Inputs: the
/// store, the terminal host, the project, the set, the path, the line from 1 and the text.
/// Output: Ok once delivered, or `changes_not_found`, `changes_closed`,
/// `ide_comment_master_unavailable` or the team's refusal.
pub(crate) fn comment(
    db: &Connection,
    host: Option<&dyn Host>,
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
    if view.agent.kind == AgentKind::Master {
        return Err(failure("ide_comment_master_unavailable"));
    }
    let Ok(agent) = view.agent.id.parse::<i64>() else {
        return Err(failure("team_agent_not_found"));
    };
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT role,status FROM team_agents WHERE id=?1 AND project=?2",
            params![agent, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| failure("changes_unavailable"))?;
    let text = message(
        &agent_name(&view.agent.label),
        &title(&view.message),
        path,
        line,
        text,
    );
    match row {
        Some((role, _)) if role == "master" => to_conversation(db, project, text),
        Some((role, status)) if role == "sub" => {
            if !AgentStatus::parse(&status).is_some_and(AgentStatus::live) {
                return Err(failure("team_agent_not_running"));
            }
            let host = host.ok_or_else(|| failure("team_agent_unreachable"))?;
            match send_to_agent(
                db,
                host,
                project,
                &format!("team-{agent}"),
                Said::Notice,
                &text,
            )? {
                Ok(typed) => {
                    assert_eq!(typed, agent);
                    Ok(())
                }
                Err(code) => Err(failure(code)),
            }
        }
        _ => Err(failure("team_agent_not_found")),
    }
}

/// Stores a comment for the team master as one owner message in the project's team
/// conversation, so the conversation's bounds and refusals hold. Inputs: the store, the
/// project and the message. Output: Ok once stored, or the team's refusal.
fn to_conversation(db: &Connection, project: &ProjectId, text: String) -> Result<(), EngineError> {
    let action = TeamAction::Message {
        project_id: project.clone(),
        text,
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
