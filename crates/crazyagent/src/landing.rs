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
pub(crate) mod ide;
#[cfg(unix)]
mod lockfiles;
#[cfg(unix)]
mod merges;
#[cfg(unix)]
mod owner;
pub mod owner_edit;
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

/// The plain code of a change set refused because one new top-level folder gains more
/// files than the owner's limit (DEC-107 rule 3).
pub const NEW_FOLDER_REFUSED: &str = "changes_new_folder_too_large";

/// The most change sets one list reply carries, newest first.
pub const LIST_MAX: usize = 32;
/// The most bytes of conflicting file names one open set's line carries.
const OPEN_SET_FILES_MAX: usize = 400;
/// The most bytes an owner-edited Accept message may hold.
pub const MESSAGE_BYTES_MAX: usize = 4096;

/// The folders a path an agent wrote is cut at when it is shown: any absolute local path
/// under one of them, anywhere in a word, shows only its last component.
const LOCAL_ROOTS: [&str; 10] = [
    "~/",
    "/Users/",
    "/home/",
    "/root/",
    "/private/",
    "/tmp/",
    "/var/",
    "/Volumes/",
    "/mnt/",
    "/opt/",
];

/// Replaces every control character but newline and tab, and every bidirectional
/// override or isolate, with U+FFFD, then shows every other character a reviewer cannot
/// see, such as zero-width, tag and line separator characters, as its `memory::visible_text`
/// escape, so text shown to the owner cannot move a terminal cursor, reorder what it shows
/// or hide text from it. Input: the text. Output: the plain text.
pub(crate) fn plain(text: &str) -> String {
    let replaced: String = text
        .chars()
        .map(|c| {
            let bidi = matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}');
            if (c.is_control() && c != '\n' && c != '\t') || bidi {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect();
    crate::memory::visible_text(&replaced)
}

/// Free text as the owner is shown it on one line: plain through `plain`, paths under
/// `roots` made relative to them and any other absolute local path cut to its last
/// component, the words joined by single spaces. Inputs: the text and the folders,
/// innermost first. Output: the line, maybe empty.
pub(crate) fn scrubbed(text: &str, roots: &[&Path]) -> String {
    let mut line = plain(text);
    for root in roots.iter().filter_map(|root| root.to_str()) {
        if root.len() > 1 {
            line = line.replace(&format!("{}/", root.trim_end_matches('/')), "");
        }
    }
    let words: Vec<String> = line
        .split(|c: char| c.is_whitespace() || c == '\u{FFFD}')
        .map(local_path)
        .filter(|word| !word.is_empty())
        .collect();
    words.join(" ")
}

/// One word with a local path in it cut to the path's last component, from where the path
/// starts (see `local_start`) to the word's closing punctuation, the text before and after
/// it kept. Input: the word. Output: the word.
fn local_path(word: &str) -> String {
    let Some(start) = local_start(word) else {
        return word.to_owned();
    };
    let (lead, rest) = word.split_at(start);
    let path = rest.trim_end_matches([')', '"', '\'', '>', ']', '`', ',', '.', ';', ':', '!', '?']);
    let tail = &rest[path.len()..];
    let last = path
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or_default();
    assert!(lead.len() + last.len() + tail.len() <= word.len());
    format!("{lead}{last}{tail}")
}

/// Where the first local path in a word starts: a `LOCAL_ROOTS` folder, a `file://` link
/// in any case or a drive letter with `:\` or `:/`, at the word's start or after a mark
/// such as `=`, `:` or `(`, never after a letter, digit, slash, dot, dash or underscore, so
/// a URL's own folders, a URL scheme and a relative path are not taken. Input: the word.
/// Output: the byte offset, or None.
fn local_start(word: &str) -> Option<usize> {
    word.char_indices().map(|(at, _)| at).find(|&at| {
        let opens = word[..at].chars().next_back().is_none_or(|before| {
            !before.is_alphanumeric() && !matches!(before, '/' | '\\' | '.' | '-' | '_')
        });
        let rest = &word[at..];
        opens
            && (LOCAL_ROOTS.iter().any(|root| rest.starts_with(root))
                || rest
                    .get(..7)
                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"))
                || matches!(rest.as_bytes(), [letter, b':', b'\\' | b'/', ..] if letter.is_ascii_alphabetic()))
    })
}

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
/// the owner's branch. `number` is the set's number in its project, from 1 in the order the
/// project's sets were added, None for a set an engine without numbers stored until the
/// next engine start numbers it.
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
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub regenerated: Vec<String>,
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
/// executable bit, a symlink, a `.gitattributes` or `.gitmodules` edit, a mode change, or
/// a file the IDE reads as its settings, under `.vscode` or a `*.code-workspace` file,
/// an edit the owner made in the agent's worktree while the agent ran, or an instruction
/// file such as `AGENTS.md`, which changes what every agent is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMark {
    Executable,
    Symlink,
    Attributes,
    Modules,
    Mode,
    OwnerEdited,
    IdeConfig,
    Instructions,
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

/// The change sets waiting for the owner: `all` counts every project the engine has, and
/// `here` counts the one project asked about, None when the count was not for a project.
/// `here` is never more than `all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitingChanges {
    pub all: u32,
    pub here: Option<u32>,
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
/// `BRANCHES_MAX`, the checked-out one, None when the owner's checkout is on a detached
/// HEAD, and the Land on branch an Accept with no branch named lands on (DEC-111 Q1),
/// None when it cannot be resolved. `land_on_problem` is the plain sentence naming the
/// Land on setting and its missing branch when it cannot, so a client offers no default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetBranches {
    pub branches: Vec<String>,
    pub current: Option<String>,
    #[serde(default)]
    pub land_on: Option<String>,
    #[serde(default)]
    pub land_on_problem: Option<String>,
}

/// What merging a sub-agent's branch into its lead's task branch did (DEC-104 K5b):
/// merged as the commit named, already in the task branch, in its history with the work
/// absent and the open set left waiting for the owner, or conflicting on the listed files,
/// with the merge undone and the branch left in the lead's clone to merge by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MergeOutcome {
    Merged { commit: String, branch: String },
    AlreadyMerged { branch: String },
    WorkAbsent { branch: String },
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

/// How long a change set store statement waits on another engine connection's write lock
/// before it fails.
pub(crate) const BUSY_WAIT: Duration = Duration::from_secs(5);

const SUBJECT_CHARS_MAX: usize = 48;
const COMMIT_SUBJECT_CHARS_MAX: usize = 72;

fn unavailable() -> EngineError {
    EngineError("changes_unavailable".into())
}

/// Records one action on a change set in its project's activity at the time the set last
/// changed, with its sub-agent for the goal; a failure is logged by its plain code, so the
/// action itself never fails on it. A Discarded set closed outside the app says so, and
/// ChangesReady is a discarded set brought back. Inputs: the store, the change set, the
/// kind of event (Conflict, Discarded, SentBack or ChangesReady) and who acted.
#[cfg(unix)]
fn note_change(
    db: &Connection,
    record: &store::Record,
    kind: crate::activity::ActivityKind,
    by: &str,
) {
    use crate::activity::ActivityKind;
    let label = &record.agent.label;
    let line = &record.prompt_line;
    let text = match kind {
        ActivityKind::Conflict => {
            format!("The changes of {label} conflict with the branch they land on: {line}")
        }
        ActivityKind::Discarded if record.outside => {
            format!("The changes of {label} were closed outside the app: {line}")
        }
        ActivityKind::Discarded => format!("Discarded the changes of {label}: {line}"),
        ActivityKind::ChangesReady => format!("Brought back the changes of {label}: {line}"),
        ActivityKind::SentBack => {
            format!("Sent the changes of {label} back to take in the newer work: {line}")
        }
        _ => unreachable!("note_change takes a change set action"),
    };
    note_text(db, record, kind, by, &text);
}

/// Records an Accept in its project's activity, naming the agents whose sets it closed.
/// Inputs: the store, the accepted change set, who acted and the agents' labels. Output:
/// none.
#[cfg(unix)]
fn note_accepted(db: &Connection, record: &store::Record, by: &str, closed: &[String]) {
    let mut text = crate::activity::accepted_text(by, &named(db, record));
    text.push_str(&closed_words(closed));
    note_text(
        db,
        record,
        crate::activity::ActivityKind::Accepted,
        by,
        &text,
    );
}

/// The words a landing's activity row ends with when it closed other sets. Input: the
/// labels of their agents. Output: the words, empty when it closed none.
#[cfg(unix)]
fn closed_words(closed: &[String]) -> String {
    if closed.is_empty() {
        return String::new();
    }
    format!(". It closed the changes of {}", closed.join(", "))
}

/// The words an activity row names a change set by, with its number once it has one; a
/// number that cannot be read is logged by its plain code and the set named without it.
/// Inputs: the store and the change set. Output: the words.
#[cfg(unix)]
fn named(db: &Connection, record: &store::Record) -> String {
    let number = store::number_of(db, &record.id).unwrap_or_else(|error| {
        eprintln!(
            "landing_number_unread cause={}",
            crate::protocol::plain_code(&error.0)
        );
        None
    });
    change_set_words(
        number,
        &agent_title(record.agent.kind, &record.agent.label),
        &record.message(),
        record.files,
    )
}

/// Records the Undone row of a finished undo, naming the sets its merge closed whose work
/// it reverted too; a failure is logged by its plain code. Inputs: the store, the undone
/// set, the labels of those sets and who undid it.
#[cfg(unix)]
fn note_undo(db: &Connection, record: &store::Record, closed: &[String], by: &str) {
    let mut text = format!(
        "Undid the changes of {}: {}",
        record.agent.label, record.prompt_line
    );
    if !closed.is_empty() {
        text.push_str(&format!(
            ". This also reverted the changes of {}",
            closed.join(", ")
        ));
    }
    note_text(db, record, crate::activity::ActivityKind::Undone, by, &text);
}

/// Records one row about a change set with its words, at the time the set last changed and
/// with its sub-agent for the goal; a failure is logged by its plain code. Inputs: the
/// store, the change set, the kind of event, who acted and the words.
#[cfg(unix)]
fn note_text(
    db: &Connection,
    record: &store::Record,
    kind: crate::activity::ActivityKind,
    by: &str,
    text: &str,
) {
    let agent = match record.agent.kind {
        AgentKind::Sub => record.agent.id.parse().ok(),
        AgentKind::Master => None,
    };
    let item = crate::activity::NewActivity {
        kind,
        actor: by,
        agent,
        goal: None,
        text,
        change_set: Some(record.id.as_str()),
    };
    let at = i64::try_from(record.updated_ms).unwrap_or(i64::MAX);
    crate::activity::note(db, &record.project, &item, at);
}

/// Records that the engine closed a change set whose branch already took every file it
/// changes, with no owner action behind it (HB-S4). Inputs: the store and the closed set.
#[cfg(unix)]
fn note_taken(db: &Connection, record: &store::Record) {
    let text = format!(
        "Closed the changes of {}: the branch they land on already took every file they change. {}",
        record.agent.label, record.prompt_line
    );
    note_text(
        db,
        record,
        crate::activity::ActivityKind::Discarded,
        crate::activity::ENGINE_ACTOR,
        &text,
    );
}

/// Records an action on a stored change set, as [`note_change`] does, once the action is
/// done. Inputs: the store, the change set, the kind of event and who acted.
#[cfg(unix)]
fn noted(db: &Connection, id: &ChangeSetId, kind: crate::activity::ActivityKind, by: &str) {
    match store::load(db, id) {
        Ok(record) => note_change(db, &record, kind, by),
        Err(error) => eprintln!(
            "activity_record_failed kind={} cause={}",
            kind.name(),
            crate::protocol::plain_code(&error.0)
        ),
    }
}

/// Opens the change set store in the engine database, adding its table when missing.
/// Input: the engine root holding `engine.db`. Output: the connection.
pub fn open(runtime: &Path) -> Result<Connection, EngineError> {
    if !runtime.is_absolute() {
        return Err(unavailable());
    }
    let db = Connection::open(runtime.join("engine.db")).map_err(|_| unavailable())?;
    db.busy_timeout(BUSY_WAIT).map_err(|_| unavailable())?;
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

/// The branch a project's work lands on, with its tip (DEC-111 Q1): the master's merges,
/// an Accept with no branch named, the measuring of change sets and the start of new
/// agents. The owner's Land on choice, else the Master's branch as [`master_branch`]
/// resolves it. Inputs: the engine database, the owner checkout root and the project.
/// Output: the branch and tip, None when nothing was chosen and no Master's branch
/// exists, or `changes_target_invalid` for a chosen branch that is no local branch.
pub fn land_on(
    db: &Connection,
    repository: &Path,
    project: &ProjectId,
) -> Result<Option<(String, String)>, EngineError> {
    #[cfg(unix)]
    return owner::land_on_branch(db, repository, project);
    #[cfg(not(unix))]
    {
        let _ = (db, repository, project);
        Err(unavailable())
    }
}

/// Points `refs/crazyagent/main` at the tip of the project's Land on branch in every agent
/// worktree the team records for it, so each agent brings main into its own branch with
/// `git merge crazyagent/main` (OS-8), under each owner repository's hold, which it waits
/// for. A worktree it cannot reach is logged and skipped. Inputs: the engine database and
/// the project. Output: Ok, else `changes_unavailable`.
pub fn refresh_main(db: &Connection, project: &ProjectId) -> Result<(), EngineError> {
    #[cfg(unix)]
    return owner::refresh_main(db, project, None).map(|_| ());
    #[cfg(not(unix))]
    {
        let _ = (db, project);
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
    return subagent::merge(db, project, lead, sub, git).inspect(|outcome| {
        if let MergeOutcome::Merged { branch, .. } = outcome {
            let now = crate::team::now_ms();
            crate::activity::note_lead_merge(db, project, lead, sub, branch, now);
        }
    });
    #[cfg(not(unix))]
    {
        let _ = (db, project, lead, sub, git);
        Err(unavailable())
    }
}

/// Readies an agent's own clone for another launch: makes any of its pack and
/// loose-object folders that are missing, as `add_agent_clone` does for a new one, since
/// the Own branch sandbox keeps the agent from making them itself (RL-3), and gives a clone
/// made before its exclude named the project graph that line (18.15.1). Input: the clone's
/// worktree. Output: Ok, else `changes_workspace_invalid`.
pub fn ready_agent_clone(worktree: &Path) -> Result<(), EngineError> {
    #[cfg(unix)]
    return clone::store_folders(worktree).map(|()| clone::exclude_graph(worktree));
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

/// Records the commit a master's leased worktree starts from, as [`record_worktree_start`]
/// does for a team agent's new worktree, unless a start is stored for it already, so a
/// lease made Ready again never moves the start past commits the master made. The master
/// runtime calls it before each master turn runs. Inputs: the store, the owner's checkout
/// root, the worktree and its git metadata folder. Output: Ok once a start is stored.
pub fn record_master_start(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<(), EngineError> {
    #[cfg(unix)]
    return flow::start_master(db, repository, worktree, git_dir);
    #[cfg(not(unix))]
    {
        let _ = (db, repository, worktree, git_dir);
        Err(unavailable())
    }
}

#[cfg(unix)]
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

/// Tells which of some paths the commit the owner's checkout has checked out holds, with
/// one engine git repository that runs nothing the repository configures; an unborn
/// branch holds none. Inputs: the checkout root and the distinct paths inside the
/// repository, each without pathspec magic. Output: one flag per path in order, or a
/// plain git code for the repository.
pub fn committed_files(checkout: &Path, paths: &[&str]) -> Result<Vec<bool>, EngineError> {
    #[cfg(unix)]
    return head_files(checkout, paths);
    #[cfg(not(unix))]
    {
        let _ = (checkout, paths);
        Err(unavailable())
    }
}

/// The short name of the branch the owner's checkout has checked out, with one engine git
/// repository that runs nothing the repository configures. Input: the checkout root.
/// Output: the name, none for a detached HEAD or a name over `BRANCH_BYTES_MAX`, or a
/// plain git code.
pub fn checked_out_branch(checkout: &Path) -> Result<Option<String>, EngineError> {
    #[cfg(unix)]
    return owner::checked_out_branch(checkout);
    #[cfg(not(unix))]
    {
        let _ = checkout;
        Err(unavailable())
    }
}

#[cfg(unix)]
fn head_files(checkout: &Path, paths: &[&str]) -> Result<Vec<bool>, EngineError> {
    assert!(paths.iter().enumerate().all(|(at, path)| {
        !path.is_empty() && !path.starts_with(':') && !paths[..at].contains(path)
    }));
    let owner = git::Repo::open(checkout, &checkout.join(".git"), None)?;
    let Some(head) = owner.commit_of("HEAD")? else {
        return Ok(vec![false; paths.len()]);
    };
    let mut arguments = vec![
        "ls-tree",
        "-z",
        "--name-only",
        "--full-tree",
        head.as_str(),
        "--",
    ];
    arguments.extend_from_slice(paths);
    let listing = owner.capture(&arguments, TRACKED_LISTING_BYTES_MAX)?;
    let listed: Vec<&[u8]> = listing
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .collect();
    let committed: Vec<bool> = paths
        .iter()
        .map(|path| listed.contains(&path.as_bytes()))
        .collect();
    assert_eq!(committed.len(), paths.len());
    Ok(committed)
}

/// Commits the deletion of files the owner's checkout no longer has, and nothing else, as
/// one commit with the owner's git identity on the owner's checked-out branch, locally
/// and never pushed, as the instruction import's move needs (DEC-106 addendum). Refuses
/// when the index stages any other path. Inputs: the checkout root, the owner's home
/// folder, the distinct paths inside the repository and the commit message. Output: the
/// commit, the branch and the paths it deleted, or `changes_import_none`,
/// `changes_import_staged`, `changes_git_busy`, `changes_detached_head`,
/// `changes_branch_moved`, `changes_owner_identity_missing` or another git code.
pub fn commit_deletions(
    repository: &Path,
    owner_home: &Path,
    paths: &[&str],
    message: &str,
) -> Result<(String, String, Vec<String>), EngineError> {
    #[cfg(unix)]
    return owner::commit_deletions(repository, owner_home, paths, message, &|| {});
    #[cfg(not(unix))]
    {
        let _ = (repository, owner_home, paths, message);
        Err(unavailable())
    }
}

/// Commits every change in an agent worktree as the engine, with hooks, filters and
/// fsmonitor off, and adds it to that worktree's open change set or opens one above the
/// commit the worktree started from, or its HEAD when no start is recorded, so commits
/// the agent made itself land too. A failed or cancelled turn commits too. Owner edits
/// waiting in the worktree settle on the change set the turn end leaves open; when they
/// cannot, they keep waiting for the next turn end and the recorded set is still returned.
/// An agent's claims on its ended work then hold for that set, or are released when
/// nothing is pending. Whether the set then goes back to its agent is the turn-end caller's,
/// through [`conflicts_with_main`] (OS-8). Inputs: the store, the workspace and the turn end.
/// Output: the change set the work now belongs to, None when nothing is pending.
pub fn record_turn_end(
    db: &Connection,
    workspace: &Workspace,
    turn: &TurnEnd,
) -> Result<Option<ChangeSetId>, EngineError> {
    #[cfg(unix)]
    {
        let now = flow::now_ms()?;
        let kept = flow::record(db, workspace, turn, &crate::worktrees::TREE_BOUNDS, now)?;
        owner_edit::logged(
            "settle",
            owner_edit::settle(db, &workspace.worktree, kept.as_ref()),
        );
        flow::hold_agent_claims(db, workspace, kept.as_ref(), now);
        Ok(kept)
    }
    #[cfg(not(unix))]
    {
        let _ = (db, workspace, turn);
        Err(unavailable())
    }
}

/// Settles the claims of a project no turn end will (OS-8 r2): releases those of closed
/// change sets whose release failed, takes back those of restored sets that did not come
/// back, and holds a stopped or failed agent's finished claims for its open set, or
/// releases them when nothing of it waits to land. Inputs: the store, the project and the
/// time now. Output: Ok, or the first store failure.
pub fn sweep_claims(db: &Connection, project: &ProjectId, now: u64) -> Result<(), EngineError> {
    #[cfg(unix)]
    {
        flow::sweep_claims(db, project, now)
    }
    #[cfg(not(unix))]
    {
        let _ = (db, project, now);
        Err(unavailable())
    }
}

/// The plain code of a turn-end refusal as an owner window shows it: a refusal carrying
/// text for the agent (see [`refusal_for_agent`]) shows its code alone. Input: the
/// refusal. Output: the plain code.
pub fn refusal_code(problem: &EngineError) -> &str {
    crate::protocol::plain_code(problem.0.split('\n').next().unwrap_or_default())
}

/// What a turn-end refusal tells the agent whose work it refused, so the agent can fix its
/// work and finish again: for a new folder past the owner's limit, the folder, how many
/// files it adds and the limit (DEC-107 rule 3). Input: the refusal. Output: the text,
/// None for a refusal the agent cannot fix.
pub fn refusal_for_agent(problem: &EngineError) -> Option<String> {
    let mut parts = problem.0.splitn(4, '\n');
    if parts.next() != Some(NEW_FOLDER_REFUSED) {
        return None;
    }
    let files: u32 = parts.next()?.parse().ok()?;
    let limit: u32 = parts.next()?.parse().ok()?;
    let folder = parts
        .next()
        .filter(|folder| !folder.is_empty() && !folder.contains('\n'))?;
    Some(format!(
        "CrazyAgent did not keep your work for review: the new top-level folder {folder}/ \
         adds {files} files, more than this project's limit of {limit} files for a folder \
         the owner's branch does not have. Delete that folder from your work (commit the \
         removal if you commit yourself), keep only the files the task needs, and finish \
         again."
    ))
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
/// is Lead, since that lead merges them (F3), but with a worker's open set whose lead's
/// copy was removed outside the app or whose lead's set closed and left it open, since no
/// lead will. Inputs: the store and the project. Output: the change sets.
pub fn owner_list(db: &Connection, project: &ProjectId) -> Result<Vec<ChangeSetView>, EngineError> {
    let sets = list(db, project)?;
    assert!(sets.len() <= LIST_MAX);
    #[cfg(unix)]
    if subagent::leads_merge(db, project)? {
        let mut kept = Vec::with_capacity(sets.len());
        for set in sets {
            let open = matches!(
                set.state,
                ChangeState::Pending | ChangeState::Conflict | ChangeState::Updating
            );
            if !subagent::is_lead_worker(db, project, &set.agent)?
                || (open && subagent::lead_left(db, project, &set)?)
            {
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
/// Counts the change sets waiting for the owner in Changes: ready to review or conflicting
/// with their branch, in the projects the engine still has. One its agent is updating waits
/// on the agent instead. Inputs: the store and the project whose own count is wanted, if
/// any. Output: the count in every project, and that project's count when one was given.
pub fn waiting(
    db: &Connection,
    project: Option<&ProjectId>,
) -> Result<WaitingChanges, EngineError> {
    #[cfg(unix)]
    return store::waiting(db, project);
    #[cfg(not(unix))]
    {
        let _ = (db, project);
        Err(unavailable())
    }
}

/// Lists one project's change sets, open ones first, then newest first, at most
/// `LIST_MAX`. Inputs: the store and the project. Output: the change sets.
pub fn list(db: &Connection, project: &ProjectId) -> Result<Vec<ChangeSetView>, EngineError> {
    #[cfg(unix)]
    return store::of_project(db, project, LIST_MAX)?
        .iter()
        .map(|record| Ok(record.view(store::number_of(db, &record.id)?)))
        .collect();
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
    return store::find(db, id)?
        .map(|record| {
            let number = store::number_of(db, &record.id)?;
            Ok((record.project.clone(), record.view(number)))
        })
        .transpose();
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
        store::open_for(db, worktree)?
            .map(|record| Ok(record.view(store::number_of(db, &record.id)?)))
            .transpose()
    }
    #[cfg(not(unix))]
    {
        let _ = (db, worktree);
        Err(unavailable())
    }
}

/// Names the teammate worktrees `<worktree>/.worktrees/<name>` of a lead that hold an open
/// change set (OP9B-M1), read from the store by their folder, so the bound is on open sets,
/// never on how many teammates the lead ever had (OP9B-R2-L1). Inputs: the store, the
/// lead's worktree and the most names to read. Output: the folder names in path order.
pub fn open_teammate_sets(
    db: &Connection,
    worktree: &Path,
    max: usize,
) -> Result<Vec<String>, EngineError> {
    #[cfg(unix)]
    {
        let Some(folder) = worktree
            .join(crate::agent_git::TEAMMATES_FOLDER)
            .to_str()
            .map(|folder| format!("{folder}/"))
        else {
            return Ok(Vec::new());
        };
        Ok(store::open_inside(db, &folder, max)?
            .into_iter()
            .filter_map(|path| path.strip_prefix(&folder).map(str::to_owned))
            .collect())
    }
    #[cfg(not(unix))]
    {
        let _ = (db, worktree, max);
        Err(unavailable())
    }
}

/// Reads one page of a change set's diff, produced by the engine with no external diff or
/// text conversion, only while the set is still at the head the owner is reading, so every
/// page shown is of the head Accept and Discard name (RV-DS-13, RV-DS-79): for an open
/// set, first noticed against the owner's branch, only what would change on that branch
/// (DEC-107), from the merge base of its tip and the set's head, or only what is not yet
/// on it when the owner landed part of it outside the app; else the whole change between
/// its base and head. Files the owner edited in the agent's worktree before a turn end
/// recorded them in this change set carry `OwnerEdited`. Inputs: the store, the change set,
/// that head and the page. Output: the page, or `changes_stale` once the set moved.
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
        let range = owner::observe_for_diff(db, id).unwrap_or_else(|error| {
            eprintln!(
                "landing_observe_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
            None
        });
        let mut shown = match range {
            Some((from, to)) => diff::page(&record.repository, &from, &to, page)?,
            None => diff::page(&record.repository, &record.base, &record.head, page)?,
        };
        owner_edit::mark(db, &record.worktree, &record.id, &mut shown.files)?;
        Ok(shown)
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
/// landed commit and branch, or the conflicting files. `place` is where the owner pressed
/// Accept, which the project's activity names.
pub fn accept(
    db: &Connection,
    owner_home: &Path,
    id: &ChangeSetId,
    expected_head: &str,
    message: Option<&str>,
    place: crate::activity::AcceptPlace,
) -> Result<AcceptOutcome, EngineError> {
    #[cfg(unix)]
    return owner::accept(db, owner_home, id, expected_head, message, place, &|_| {
        false
    });
    #[cfg(not(unix))]
    {
        let _ = (db, owner_home, id, expected_head, message, place);
        Err(unavailable())
    }
}

/// Lands a change set as one squash commit on a chosen local branch: the owner's
/// checked-out branch through the owner's checkout as [`accept`] does, a branch checked
/// out nowhere by moving its ref only, leaving the owner's files untouched, or an agent's
/// own branch through that agent's worktree, which must hold nothing uncommitted. Inputs:
/// the store, the owner's home folder, the change set, the head the owner reviewed, the
/// edited message, None for the proposed one, the target branch and where the owner pressed
/// Accept. Output: the landed commit and branch, or the conflicting files.
pub fn accept_into(
    db: &Connection,
    owner_home: &Path,
    id: &ChangeSetId,
    expected_head: &str,
    message: Option<&str>,
    target_branch: &str,
    place: crate::activity::AcceptPlace,
) -> Result<AcceptOutcome, EngineError> {
    #[cfg(unix)]
    return owner::accept_to(
        db,
        owner_home,
        id,
        expected_head,
        message,
        Some(target_branch),
        owner::By::Owner(Some(place)),
        &|_| false,
    );
    #[cfg(not(unix))]
    {
        let _ = (
            db,
            owner_home,
            id,
            expected_head,
            message,
            target_branch,
            place,
        );
        Err(unavailable())
    }
}

/// The refusal of a land the owner did not press while a worker below the set's agent
/// holds an open change set (HB-S4-LANDED).
pub const WORKER_OPEN: &str = "changes_worker_open";

/// What the agent refused with [`WORKER_OPEN`] reads, after "Not merged" or "Not accepted".
pub const WORKER_OPEN_TEXT: &str = "a worker's work under that agent is still open. The owner or, while Lands is Lead, that worker's lead has to land or discard it first. Then merge again.";

/// Lands a change set on a chosen local branch as [`accept_into`] does, for an Accept the
/// owner did not make: the project's activity names who accepted it, the master or the
/// engine. The owner's Accept goes through [`accept`] or [`accept_into`] instead, which know
/// where it was pressed. Inputs: as [`accept_into`] without the place, plus who accepts.
/// Output: the landed commit and branch, or the conflicting files.
pub fn accept_into_as(
    db: &Connection,
    owner_home: &Path,
    id: &ChangeSetId,
    expected_head: &str,
    message: Option<&str>,
    target_branch: &str,
    by: &str,
) -> Result<AcceptOutcome, EngineError> {
    assert_ne!(
        by,
        crate::activity::OWNER_ACTOR,
        "the owner's Accept goes through accept or accept_into"
    );
    #[cfg(unix)]
    return owner::accept_to(
        db,
        owner_home,
        id,
        expected_head,
        message,
        Some(target_branch),
        owner::By::Named(by),
        &|_| false,
    );
    #[cfg(not(unix))]
    {
        let _ = (
            db,
            owner_home,
            id,
            expected_head,
            message,
            target_branch,
            by,
        );
        Err(unavailable())
    }
}

/// Lands the change set the project's master made the way an Accept does, when it holds
/// nothing but fixes of real conflicts with other open sets, `team::master_lands` says
/// Merges and "Ask me before merging" is off at that moment: one squash commit on the Land
/// on branch, every open set of the project the master's head holds closed with it, and
/// one MergedIntoMain activity row naming them. Anything else leaves everything as it was.
/// Inputs: the store, the owner's home and the change set. Output: the outcome of the
/// land, or None when nothing was tried.
pub fn land_master_set(
    db: &Connection,
    owner_home: &Path,
    id: &ChangeSetId,
) -> Result<Option<AcceptOutcome>, EngineError> {
    #[cfg(unix)]
    return owner::land_master(db, owner_home, id, &|_| false);
    #[cfg(not(unix))]
    {
        let _ = (db, owner_home, id);
        Ok(None)
    }
}

/// Undoes a change set its own Accept merged into main: one commit on the branch it landed
/// on that reverts the landed commit by a three-way merge, made with the owner's identity
/// and named `Undo: <landed subject>`, and one Undone activity row; the merge's own rows
/// then show Undone. A conflict with what that branch changed since returns the files and
/// changes nothing. Inputs: the store, the owner's home and the change set. Output: the
/// undo commit and branch, the conflicts, or `changes_undo_unavailable` for a set whose
/// Accept made no commit of its own or that is already undone.
pub fn undo_landing(
    db: &Connection,
    owner_home: &Path,
    id: &ChangeSetId,
) -> Result<AcceptOutcome, EngineError> {
    #[cfg(unix)]
    return owner::undo(db, owner_home, id, &|_| false);
    #[cfg(not(unix))]
    {
        let _ = (db, owner_home, id);
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
    discard_as(db, id, expected_head, now_ms, crate::activity::OWNER_ACTOR)
}

/// Discards a change set as [`discard`] does, recording who did it in the project's
/// activity. Inputs: the store, the change set, the head that was reviewed, the time now
/// and the actor. Output: the restorable-until time.
pub fn discard_as(
    db: &Connection,
    id: &ChangeSetId,
    expected_head: &str,
    now_ms: u64,
    by: &str,
) -> Result<u64, EngineError> {
    #[cfg(unix)]
    return flow::discard(
        db,
        id,
        expected_head,
        now_ms,
        by == crate::activity::OWNER_ACTOR,
    )
    .inspect(|_| noted(db, id, crate::activity::ActivityKind::Discarded, by));
    #[cfg(not(unix))]
    {
        let _ = (db, id, expected_head, now_ms, by);
        Err(unavailable())
    }
}

/// Brings a discarded change set back as pending, with its work back in the agent's
/// worktree. Inputs: the store, the change set and the time now. Output: Ok once
/// pending.
pub fn restore(db: &Connection, id: &ChangeSetId, now_ms: u64) -> Result<(), EngineError> {
    #[cfg(unix)]
    return flow::restore(db, id, now_ms).inspect(|()| {
        noted(
            db,
            id,
            crate::activity::ActivityKind::ChangesReady,
            crate::activity::OWNER_ACTOR,
        )
    });
    #[cfg(not(unix))]
    {
        let _ = (db, id, now_ms);
        Err(unavailable())
    }
}

/// Tells whether an agent's change set conflicts with the tip it lands on, so its turn
/// end can send it back to its agent before the owner sees it (OS-8). Only a Pending set
/// of a sub-agent in a linked worktree that its own resolve task fixes is checked, and
/// never with an owner-configured filter or merge driver on a path it changes. A clash in
/// known lockfiles only is rewritten by their tools on top of the set instead, which then
/// waits for it, not its agent, when `after` is given: the rewrite then runs off this
/// thread and `after` runs once it lands nothing. Inputs: the store, the change set and
/// what runs after. Output: the answer, or the plain code of a check that could not run.
pub(crate) fn conflicts_with_main(
    db: &Connection,
    id: &ChangeSetId,
    after: Option<AfterRegeneration>,
) -> Result<bool, EngineError> {
    #[cfg(unix)]
    return owner::conflicts_with_main(db, id, after);
    #[cfg(not(unix))]
    {
        let _ = (db, id, after);
        Err(unavailable())
    }
}

/// What runs after a lockfile regeneration that landed nothing, on its own thread with its
/// own store: the set, and whether another regeneration may start, which is false once
/// one refused the clash and true when the set or main moved meanwhile.
pub(crate) type AfterRegeneration = fn(&Connection, &ChangeSetId, bool);

/// Removes what lockfile regenerations a stopped engine left behind (OS-8): their
/// temporary worktrees with their admin folders and the engine's scratch folders. Input:
/// the store. Output: how many temporary worktrees it removed.
pub fn sweep_regenerations(db: &Connection) -> Result<usize, EngineError> {
    #[cfg(unix)]
    return lockfiles::sweep(db);
    #[cfg(not(unix))]
    {
        let _ = db;
        Ok(0)
    }
}

/// Merges the owner's branch tip into the agent's worktree with conflict markers, ready
/// for the resolve task. Inputs: the store and the change set. Output: Ok once merged.
pub fn update(db: &Connection, id: &ChangeSetId) -> Result<(), EngineError> {
    #[cfg(unix)]
    return owner::update(db, id).inspect(|()| {
        noted(
            db,
            id,
            crate::activity::ActivityKind::SentBack,
            crate::activity::OWNER_ACTOR,
        )
    });
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

/// Names a change set as the owner reads it everywhere: "Change set #16 (Wren: checkout
/// UI, 7 files)", or its agent alone for a set the engine has not numbered yet. Input: the
/// change set. Output: the name.
pub fn change_set_name(set: &ChangeSetView) -> String {
    let agent = agent_title(set.agent.kind, &set.agent.label);
    match set.number {
        Some(_) => change_set_words(set.number, &agent, &set.message, set.files).replacen(
            "change set",
            "Change set",
            1,
        ),
        None => agent,
    }
}

/// One open change set as the owner's Changes view reads it: its name, its size, the
/// view's state words and the files it conflicts on, so a master and the owner mean the
/// same set (HB-S5-J). Names and paths come from agents, so the whole line stays one line
/// with its control and hidden characters replaced. Input: the open change set. Output: the
/// line.
pub fn open_set_line(set: &ChangeSetView) -> String {
    let state = match set.state {
        ChangeState::Pending => "Ready to review",
        ChangeState::Conflict => "Conflicts with the owner's branch",
        ChangeState::Updating if set.waiting => "Updated, waiting for the agent",
        ChangeState::Updating if !set.conflicts.is_empty() => "Agent is fixing conflicts with main",
        ChangeState::Updating => "Agent is updating",
        ChangeState::Accepted | ChangeState::Discarded => {
            unreachable!("only an open change set has a line")
        }
    };
    let files = if set.conflicts.is_empty() {
        String::new()
    } else {
        let joined = set.conflicts.join(", ");
        format!(": {}", crate::team::bound_text(&joined, OPEN_SET_FILES_MAX))
    };
    let line = format!(
        "{}, +{} -{}, {state}{files}",
        change_set_name(set),
        set.added,
        set.removed
    );
    crate::memory::visible_text(&line.replace(char::is_control, "\u{fffd}"))
}

/// The words a sentence names a change set by: "change set #16 (Wren: checkout UI, 7
/// files)", or "the changes of Wren: checkout UI" while the engine has not numbered it.
/// Inputs: its number, its agent's name, its commit message and its file count. Output:
/// the words.
pub fn change_set_words(number: Option<u64>, agent: &str, message: &str, files: u32) -> String {
    let subject = subject(message);
    let about = if subject.is_empty() {
        agent.to_owned()
    } else {
        format!("{agent}: {subject}")
    };
    let Some(number) = number else {
        return format!("the changes of {about}");
    };
    assert!(number >= 1, "a change set number starts at 1");
    let files = if files == 1 {
        "1 file".to_owned()
    } else {
        format!("{files} files")
    };
    format!("change set #{number} ({about}, {files})")
}

/// The name a change set's agent goes by: Master for the master, else the label the owner
/// knows it by. Inputs: the agent's kind and label. Output: the name.
pub fn agent_title(kind: AgentKind, label: &str) -> String {
    match kind {
        AgentKind::Master => "Master".to_owned(),
        AgentKind::Sub => label.to_owned(),
    }
}

/// A commit subject from free text: its first line through `scrubbed`, ended at the last
/// whole word within `COMMIT_SUBJECT_CHARS_MAX` characters. A first word longer than that is
/// cut to fit. Inputs: the text and the folders, innermost first. Output: the subject, maybe
/// empty.
pub(crate) fn commit_subject(text: &str, roots: &[&Path]) -> String {
    let line = scrubbed(text.lines().next().unwrap_or_default(), roots);
    let mut subject = String::new();
    for word in line.split(' ').filter(|word| !word.is_empty()) {
        let (used, more) = (subject.chars().count(), word.chars().count());
        if used == 0 && more > COMMIT_SUBJECT_CHARS_MAX {
            return word.chars().take(COMMIT_SUBJECT_CHARS_MAX).collect();
        }
        if used + usize::from(used > 0) + more > COMMIT_SUBJECT_CHARS_MAX {
            break;
        }
        if used > 0 {
            subject.push(' ');
        }
        subject.push_str(word);
    }
    assert!(subject.chars().count() <= COMMIT_SUBJECT_CHARS_MAX);
    subject
}

/// The message a change set goes by in Changes and in the project's activity, live or
/// backfilled: its stored message, the owner's or the proposed one, else its agent's prompt,
/// else "Changes from <agent>", with hidden characters shown as escapes. Inputs: the stored
/// message, the prompt line and the agent's label. Output: the message.
pub fn change_set_message(message: &str, prompt_line: &str, label: &str) -> String {
    let message = if !message.is_empty() {
        message.to_owned()
    } else if !prompt_line.is_empty() {
        prompt_line.to_owned()
    } else {
        format!("Changes from {}", commit_subject(label, &[]))
    };
    crate::memory::visible_text(&message)
}

/// The first line of a commit message, cut to `SUBJECT_CHARS_MAX` characters ending in
/// "..." when longer. Input: the message. Output: the subject, empty for an empty message.
fn subject(message: &str) -> String {
    let line = message.lines().next().unwrap_or("").trim();
    if line.chars().count() <= SUBJECT_CHARS_MAX {
        return line.to_owned();
    }
    let kept: String = line.chars().take(SUBJECT_CHARS_MAX - 3).collect();
    format!("{kept}...")
}

/// Numbers the change sets of the projects the engine has that carry no number yet, such
/// as the ones an engine without numbers wrote, after each project's highest number, in the
/// order they were made. Runs at every engine start. Input: the store. Output: how many
/// sets got a number.
pub fn number_change_sets(db: &Connection) -> Result<usize, EngineError> {
    #[cfg(unix)]
    {
        store::initialize(db)?;
        store::number_unnumbered(db)
    }
    #[cfg(not(unix))]
    {
        let _ = db;
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

/// Names the native teammates of a harness that work in their own worktree,
/// `<worktree>/.worktrees/<name>`, on their own branch `<task branch>--<name>` (DEC-111 Q17,
/// OP-9b), read as data from its clone's git records, or for a linked worktree the project
/// repository's, so no git runs. Inputs: the store, the worktree and its git folder.
/// Output: the names, or the code that kept them from being read.
pub fn teammate_worktrees(
    db: &Connection,
    worktree: &Path,
    git_dir: &Path,
) -> Result<Vec<String>, EngineError> {
    #[cfg(unix)]
    return retire::own_teammates(db, worktree, git_dir);
    #[cfg(not(unix))]
    {
        let _ = (db, worktree, git_dir);
        Err(unavailable())
    }
}

/// The linked worktree and git folder of one native teammate of a linked lead that works in
/// its own worktree, `<worktree>/.worktrees/<name>`, on its own branch
/// `<task branch>--<name>` (OP-9b), read as data with no git run, so the engine commits
/// that teammate's turns there. Inputs: the store, the lead's worktree and git folder and
/// the teammate's name. Output: the teammate's worktree and git folder, None for a lead in
/// its own clone or a teammate with none, or the code that kept them from being read.
pub fn linked_teammate_place(
    db: &Connection,
    worktree: &Path,
    git_dir: &Path,
    name: &str,
) -> Result<Option<(PathBuf, PathBuf)>, EngineError> {
    #[cfg(unix)]
    return retire::linked_teammate_place(db, worktree, git_dir, name);
    #[cfg(not(unix))]
    {
        let _ = (db, worktree, git_dir, name);
        Err(unavailable())
    }
}

/// Makes one native teammate's own worktree inside its lead's worktree (OP-9, OP-9b):
/// `<worktree>/.worktrees/<name>` on `<task branch>--<name>`, cut from the lead's HEAD for a
/// new branch and checked out as it stands for one an earlier run left: a worktree of the
/// lead's own clone, with git run only with the lead's rights, or for a linked lead a
/// linked worktree of the project repository, with git run under the lead's teammates'
/// setup profile. Inputs: the store, the lead's worktree and git folder, the teammate's
/// name and that git. Output: Ok once git records the worktree on that branch, else a
/// plain `changes_*` refusal.
pub fn add_teammate_worktree(
    db: &Connection,
    worktree: &Path,
    git_dir: &Path,
    name: &str,
    git: &LeadGit<'_>,
) -> Result<(), EngineError> {
    #[cfg(unix)]
    return if git_dir == crate::agent_git::git_dir(worktree) {
        subagent::add_teammate(db, worktree, git_dir, name, git)
    } else {
        let repository = retire::linked_repository(git_dir)?;
        subagent::add_linked_teammate(db, repository, worktree, git_dir, name, git)
    };
    #[cfg(not(unix))]
    {
        let _ = (db, worktree, git_dir, name, git);
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
