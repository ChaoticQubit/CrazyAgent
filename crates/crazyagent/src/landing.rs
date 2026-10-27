use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
mod diff;
#[cfg(unix)]
mod flow;
#[cfg(unix)]
mod git;
#[cfg(unix)]
mod owner;
#[cfg(unix)]
mod retire;
#[cfg(unix)]
mod store;
#[cfg(all(test, unix))]
mod tests;

/// How long a discarded change set stays restorable before its ref is deleted.
pub const RESTORE_WINDOW_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// The most change sets one list reply carries, newest first.
pub const LIST_MAX: usize = 32;
/// The most bytes an owner-edited Accept message may hold.
pub const MESSAGE_BYTES_MAX: usize = 4096;

/// Who made a change set: the master of a project conversation or one team sub-agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Master,
    Sub,
}

/// The agent a change set belongs to. Fields: its kind, its id (the master session id or
/// the team agent id) and the label the owner knows it by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeAgent {
    pub kind: AgentKind,
    pub id: String,
    pub label: String,
}

/// How the turn or task that ended went. A failed or cancelled turn still commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

/// Where a change set stands. Pending waits for the owner; Conflict was refused by an
/// Accept that conflicts; Updating holds the owner's branch merged in with conflict
/// markers until the agent's next turn ends; Accepted and Discarded are final, except
/// that a Discarded set is restorable for `RESTORE_WINDOW_MS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeState {
    Pending,
    Conflict,
    Updating,
    Accepted,
    Discarded,
}

/// A change set id: 32 lowercase hexadecimal characters the engine chose.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ChangeSetId(String);

impl ChangeSetId {
    /// Checks a change set id. Input: the text. Output: the id, or `changes_not_found`
    /// for anything but 32 lowercase hexadecimal characters.
    pub fn new(value: impl Into<String>) -> Result<Self, EngineError> {
        let value = value.into();
        if value.len() == 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Ok(Self(value));
        }
        Err(EngineError("changes_not_found".into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ChangeSetId {
    type Error = EngineError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ChangeSetId> for String {
    fn from(id: ChangeSetId) -> Self {
        id.0
    }
}

/// The agent worktree a turn or task ended in, as the engine computed it. Fields: the
/// project, the agent, the owner's checkout root the worktree was added from, the
/// worktree folder and its git metadata folder under the checkout's `.git/worktrees`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub project: ProjectId,
    pub agent: ChangeAgent,
    pub repository: PathBuf,
    pub worktree: PathBuf,
    pub git_dir: PathBuf,
}

/// The end of one turn or task. Fields: how it went, its turn or task id and the first
/// line of the prompt that started it; the engine bounds and cleans that line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEnd {
    pub outcome: TurnOutcome,
    pub turn: String,
    pub prompt_line: String,
}

/// One change set as the owner's Changes view shows it. `checked` is false in V1: no
/// CrazyAgent check ran. `conflicts` names the files the last Accept conflicted on.
/// `message` is the commit message an Accept proposes for the owner to edit. `waiting` is
/// true while an updating change set is merged but its agent was not yet handed the work
/// that resolves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSetView {
    pub id: ChangeSetId,
    pub agent: ChangeAgent,
    pub state: ChangeState,
    pub base: String,
    pub head: String,
    pub files: u32,
    pub added: u32,
    pub removed: u32,
    pub last_outcome: TurnOutcome,
    pub checked: bool,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub restorable_until_ms: Option<u64>,
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub waiting: bool,
}

/// How a file changed between a change set's base and head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

/// What a changed path holds on its new side, or its old side when deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Text,
    Binary,
    Symlink,
    Gitlink,
}

/// A property of a changed path the owner should look at before accepting: an
/// executable bit, a symlink, a `.gitattributes` or `.gitmodules` edit, or a mode change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMark {
    Executable,
    Symlink,
    Attributes,
    Modules,
    Mode,
}

/// One changed path. `old_path` is set for a rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub kind: FileKind,
    pub marks: Vec<FileMark>,
}

/// One page of a change set's diff: some of its files and some of its patch text, plain
/// text with control characters replaced, never to be rendered as HTML or ANSI. `next`
/// is the following page while either has more; `truncated` says the whole diff passed
/// the engine's bound and the rest is not shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeDiff {
    pub files: Vec<ChangedFile>,
    pub text: String,
    pub next: Option<u16>,
    pub truncated: bool,
}

/// What an Accept did: landed one squash commit on the owner's checked-out branch, or
/// conflicted on the listed files and changed nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AcceptOutcome {
    Landed { commit: String, branch: String },
    Conflict { files: Vec<String> },
}

fn unavailable() -> EngineError {
    EngineError("changes_unavailable".into())
}

/// Opens the change set store in the engine database, adding its table when missing.
/// Input: the engine root holding `engine.db`. Output: the connection.
pub fn open(runtime: &Path) -> Result<Connection, EngineError> {
    if !runtime.is_absolute() {
        return Err(unavailable());
    }
    let db = Connection::open(runtime.join("engine.db")).map_err(|_| unavailable())?;
    db.busy_timeout(Duration::from_secs(5))
        .map_err(|_| unavailable())?;
    #[cfg(unix)]
    store::initialize(&db)?;
    Ok(db)
}

/// Records the commit a newly added agent worktree starts from, so the change sets it
/// opens later hold every commit the agent makes above it. Call it once, right after the
/// worktree is added and before any agent runs in it. Inputs: the store, the owner's
/// checkout root, the worktree and its git metadata folder. Output: Ok once stored.
pub fn record_worktree_start(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<(), EngineError> {
    #[cfg(unix)]
    return flow::start(db, repository, worktree, git_dir);
    #[cfg(not(unix))]
    {
        let _ = (db, repository, worktree, git_dir);
        Err(unavailable())
    }
}

/// Commits every change in an agent worktree as the engine, with hooks, filters and
/// fsmonitor off, and adds it to that worktree's open change set or opens one above the
/// commit the worktree started from, or its HEAD when no start is recorded, so commits
/// the agent made itself land too. A failed or cancelled turn commits too. Inputs: the
/// store, the workspace and the turn end. Output: the change set the work now belongs
/// to, None when nothing is pending.
pub fn record_turn_end(
    db: &Connection,
    workspace: &Workspace,
    turn: &TurnEnd,
) -> Result<Option<ChangeSetId>, EngineError> {
    #[cfg(unix)]
    return flow::record(
        db,
        workspace,
        turn,
        &crate::worktrees::TREE_BOUNDS,
        flow::now_ms()?,
    );
    #[cfg(not(unix))]
    {
        let _ = (db, workspace, turn);
        Err(unavailable())
    }
}

/// The plain prompt for the task that resolves the conflict markers an update left.
/// Inputs: the store and the change set. Output: the prompt, or `changes_stale` when
/// the change set is not waiting for its resolve task.
pub fn resolve_task_prompt(db: &Connection, id: &ChangeSetId) -> Result<String, EngineError> {
    #[cfg(unix)]
    return flow::resolve_prompt(db, id);
    #[cfg(not(unix))]
    {
        let _ = (db, id);
        Err(unavailable())
    }
}

/// Lists one project's change sets, open ones first, then newest first, at most
/// `LIST_MAX`. Inputs: the store and the project. Output: the change sets.
pub fn list(db: &Connection, project: &ProjectId) -> Result<Vec<ChangeSetView>, EngineError> {
    #[cfg(unix)]
    return Ok(store::of_project(db, project, LIST_MAX)?
        .iter()
        .map(store::Record::view)
        .collect());
    #[cfg(not(unix))]
    {
        let _ = (db, project);
        Err(unavailable())
    }
}

/// Reads one page of a change set's diff between its base and head, produced by the
/// engine with no external diff or text conversion. Inputs: the store, the change set
/// and the page. Output: the page.
pub fn diff(db: &Connection, id: &ChangeSetId, page: u16) -> Result<ChangeDiff, EngineError> {
    #[cfg(unix)]
    {
        store::initialize(db)?;
        let record = store::load(db, id)?;
        diff::page(&record.repository, &record.base, &record.head, page)
    }
    #[cfg(not(unix))]
    {
        let _ = (db, id, page);
        Err(unavailable())
    }
}

/// Lands a change set as one squash commit on the owner's checked-out branch, authored
/// with the owner's git identity. Inputs: the store, the owner's home folder (where git
/// finds the owner's own user.name and user.email), the change set, the head the owner
/// reviewed and the owner's edited message, None for the proposed one. Output: the
/// landed commit and branch, or the conflicting files.
pub fn accept(
    db: &Connection,
    owner_home: &Path,
    id: &ChangeSetId,
    expected_head: &str,
    message: Option<&str>,
) -> Result<AcceptOutcome, EngineError> {
    #[cfg(unix)]
    return owner::accept(db, owner_home, id, expected_head, message, &|_| false);
    #[cfg(not(unix))]
    {
        let _ = (db, owner_home, id, expected_head, message);
        Err(unavailable())
    }
}

/// Reads the owner's git identity as the owner's own git sees it for a repository: the
/// repository's config, then the owner's global config in their home, never the system
/// config. Inputs: the repository root and the owner's home. Output: the name and email,
/// or `changes_owner_identity_missing`.
pub fn owner_git_identity(
    repository: &Path,
    owner_home: &Path,
) -> Result<(String, String), EngineError> {
    #[cfg(unix)]
    return owner::owner_identity(repository, owner_home);
    #[cfg(not(unix))]
    {
        let _ = (repository, owner_home);
        Err(unavailable())
    }
}

/// Discards a change set, keeping its commits restorable for `RESTORE_WINDOW_MS`, and
/// points the agent's worktree at the owner's current commit. Inputs: the store, the
/// change set, the head the owner reviewed and the time now. Output: the
/// restorable-until time.
pub fn discard(
    db: &Connection,
    id: &ChangeSetId,
    expected_head: &str,
    now_ms: u64,
) -> Result<u64, EngineError> {
    #[cfg(unix)]
    return flow::discard(db, id, expected_head, now_ms);
    #[cfg(not(unix))]
    {
        let _ = (db, id, expected_head, now_ms);
        Err(unavailable())
    }
}

/// Brings a discarded change set back as pending, with its work back in the agent's
/// worktree. Inputs: the store, the change set and the time now. Output: Ok once
/// pending.
pub fn restore(db: &Connection, id: &ChangeSetId, now_ms: u64) -> Result<(), EngineError> {
    #[cfg(unix)]
    return flow::restore(db, id, now_ms);
    #[cfg(not(unix))]
    {
        let _ = (db, id, now_ms);
        Err(unavailable())
    }
}

/// Merges the owner's branch tip into the agent's worktree with conflict markers, ready
/// for the resolve task. Inputs: the store and the change set. Output: Ok once merged.
pub fn update(db: &Connection, id: &ChangeSetId) -> Result<(), EngineError> {
    #[cfg(unix)]
    return owner::update(db, id);
    #[cfg(not(unix))]
    {
        let _ = (db, id);
        Err(unavailable())
    }
}

/// Notes that an updating change set's agent was handed the work that resolves it; until
/// then the set shows as merged and waiting for its agent. Inputs: the store and the
/// change set. Output: Ok, also when the set is no longer updating.
pub fn resolve_handed_over(db: &Connection, id: &ChangeSetId) -> Result<(), EngineError> {
    #[cfg(unix)]
    return flow::resolve_handed_over(db, id);
    #[cfg(not(unix))]
    {
        let _ = (db, id);
        Err(unavailable())
    }
}

/// Finishes or undoes an Accept or worktree reset an engine stop interrupted.
/// Input: the store. Output: Ok once nothing is left in flight.
pub fn reconcile(db: &Connection) -> Result<(), EngineError> {
    #[cfg(unix)]
    return owner::reconcile(db);
    #[cfg(not(unix))]
    {
        let _ = db;
        Err(unavailable())
    }
}

/// Says why a finished agent's worktree cannot be removed now, as a plain code:
/// `cleanup_changes_open`, `cleanup_changes_restorable`, `cleanup_worktree_invalid`,
/// `cleanup_commits_unlanded` or `cleanup_worktree_dirty`. Inputs: the store, the owner's
/// checkout root, the worktree, its git folder and the time now. Output: None when it can
/// be removed.
pub fn retire_blocker(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
    now_ms: u64,
) -> Result<Option<&'static str>, EngineError> {
    #[cfg(unix)]
    return retire::blocker(db, repository, worktree, git_dir, now_ms);
    #[cfg(not(unix))]
    {
        let _ = (db, repository, worktree, git_dir, now_ms);
        Err(unavailable())
    }
}

/// Removes a finished agent's worktree with the host git, never forcing, after checking
/// `retire_blocker` again while no landing step can run on its repository. Inputs: the
/// store, the owner's checkout root, the worktree, its git folder and the time now.
/// Output: Ok once removed, or the plain code that kept it.
pub fn retire_worktree(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
    now_ms: u64,
) -> Result<Result<(), &'static str>, EngineError> {
    #[cfg(unix)]
    return retire::remove(db, repository, worktree, git_dir, now_ms);
    #[cfg(not(unix))]
    {
        let _ = (db, repository, worktree, git_dir, now_ms);
        Err(unavailable())
    }
}

/// Deletes the refs of discarded change sets whose restore window ended.
/// Inputs: the store and the time now. Output: how many were deleted.
pub fn prune(db: &Connection, now_ms: u64) -> Result<usize, EngineError> {
    #[cfg(unix)]
    return flow::prune(db, now_ms);
    #[cfg(not(unix))]
    {
        let _ = (db, now_ms);
        Err(unavailable())
    }
}
