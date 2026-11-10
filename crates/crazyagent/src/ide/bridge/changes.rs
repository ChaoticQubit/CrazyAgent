use super::IdeCall;
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::landing::{
    self, AcceptOutcome, AgentKind, ChangeDiff, ChangeSetId, ChangeSetView, ChangeState,
    ChangedFile,
};
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde::Serialize;
use std::path::{Path, PathBuf};

const TITLE_CHARS_MAX: usize = 80;
const LIST_BYTES_MAX: usize = 48 * 1024;
const BASE_PAGE_BYTES: usize = 24 * 1024;

/// One change set as the IDE's Source Control shows it: one group named
/// "<agent>: <title>" whose files open VS Code's diff between the base text and the text
/// at the recorded head, which Accept lands. `head` is what Accept and Discard send back, so a set that moved on since the
/// list is refused as stale. `message` is the commit message Accept proposes. `problem`
/// is the plain code when the files could not be read or did not fit the reply; the group
/// then shows that instead. `waiting` marks an Updating set already merged and waiting
/// for its agent, which the owner can still discard.
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

/// One page of a file's base or head text, at most `BASE_PAGE_BYTES` once escaped for
/// JSON, so every page fits one engine frame. `next` is the following page while there is more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BasePage {
    pub(crate) text: String,
    pub(crate) next: Option<u16>,
}

/// Answers one Changes call of the IDE's project, its store and git work on a blocking
/// thread as the desktop's Changes requests run. Inputs: the engine root holding
/// `engine.db`, the project and the call. Output: the reply data, or the refusal.
pub(crate) async fn answer(
    root: PathBuf,
    project: ProjectId,
    call: IdeCall,
) -> Result<serde_json::Value, EngineError> {
    tokio::task::spawn_blocking(move || {
        let db = landing::open(&root)?;
        match call {
            IdeCall::ChangeSets {} => json(change_sets(&db, &project, now_ms()?)?),
            IdeCall::ChangeDiff { id, head, page } => {
                json(change_diff(&db, &project, &id, &head, page)?)
            }
            IdeCall::BaseBlob {
                id,
                head,
                path,
                page,
            } => json(base_blob(&db, &project, &id, &head, &path, page)?),
            IdeCall::ConfigBlob {
                id,
                head,
                path,
                page,
            } => json(config_blob(&db, &project, &id, &head, &path, page)?),
            IdeCall::HeadBlob {
                id,
                head,
                path,
                page,
            } => json(head_blob(&db, &project, &id, &head, &path, page)?),
            IdeCall::Accept { id, head, message } => {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .ok_or_else(|| failure("changes_owner_identity_missing"))?;
                json(accept(&db, &home, &project, &id, &head, &message)?)
            }
            IdeCall::Discard { id, head } => json(discard(&db, &project, &id, &head, now_ms()?)?),
            IdeCall::Restore { id } => json(restore(&db, &project, &id, now_ms()?)?),
            _ => Err(failure("ide_request_invalid")),
        }
    })
    .await
    .map_err(|_| failure("changes_unavailable"))?
}

/// A reply value as JSON. Input: the value. Output: the JSON, or `changes_outcome_unknown`.
fn json(value: impl Serialize) -> Result<serde_json::Value, EngineError> {
    serde_json::to_value(value).map_err(|_| failure("changes_outcome_unknown"))
}

/// Milliseconds since the Unix epoch. Output: now, or `changes_unavailable`.
fn now_ms() -> Result<u64, EngineError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_millis()).ok())
        .ok_or_else(|| failure("changes_unavailable"))
}

/// Whether a head is a full commit id as the desktop sends one: 40 or 64 lowercase
/// hexadecimal characters. Input: the head. Output: whether it is one.
fn commit_id(head: &str) -> bool {
    matches!(head.len(), 40 | 64)
        && head
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The agent's name and role without the harness the change set's label adds after a
/// middle dot. Input: the label. Output: the name, such as "Ada (@builder)".
pub(super) fn agent_name(label: &str) -> String {
    label
        .split(" \u{b7} ")
        .next()
        .unwrap_or(label)
        .trim()
        .to_owned()
}

/// The first line of the proposed commit message, at most `TITLE_CHARS_MAX` characters.
/// Input: the message. Output: the title.
pub(super) fn title(message: &str) -> String {
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
/// each with every file it touches, within one frame's budget. A set whose files cannot be
/// read still shows, with its problem code. Inputs: the store, the IDE's project and now.
/// Output: the sets.
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
        let worktree = landing::ide::locate(db, project, &view.id)?;
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
            worktree,
            files,
            restorable_until: view.restorable_until_ms,
            problem,
            id: view.id,
        });
    }
    fit(sets)
}

/// Keeps a list within `LIST_BYTES_MAX` once encoded, in the engine's order: a set that
/// would pass it is kept without its files and with its title as its message, marked
/// `changes_too_large`, and a set that still would not fit is left out while later ones
/// may still fit. Input: the sets. Output: the sets that fit.
fn fit(sets: Vec<IdeChangeSet>) -> Result<Vec<IdeChangeSet>, EngineError> {
    let size = |set: &IdeChangeSet| {
        serde_json::to_vec(set)
            .map(|bytes| bytes.len() + 1)
            .map_err(|_| failure("changes_outcome_unknown"))
    };
    let mut used = 2;
    let mut kept = Vec::new();
    for mut set in sets {
        let mut cost = size(&set)?;
        if used + cost > LIST_BYTES_MAX {
            set.files.clear();
            set.message = set.title.clone();
            set.problem = Some("changes_too_large".into());
            cost = size(&set)?;
            if used + cost > LIST_BYTES_MAX {
                continue;
            }
        }
        used += cost;
        kept.push(set);
    }
    assert!(used <= LIST_BYTES_MAX, "the list stays within its budget");
    Ok(kept)
}

/// Answers `ide.change_diff`: one page of a change set of the IDE's project at the head the
/// IDE listed, which Accept and Discard name too, so the owner never reviews a head other
/// than the one Accept lands. Inputs: the store, the project, the change set, the listed
/// head and the page. Output: the page, or `changes_stale` once the set holds another head
/// or for a head that is no commit id.
pub(crate) fn change_diff(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    head: &str,
    page: u16,
) -> Result<ChangeDiff, EngineError> {
    if !commit_id(head) {
        return Err(failure("changes_stale"));
    }
    landing::ide::locate(db, project, id)?;
    landing::diff(db, id, head, page)
}

/// Answers `ide.base_blob`: one page of the base text of one text file of a change set of
/// the IDE's project, the left side of its diff. Inputs: the store, the project, the
/// change set, the head the IDE listed, the path and the page. Output: the page, or
/// `changes_not_found` past the end, `changes_stale` once the set holds another head.
pub(crate) fn base_blob(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    head: &str,
    path: &str,
    page: u16,
) -> Result<BasePage, EngineError> {
    if !commit_id(head) {
        return Err(failure("changes_stale"));
    }
    let text = landing::ide::base_text(db, project, id, head, path)?;
    page_of(&text, page).ok_or_else(|| failure("changes_not_found"))
}

/// One page of an IDE settings file as the agent's change set has it, the right side of
/// its diff, served by the engine because the IDE server cannot read agent `.vscode` or
/// `*.code-workspace` files. Inputs: the store, the project, the set, the head the IDE
/// listed, the path and the page. Output: the page, or `changes_not_found` for any other
/// path or a page past the end, `changes_stale` once the set holds another head.
pub(crate) fn config_blob(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    head: &str,
    path: &str,
    page: u16,
) -> Result<BasePage, EngineError> {
    if !commit_id(head) {
        return Err(failure("changes_stale"));
    }
    let text = landing::ide::config_text(db, project, id, head, path)?;
    page_of(&text, page).ok_or_else(|| failure("changes_not_found"))
}

/// Answers `ide.head_blob`: one page of the text one text file of a change set of the
/// IDE's project has at the set's recorded head, the right side of its diff, so the owner
/// reviews what Accept lands rather than the agent worktree. Inputs: the store, the
/// project, the change set, the head the IDE listed, the path and the page. Output: the
/// page, or `changes_not_found` for a deleted file or past the end, `changes_stale` once
/// the set holds another head.
pub(crate) fn head_blob(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    head: &str,
    path: &str,
    page: u16,
) -> Result<BasePage, EngineError> {
    if !commit_id(head) {
        return Err(failure("changes_stale"));
    }
    let text = landing::ide::head_text(db, project, id, head, path)?;
    page_of(&text, page).ok_or_else(|| failure("changes_not_found"))
}

/// The bytes serde_json writes for one character inside a JSON string.
fn escaped_len(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        '\u{0}'..='\u{1f}' => 6,
        _ => character.len_utf8(),
    }
}

/// Where the page starting at `start` ends: the most whole characters whose escaped size
/// fits `BASE_PAGE_BYTES`. Inputs: the text and a character boundary. Output: the end.
fn page_end(text: &str, start: usize) -> usize {
    let mut used = 0;
    for (offset, character) in text[start..].char_indices() {
        used += escaped_len(character);
        if used > BASE_PAGE_BYTES {
            return start + offset;
        }
    }
    text.len()
}

/// One page of a base text; an empty text is one empty page. Inputs: the text and the
/// page. Output: the page, or None past the last one.
fn page_of(text: &str, page: u16) -> Option<BasePage> {
    let mut start = 0;
    let mut index: u16 = 0;
    loop {
        let end = page_end(text, start);
        assert!(
            end > start || text.is_empty(),
            "every page holds a character"
        );
        let more = end < text.len();
        if index == page {
            return Some(BasePage {
                text: text[start..end].to_owned(),
                next: if more { index.checked_add(1) } else { None },
            });
        }
        if !more {
            return None;
        }
        start = end;
        index = index.checked_add(1)?;
    }
}

/// Answers `ide.accept`: lands a change set of the IDE's project through the engine as one
/// commit under the owner's git identity; the IDE never merges by itself. It lands the
/// recorded head the diff shows, as the desktop's Accept does, so an agent already working
/// on its next turn does not hold it back. Inputs: the store, the owner's home, the
/// project, the set, the head the owner reviewed and the owner's message. Output: the
/// commit, or the conflicting files; a head that is no commit id is `changes_stale`.
pub(crate) fn accept(
    db: &Connection,
    owner_home: &Path,
    project: &ProjectId,
    id: &ChangeSetId,
    expected_head: &str,
    message: &str,
) -> Result<AcceptOutcome, EngineError> {
    if !commit_id(expected_head) {
        return Err(failure("changes_stale"));
    }
    landing::ide::locate(db, project, id)?;
    landing::accept(
        db,
        owner_home,
        id,
        expected_head,
        Some(message),
        crate::activity::AcceptPlace::Ide,
    )
}

/// Answers `ide.discard`: discards a change set of the IDE's project, restorable for the
/// engine's window. Inputs: the store, the project, the set, the head the owner reviewed
/// and now. Output: when it can no longer be restored; a head that is no commit id is
/// `changes_stale`.
pub(crate) fn discard(
    db: &Connection,
    project: &ProjectId,
    id: &ChangeSetId,
    expected_head: &str,
    now_ms: u64,
) -> Result<u64, EngineError> {
    if !commit_id(expected_head) {
        return Err(failure("changes_stale"));
    }
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
pub(super) mod tests;
