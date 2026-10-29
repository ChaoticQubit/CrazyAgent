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
#[cfg(unix)]
mod subagent;
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
/// that resolves it. `outside` is true when the engine closed it because its work was
/// landed outside the app (Accepted) or removed outside the app (Discarded, never
/// restorable). While open, `files`, `added` and `removed` count only what is not yet on
/// the owner's branch.
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
    #[serde(default)]
    pub outside: bool,
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

/// The local branches of a change set's repository that an Accept can land on, at most
/// `BRANCHES_MAX`, and the checked-out one, the default target; None when the owner's
/// checkout is on a detached HEAD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetBranches {
    pub branches: Vec<String>,
    pub current: Option<String>,
}

/// What merging a sub-agent's branch into its lead's task branch did (DEC-104 K5b):
/// merged as the commit named, already in the task branch, or conflicting on the listed
/// files, with the merge undone and the branch left in the lead's clone to merge by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MergeOutcome {
    Merged { commit: String, branch: String },
    AlreadyMerged { branch: String },
    Conflict { branch: String, files: Vec<String> },
}

/// The lead's own rights for the git steps of a sub-agent merge: the lead's sandbox and
/// the profile the engine installed for it, under which every step in its clone runs.
pub struct LeadGit<'a> {
    pub sandbox: &'a crate::sandbox::ProjectSandbox,
    pub profile: &'a Path,
}

/// The most local branches a target listing carries.
pub const BRANCHES_MAX: usize = 256;

/// The longest branch name an Accept target may have, in bytes.
pub const BRANCH_BYTES_MAX: usize = 255;

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
    add_agent_clone_at(repository, worktree, None)
}

/// Makes an agent's own clone as `add_agent_clone` does, at a lead's commit for a worker
/// cut from its lead (N1). Inputs: the owner checkout root, the new worktree and the
/// commit, None for the owner's HEAD. Output: Ok, else as `add_agent_clone`.
pub fn add_agent_clone_at(
    repository: &Path,
    worktree: &Path,
    base: Option<&str>,
) -> Result<(), EngineError> {
    let branch = crate::agent_git::branch(worktree)
        .ok_or_else(|| EngineError("changes_workspace_invalid".into()))?;
    add_agent_clone_on(repository, worktree, base, &branch)
}

/// Makes an agent's own clone as `add_agent_clone` does, on a named branch, such as a
/// harness's task branch from [`claim_task_branch`], at a commit the owner repository
/// holds. Inputs: the owner checkout root, the new worktree, the commit, None for the
/// owner's HEAD, and the branch. Output: Ok, else as `add_agent_clone`.
pub fn add_agent_clone_on(
    repository: &Path,
    worktree: &Path,
    base: Option<&str>,
    branch: &str,
) -> Result<(), EngineError> {
    #[cfg(unix)]
    return clone::add(repository, worktree, base, branch);
    #[cfg(not(unix))]
    {
        let _ = (repository, worktree, base, branch);
        Err(unavailable())
    }
}

/// Names and stores the task branch a new harness works on (DEC-100), `crazyagent/<name>`
/// or its first free `-2` to `-64` form, before its worktree is made: its own clone is then
/// made on it ([`add_agent_clone_on`]) or its linked worktree put on it
/// ([`name_agent_branch`]). Inputs: the store, the owner checkout root, the new worktree
/// and the name, lower-case letters and digits joined by single hyphens. Output: the
/// branch, or `changes_agent_branch_invalid` or `changes_agent_branch_exists`.
pub fn claim_task_branch(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    name: &str,
) -> Result<String, EngineError> {
    #[cfg(unix)]
    return flow::claim_branch(db, repository, worktree, name);
    #[cfg(not(unix))]
    {
        let _ = (db, repository, worktree, name);
        Err(unavailable())
    }
}

/// The branch an agent worktree commits on: its stored task branch, else the branch its
/// folder names for its layout, read from the store and paths with no git run. Inputs: the
/// store, the worktree and its git metadata folder. Output: the branch, None for a folder
/// that names none.
pub fn agent_branch(
    db: &Connection,
    worktree: &Path,
    git_dir: &Path,
) -> Result<Option<String>, EngineError> {
    let layout = if git_dir == crate::agent_git::git_dir(worktree) {
        crate::agent_git::Layout::Clone
    } else {
        crate::agent_git::Layout::Linked
    };
    #[cfg(unix)]
    return flow::agent_branch(db, worktree, layout);
    #[cfg(not(unix))]
    {
        let _ = (db, worktree, layout);
        Err(unavailable())
    }
}

/// The branch the master works on and merges finished harnesses into (DEC-100), with its
/// tip: the owner's "Master's branch" choice, else the repository's default branch (the
/// local branch `origin`'s HEAD names), else the owner's checked-out branch. Inputs: the
/// owner checkout root and the owner's choice. Output: the branch and tip, None when no
/// such branch exists and nothing was chosen, or `changes_target_invalid` for a choice
/// that is no local branch.
pub fn master_branch(
    repository: &Path,
    chosen: Option<&str>,
) -> Result<Option<(String, String)>, EngineError> {
    #[cfg(unix)]
    return owner::master_branch(repository, chosen);
    #[cfg(not(unix))]
    {
        let _ = (repository, chosen);
        Err(unavailable())
    }
}

/// The commit a lead's worktree holds now, which its new worker's worktree is cut from
/// (N1). Inputs: the store, the owner's checkout root, the lead's worktree and its git
/// metadata folder. Output: the commit id.
pub fn agent_tip(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<String, EngineError> {
    #[cfg(unix)]
    return flow::tip(db, repository, worktree, git_dir);
    #[cfg(not(unix))]
    {
        let _ = (db, repository, worktree, git_dir);
        Err(unavailable())
    }
}

/// Lists the branches of a lead's sub-agents (DEC-104 K5b) whose work is not yet in the
/// lead's task branch, so a lead cannot report its work complete while a sub-agent's work
/// waits. Read through the engine's checked fetch; no git runs in any agent's clone.
/// Inputs: the engine database, the project and the lead's team agent id. Output: the
/// unmerged branches, empty when every sub-agent's work is in, or a plain `changes_*` code.
pub fn unmerged_sub_agents(
    db: &Connection,
    project: &ProjectId,
    lead: i64,
) -> Result<Vec<String>, EngineError> {
    #[cfg(unix)]
    return subagent::unmerged(db, project, lead);
    #[cfg(not(unix))]
    {
        let _ = (db, project, lead);
        Err(unavailable())
    }
}

/// Names and stores the branch a new sub-agent of a lead works on (DEC-104 K5b),
/// `<task branch>--<name>` or its first free `-2` to `-64` form, before its worktree is
/// made. Inputs: the store, the owner checkout root, the new worktree, the lead's task
/// branch, the lead's own clone (None for a linked worktree) and the name. Output: the
/// branch, None when the lead's branch cannot carry sub-agent branches, or a plain code.
pub fn claim_sub_agent_branch(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    task_branch: &str,
    lead_clone: Option<&Path>,
    name: &str,
) -> Result<Option<String>, EngineError> {
    #[cfg(unix)]
    return subagent::claim(db, repository, worktree, task_branch, lead_clone, name);
    #[cfg(not(unix))]
    {
        let _ = (db, repository, worktree, task_branch, lead_clone, name);
        Err(unavailable())
    }
}

/// Merges a sub-agent's branch into its lead's task branch in the lead's own clone
/// (DEC-104 K5b): the engine's checked fetch reads the sub-agent's work, then git runs in
/// the lead's clone only with the lead's rights. Inputs: the engine database, the project,
/// the lead, the sub-agent and the lead's git. Output: what the merge did, or a plain
/// `changes_*` refusal.
pub fn merge_sub_agent(
    db: &Connection,
    project: &ProjectId,
    lead: i64,
    sub: i64,
    git: &LeadGit<'_>,
) -> Result<MergeOutcome, EngineError> {
    #[cfg(unix)]
    return subagent::merge(db, project, lead, sub, git);
    #[cfg(not(unix))]
    {
        let _ = (db, project, lead, sub, git);
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

/// Puts a newly added linked agent worktree on its own branch, its stored task branch or
/// else `crazyagent/agent-<id>`, created at the worktree's HEAD and checked out there, so
/// the agent's turns commit on it. Call it once, right after the worktree is added and before any agent runs in it.
/// Inputs: the store, the owner's checkout root, the worktree and its git metadata
/// folder. Output: the branch, or `changes_agent_branch_exists` when a branch of that
/// name is already there.
pub fn name_agent_branch(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<String, EngineError> {
    #[cfg(unix)]
    return flow::name_branch(db, repository, worktree, git_dir);
    #[cfg(not(unix))]
    {
        let _ = (db, repository, worktree, git_dir);
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

const TRACKED_LISTING_BYTES_MAX: usize = 4096;

/// Tells which of some paths the owner's checkout's git index tracks, with one engine git
/// repository that runs nothing the repository configures. Inputs: the checkout root and
/// the distinct paths inside the repository, each without pathspec magic. Output: one flag
/// per path in order, or a plain git code for the repository.
pub fn tracked_files(checkout: &Path, paths: &[&str]) -> Result<Vec<bool>, EngineError> {
    #[cfg(unix)]
    return indexed_files(checkout, paths);
    #[cfg(not(unix))]
    {
        let _ = (checkout, paths);
        Err(unavailable())
    }
}

#[cfg(unix)]
fn indexed_files(checkout: &Path, paths: &[&str]) -> Result<Vec<bool>, EngineError> {
    assert!(paths.iter().enumerate().all(|(at, path)| {
        !path.is_empty() && !path.starts_with(':') && !paths[..at].contains(path)
    }));
    let owner = git::Repo::open(checkout, &checkout.join(".git"), Some(checkout))?;
    let mut arguments = vec!["ls-files", "-z", "--cached", "--full-name", "--"];
    arguments.extend_from_slice(paths);
    let listing = owner.capture(&arguments, TRACKED_LISTING_BYTES_MAX)?;
    let listed: Vec<&[u8]> = listing
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .collect();
    let tracked: Vec<bool> = paths
        .iter()
        .map(|path| listed.contains(&path.as_bytes()))
        .collect();
    assert_eq!(tracked.len(), paths.len());
    Ok(tracked)
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

/// Lists one project's change sets as the owner's Changes and the master's brief show
/// them: as [`list`], without the sets of a lead's workers while the owner's Lands setting
/// is Lead, since that lead merges them (F3). Inputs: the store and the project. Output:
/// the change sets.
pub fn owner_list(db: &Connection, project: &ProjectId) -> Result<Vec<ChangeSetView>, EngineError> {
    let sets = list(db, project)?;
    assert!(sets.len() <= LIST_MAX);
    #[cfg(unix)]
    if subagent::leads_merge(db, project)? {
        let mut kept = Vec::with_capacity(sets.len());
        for set in sets {
            if !subagent::is_lead_worker(db, project, &set.agent)? {
                kept.push(set);
            }
        }
        return Ok(kept);
    }
    Ok(sets)
}

/// Closes the open change sets of a lead's workers whose work the lead's own clone
/// already holds, also after a merge the lead ran by hand (F3), as the lead's merge
/// closes them. Inputs: the store, the project and the lead's team agent id. Output: Ok,
/// or a plain `changes_*` code for a lead whose tip could not be read.
pub fn settle_sub_agents(
    db: &Connection,
    project: &ProjectId,
    lead: i64,
) -> Result<(), EngineError> {
    #[cfg(unix)]
    return subagent::settle(db, project, lead);
    #[cfg(not(unix))]
    {
        let _ = (db, project, lead);
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

/// Reads one page of a change set's diff, produced by the engine with no external diff or
/// text conversion: for an open set, first noticed against the owner's branch, only what
/// is not yet on that branch when the owner landed part of it outside the app, else the
/// whole change between its base and head. Inputs: the store, the change set and the page.
/// Output: the page.
pub fn diff(db: &Connection, id: &ChangeSetId, page: u16) -> Result<ChangeDiff, EngineError> {
    #[cfg(unix)]
    {
        store::initialize(db)?;
        let record = store::load(db, id)?;
        let remainder = owner::observe_for_diff(db, id).unwrap_or_else(|error| {
            eprintln!(
                "landing_observe_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
            None
        });
        match remainder {
            Some((tip, tree)) => diff::page(&record.repository, &tip, &tree, page),
            None => diff::page(&record.repository, &record.base, &record.head, page),
        }
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

/// Lands a change set as one squash commit on a chosen local branch: the owner's
/// checked-out branch through the owner's checkout as [`accept`] does, a branch checked
/// out nowhere by moving its ref only, leaving the owner's files untouched, or an agent's
/// own branch through that agent's worktree, which must hold nothing uncommitted. Inputs:
/// the store, the owner's home folder, the change set, the head the owner reviewed, the
/// edited message, None for the proposed one, and the target branch. Output: the landed
/// commit and branch, or the conflicting files.
pub fn accept_into(
    db: &Connection,
    owner_home: &Path,
    id: &ChangeSetId,
    expected_head: &str,
    message: Option<&str>,
    target_branch: &str,
) -> Result<AcceptOutcome, EngineError> {
    #[cfg(unix)]
    return owner::accept_to(
        db,
        owner_home,
        id,
        expected_head,
        message,
        Some(target_branch),
        &|_| false,
    );
    #[cfg(not(unix))]
    {
        let _ = (db, owner_home, id, expected_head, message, target_branch);
        Err(unavailable())
    }
}

/// Lists the local branches an Accept of a change set can land on and the owner's
/// checked-out branch. Inputs: the store and the change set. Output: the branches.
pub fn target_branches(db: &Connection, id: &ChangeSetId) -> Result<TargetBranches, EngineError> {
    #[cfg(unix)]
    return owner::branches(db, id);
    #[cfg(not(unix))]
    {
        let _ = (db, id);
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

/// Notices what the owner did outside the app to each open change set, one project's or
/// every project's: closes one whose work the owner's checked-out branch already holds as
/// landed outside the app, closes one whose agent copy, branch or commit is gone as
/// removed outside the app, and counts only what is not yet on the owner's branch for the
/// rest. A change set that cannot be judged now is logged by its plain code and left as it
/// was, so it never stops the others. Inputs: the store and the project, None for every
/// project. Output: Ok once each open change set was judged or left.
pub fn observe(db: &Connection, project: Option<&ProjectId>) -> Result<(), EngineError> {
    #[cfg(unix)]
    return owner::observe(db, project);
    #[cfg(not(unix))]
    {
        let _ = (db, project);
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

/// Names a harness's teammate branches and teammate worktrees whose work is not in its
/// task branch yet (DEC-104), read through the engine's checked fetch of its clone, so
/// no git runs in it. Inputs: the store, the owner's checkout root, the worktree and its
/// git folder. Output: the names, none for a linked worktree or a clone without
/// teammates, or the plain code that kept the clone from being read.
pub fn unlanded_teammates(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<Result<Vec<String>, &'static str>, EngineError> {
    #[cfg(unix)]
    return retire::unlanded_teammates(db, repository, worktree, git_dir);
    #[cfg(not(unix))]
    {
        let _ = (db, repository, worktree, git_dir);
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
