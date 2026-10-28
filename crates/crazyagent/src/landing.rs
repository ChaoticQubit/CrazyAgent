use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
mod clone;
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
/// worktree folder and its git metadata folder: under the checkout's `.git/worktrees`
/// for a linked worktree, `agent_git::git_dir` of the worktree for an agent's own clone.
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

/// Makes an agent's own clone of the owner repository in a new worktree folder for the
/// Own branch and Full access levels (DEC-102): its own git folder at
/// `agent_git::git_dir`, the project's objects borrowed read-only, its branch
/// `agent_git::branch` checked out at the owner's HEAD commit. The engine runs no git in
/// the clone after this but the fetch that reads its branch. Inputs: the owner's
/// checkout root and the new worktree folder. Output: Ok once checked out, else a plain
/// `changes_*` code.
pub fn add_agent_clone(repository: &Path, worktree: &Path) -> Result<(), EngineError> {
    #[cfg(unix)]
    return clone::add(repository, worktree);
    #[cfg(not(unix))]
    {
        let _ = (repository, worktree);
        Err(unavailable())
    }
}

/// Readies an agent's own clone for another launch: makes any of its pack and
/// loose-object folders that are missing, as `add_agent_clone` does for a new one, since
/// the Own branch sandbox keeps the agent from making them itself (RL-3). Input: the
/// clone's worktree. Output: Ok, else `changes_workspace_invalid`.
pub fn ready_agent_clone(worktree: &Path) -> Result<(), EngineError> {
    #[cfg(unix)]
    return clone::store_folders(worktree);
    #[cfg(not(unix))]
    {
        let _ = worktree;
        Err(unavailable())
    }
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

/// One file as committed in the owner's checkout: its bytes up to the reader's bound, its
/// size and whether the bound cut it.
#[derive(Debug)]
pub struct HeadBlob {
    pub bytes: Vec<u8>,
    pub size: u64,
    pub cut: bool,
}

/// What reading one committed file found: its blob, none when nothing is at the path, or
/// `instruction_file_unsafe` for a link, folder or submodule there.
pub type HeadEntry = Result<Option<HeadBlob>, EngineError>;

const HEAD_ENTRY_BYTES_MAX: usize = 4096;

/// Reads regular files as committed at the commit the owner's checkout's HEAD names, with
/// one engine git repository that runs nothing the repository configures, never from a
/// work tree. Inputs: the checkout root, the distinct paths inside the repository, each
/// without pathspec magic, and the most bytes to keep of each. Output: one entry per path
/// in order, every one none when HEAD names no commit, or a plain git code for the
/// repository.
pub fn head_files(
    checkout: &Path,
    paths: &[&str],
    limit: usize,
) -> Result<Vec<HeadEntry>, EngineError> {
    #[cfg(unix)]
    return committed_files(checkout, paths, limit);
    #[cfg(not(unix))]
    {
        let _ = (checkout, paths, limit);
        Err(unavailable())
    }
}

#[cfg(unix)]
fn committed_files(
    checkout: &Path,
    paths: &[&str],
    limit: usize,
) -> Result<Vec<HeadEntry>, EngineError> {
    assert!(paths.iter().enumerate().all(|(at, path)| {
        !path.is_empty() && !path.starts_with(':') && !paths[..at].contains(path)
    }));
    let git_failed = || git::failure("changes_git_failed");
    let owner = git::Repo::open(checkout, &checkout.join(".git"), None)?;
    let Some(head) = owner.commit_of("HEAD")? else {
        return Ok(paths.iter().map(|_| Ok(None)).collect());
    };
    let mut arguments = vec![
        "ls-tree",
        "-z",
        "--full-tree",
        "--format=%(objectmode) %(objecttype) %(objectname) %(objectsize)%x09%(path)",
        head.as_str(),
        "--",
    ];
    arguments.extend_from_slice(paths);
    let listing = owner.capture(&arguments, HEAD_ENTRY_BYTES_MAX * paths.len())?;
    let mut entries: Vec<(&str, &str, &str, &str, &str)> = Vec::new();
    for entry in listing.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let entry = std::str::from_utf8(entry).map_err(|_| git_failed())?;
        let (fields, named) = entry.split_once('\t').ok_or_else(git_failed)?;
        let fields: Vec<&str> = fields.split(' ').collect();
        let [mode, kind, object, size] = fields.as_slice() else {
            return Err(git_failed());
        };
        entries.push((named, mode, kind, object, size));
    }
    if !listing.is_empty() && !listing.ends_with(b"\0") {
        return Err(git_failed());
    }
    let read = |path: &str| -> HeadEntry {
        let Some(&(_, mode, kind, object, size)) =
            entries.iter().find(|(named, ..)| *named == path)
        else {
            return Ok(None);
        };
        if kind != "blob" || !matches!(mode, "100644" | "100755") {
            return Err(EngineError("instruction_file_unsafe".into()));
        }
        if !git::is_object_id(object) {
            return Err(git_failed());
        }
        let size: u64 = size.parse().map_err(|_| git_failed())?;
        let output = owner.exec(&["cat-file", "blob", object], None, &[], limit)?;
        if output.code != 0 {
            return Err(git_failed());
        }
        assert!(output.stdout.len() <= limit);
        Ok(Some(HeadBlob {
            bytes: output.stdout,
            size,
            cut: output.overflow,
        }))
    };
    let found: Vec<HeadEntry> = paths.iter().map(|path| read(path)).collect();
    assert_eq!(found.len(), paths.len());
    Ok(found)
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

/// Reads one change set by its id, however many change sets its project holds.
/// Inputs: the store and the id. Output: its project and view, or None when no change
/// set has that id.
pub fn change_set(
    db: &Connection,
    id: &ChangeSetId,
) -> Result<Option<(ProjectId, ChangeSetView)>, EngineError> {
    #[cfg(unix)]
    return Ok(store::find(db, id)?.map(|record| (record.project.clone(), record.view())));
    #[cfg(not(unix))]
    {
        let _ = (db, id);
        Err(unavailable())
    }
}

/// Reads the change set open in one agent worktree, the one its next turn end adds to,
/// however many change sets its project holds. Inputs: the store and the worktree.
/// Output: the view, or None when nothing is open there.
pub fn open_change_set(
    db: &Connection,
    worktree: &Path,
) -> Result<Option<ChangeSetView>, EngineError> {
    #[cfg(unix)]
    {
        let Some(worktree) = worktree.to_str() else {
            return Ok(None);
        };
        Ok(store::open_for(db, worktree)?
            .as_ref()
            .map(store::Record::view))
    }
    #[cfg(not(unix))]
    {
        let _ = (db, worktree);
        Err(unavailable())
    }
}

/// Reads one page of a change set's diff between its base and head, produced by the
/// engine with no external diff or text conversion, only while the set is still at the
/// head the owner is reading, so every page shown is of the head Accept and Discard name
/// (RV-DS-13, RV-DS-79). Inputs: the store, the change set, that head and the page.
/// Output: the page, or `changes_stale` once the set moved to another head.
pub fn diff(
    db: &Connection,
    id: &ChangeSetId,
    head: &str,
    page: u16,
) -> Result<ChangeDiff, EngineError> {
    #[cfg(unix)]
    {
        store::initialize(db)?;
        let record = store::load(db, id)?;
        if record.head != head {
            return Err(EngineError("changes_stale".into()));
        }
        diff::page(&record.repository, &record.base, &record.head, page)
    }
    #[cfg(not(unix))]
    {
        let _ = (db, id, head, page);
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
/// `cleanup_commits_unlanded`, `cleanup_refs_unlanded`, `cleanup_worktree_dirty` or
/// `cleanup_worktree_unreadable` when git cannot read this one worktree. An agent's own
/// clone is judged through the engine's fetch of its branch and its ref names read as
/// data, with no git run in it. Inputs: the store, the owner's checkout root, the
/// worktree, its git folder and the time now. Output: None when it can be removed.
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

/// Removes a finished agent's worktree with the host git, never forcing, or an agent's
/// own clone as a folder with no git, after checking `retire_blocker` again while no
/// landing step can run on its repository. Inputs: the store, the owner's checkout root,
/// the worktree, its git folder and the time now. Output: Ok once removed,
/// `cleanup_worktree_leftover` when git deleted its record of the worktree, or a clone's
/// folder was deleted, only part way, or the plain code that kept it.
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
