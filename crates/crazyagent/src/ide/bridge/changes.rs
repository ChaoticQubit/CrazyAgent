use crate::engine::EngineError;
use crate::landing::{
    self, AcceptOutcome, AgentKind, ChangeDiff, ChangeSetId, ChangeSetView, ChangeState,
    ChangedFile,
};
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde::Serialize;
use std::path::{Path, PathBuf};

const TITLE_CHARS_MAX: usize = 80;

/// One change set as the IDE's Source Control shows it: one group named
/// "<agent>: <title>" whose files open VS Code's diff between the base text and the agent
/// worktree. `head` is what Accept and Discard send back, so a set that moved on since the
/// list is refused as stale. `message` is the commit message Accept proposes. `problem`
/// is the plain code when the files could not be read; the group then shows that instead.
/// `waiting` marks an Updating set already merged and waiting for its agent, which the
/// owner can still discard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct IdeChangeSet {
    pub(crate) id: ChangeSetId,
    pub(crate) agent: String,
    pub(crate) role: AgentKind,
    pub(crate) title: String,
    pub(crate) state: ChangeState,
    pub(crate) waiting: bool,
    pub(crate) head: String,
    pub(crate) message: String,
    pub(crate) worktree: PathBuf,
    pub(crate) files: Vec<ChangedFile>,
    pub(crate) restorable_until: Option<u64>,
    pub(crate) problem: Option<String>,
}

/// The agent's name and role without the harness the change set's label adds after a
/// middle dot. Input: the label. Output: the name, such as "Ada (@builder)".
fn agent_name(label: &str) -> String {
    label
        .split(" \u{b7} ")
        .next()
        .unwrap_or(label)
        .trim()
        .to_owned()
}

/// The first line of the proposed commit message, at most `TITLE_CHARS_MAX` characters.
/// Input: the message. Output: the title.
fn title(message: &str) -> String {
    let line = message.lines().next().unwrap_or("").trim();
    let mut title: String = line.chars().take(TITLE_CHARS_MAX).collect();
    if line.chars().count() > TITLE_CHARS_MAX {
        title.push_str("...");
    }
    title
}

/// Whether a change set has a Source Control group: every set but an Accepted one, and a
/// Discarded one only while it can still be restored. Inputs: the set and now.
fn shown(view: &ChangeSetView, now_ms: u64) -> bool {
    match view.state {
        ChangeState::Accepted => false,
        ChangeState::Discarded => view.restorable_until_ms.is_some_and(|until| until > now_ms),
        ChangeState::Pending | ChangeState::Conflict | ChangeState::Updating => true,
    }
}

/// Answers `ide.change_sets`: the project's change sets that have a Source Control group,
/// each with every file it touches. A set whose files cannot be read still shows, with its
/// problem code. Inputs: the store, the IDE's project and now. Output: the sets.
pub(crate) fn change_sets(
    db: &Connection,
    project: &ProjectId,
    now_ms: u64,
) -> Result<Vec<IdeChangeSet>, EngineError> {
    let mut sets = Vec::new();
    for view in landing::list(db, project)? {
        if !shown(&view, now_ms) {
            continue;
        }
        let located = landing::ide::locate(db, project, &view.id)?;
        let (files, problem) = match landing::ide::files(db, project, &view.id) {
            Ok(files) => (files, None),
            Err(error) => (
                Vec::new(),
                Some(crate::protocol::plain_code(&error.0).to_owned()),
            ),
        };
        sets.push(IdeChangeSet {
            agent: agent_name(&view.agent.label),
            role: view.agent.kind,
            title: title(&view.message),
            state: view.state,
            waiting: view.waiting,
            head: view.head.clone(),
            message: view.message.clone(),
            worktree: located.worktree,
            files,
            restorable_until: view.restorable_until_ms,
            problem,
            id: view.id,
        });
    }
    Ok(sets)
}

/// Answers `ide.change_diff`: one page of a change set of the IDE's project. Inputs: the
/// store, the project, the change set and the page. Output: the page.
pub(crate) fn change_diff(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    page: u16,
) -> Result<ChangeDiff, EngineError> {
    landing::ide::locate(db, project, id)?;
    landing::diff(db, id, page)
}

/// Answers `ide.base_blob`: the base text of one text file of a change set of the IDE's
/// project, the left side of its diff. Inputs: the store, the project, the change set and
/// the path. Output: the text, empty for an added file.
pub(crate) fn base_blob(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    path: &str,
) -> Result<String, EngineError> {
    landing::ide::base_text(db, project, id, path)
}

/// Answers `ide.accept`: lands a change set of the IDE's project through the engine as one
/// commit under the owner's git identity; the IDE never merges by itself. Inputs: the
/// store, the owner's home, the project, the set, the head the owner reviewed and the
/// owner's message, None for the proposed one. Output: the commit, or the conflicting files.
pub(crate) fn accept(
    db: &Connection,
    owner_home: &Path,
    project: &ProjectId,
    id: &ChangeSetId,
    expected_head: &str,
    message: Option<&str>,
) -> Result<AcceptOutcome, EngineError> {
    landing::ide::locate(db, project, id)?;
    landing::accept(db, owner_home, id, expected_head, message)
}

/// Answers `ide.discard`: discards a change set of the IDE's project, restorable for the
/// engine's window. Inputs: the store, the project, the set, the head the owner reviewed
/// and now. Output: when it can no longer be restored.
pub(crate) fn discard(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    expected_head: &str,
    now_ms: u64,
) -> Result<u64, EngineError> {
    landing::ide::locate(db, project, id)?;
    landing::discard(db, id, expected_head, now_ms)
}

/// Answers `ide.restore`: brings a discarded change set of the IDE's project back as
/// pending. Inputs: the store, the project, the set and now. Output: Ok once pending.
pub(crate) fn restore(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    now_ms: u64,
) -> Result<(), EngineError> {
    landing::ide::locate(db, project, id)?;
    landing::restore(db, id, now_ms)
}

#[cfg(all(test, unix))]
mod tests;
