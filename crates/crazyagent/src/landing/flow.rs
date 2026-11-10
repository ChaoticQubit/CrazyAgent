use super::clone;
use super::diff::{self, plain};
use super::git::{Repo, Writes, failure, is_object_id, object_id};
use super::store::{self, Intent, Record};
use super::{AgentKind, ChangeSetId, ChangeState, RESTORE_WINDOW_MS, TurnEnd, Workspace};
use crate::agent_git::{self, Layout};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use crate::worktrees::TreeBounds;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};

pub(super) const ENGINE: (&str, &str) = ("CrazyAgent", "engine@crazyagent.invalid");
const TASK_SUFFIXES_MAX: usize = 64;
const LISTING_BYTES_MAX: usize = 64 * 1024 * 1024;
const PATH_BYTES_MAX: usize = 1024;
const FOLDER_CHARS_MAX: usize = 128;
const LAYOUT_NAMES: [&str; 4] = ["head", "objects", "refs", "commondir"];
const LAYOUT_FOLDERS_MAX: usize = 64;
const LINK_HEADER_BYTES_MAX: usize = 128;
const NAME_BYTES_MAX: usize = 128;
/// The most closed change sets holding claims one claims sweep releases.
const SWEPT_SETS_MAX: usize = 16;
const PROPOSED_BYTES_MAX: usize = 1024;
const AGENT_COMMITS_READ_MAX: usize = 200;
const MORE_COMMITS: &str = "\n- and more commits";
const LINK_BYTES_MAX: usize = 4096;
const HEAD_BYTES_MAX: usize = 512;
const CONFLICTS_SHOWN_MAX: usize = 50;
const INDEX_BYTES_MAX: u64 = 256 * 1024 * 1024;
static BUSY: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
static FREED: Condvar = Condvar::new();

/// The engine's hold on one owner repository: landing work on it runs one at a time in
/// this engine. Dropping it lets the next one start.
pub(super) struct Held(PathBuf);

impl Drop for Held {
    fn drop(&mut self) {
        let mut busy = BUSY.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(busy.remove(&self.0));
        FREED.notify_all();
    }
}

/// Waits for and takes the engine's hold on one owner repository.
/// Input: the repository root. Output: the hold.
pub(super) fn hold(repository: &Path) -> Held {
    let mut busy = BUSY.lock().unwrap_or_else(PoisonError::into_inner);
    while busy.contains(repository) {
        busy = FREED.wait(busy).unwrap_or_else(PoisonError::into_inner);
    }
    assert!(busy.insert(repository.to_path_buf()));
    Held(repository.to_path_buf())
}

/// Tells whether someone holds the engine's hold on one owner repository right now.
/// Input: the repository root. Output: the answer.
#[cfg(test)]
pub(super) fn busy(repository: &Path) -> bool {
    BUSY.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(repository)
}

fn invalid() -> EngineError {
    failure("changes_workspace_invalid")
}

fn canonical(path: &Path) -> Result<(), EngineError> {
    if path.is_absolute() && std::fs::canonicalize(path).ok().as_deref() == Some(path) {
        return Ok(());
    }
    Err(invalid())
}

/// Checks an agent's workspace and says which layout it has: every path absolute and
/// free of links, the repository's `.git` a real folder and the worktree a real folder
/// outside it. A `Clone` has the git folder the engine names for it, `<worktree>/.git`, as
/// a real folder and a worktree folder that names a branch. A `Linked` worktree's git
/// folder sits directly under the repository's `.git/worktrees` and names this
/// worktree's `.git` as its own. Inputs: the three paths, as the engine recorded them.
/// Output: the layout, or `changes_workspace_invalid`.
pub(super) fn check_places(
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<Layout, EngineError> {
    for path in [repository, worktree, git_dir] {
        canonical(path)?;
    }
    let metadata = repository.join(".git");
    if !std::fs::symlink_metadata(&metadata).is_ok_and(|found| found.is_dir())
        || worktree == repository
        || worktree.starts_with(&metadata)
        || !std::fs::symlink_metadata(worktree).is_ok_and(|found| found.is_dir())
    {
        return Err(invalid());
    }
    if git_dir == agent_git::git_dir(worktree) {
        if agent_git::branch(worktree).is_none()
            || !std::fs::symlink_metadata(git_dir).is_ok_and(|found| found.is_dir())
        {
            return Err(invalid());
        }
        return Ok(Layout::Clone);
    }
    if git_dir.parent() != Some(metadata.join("worktrees").as_path()) {
        return Err(invalid());
    }
    let link = bounded_read(&git_dir.join("gitdir"), LINK_BYTES_MAX)?;
    let named = std::str::from_utf8(&link).map_err(|_| invalid())?;
    if Path::new(named.trim_end_matches('\n')) != worktree.join(".git") {
        return Err(invalid());
    }
    Ok(Layout::Linked)
}

/// Tells whether a change set's agent works in its own clone, from the paths the engine
/// recorded for it, without touching the disk. Input: the change set. Output: the answer.
pub(super) fn in_clone(record: &Record) -> bool {
    record.git_dir == agent_git::git_dir(&record.worktree)
}

/// Checks that every object the `wanted` commits hold beyond the `trusted` ones has the
/// bytes its id names: packs them into a private folder and has git index that pack,
/// which hashes each object again from its bytes, and refuses unless every listed id is
/// in that index. Git reads a loose object as stored without checking it, so this is the
/// check before the engine writes from objects a confined agent could have planted. The
/// pack is never installed. Inputs: the owner checkout root and the commits.
/// Output: Ok, else `changes_workspace_invalid` for an object whose bytes do not match.
pub(super) fn verify_objects(
    repository: &Path,
    wanted: &[&str],
    trusted: &[&str],
) -> Result<(), EngineError> {
    assert!(!wanted.is_empty());
    let shared = shared(repository)?;
    let mut args = vec!["rev-list", "--objects", "--no-object-names"];
    args.extend(wanted);
    args.push("--not");
    args.extend(trusted);
    let listed = shared.run(&args)?;
    let ids = listed
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(object_id)
        .collect::<Result<BTreeSet<String>, EngineError>>()?;
    if ids.is_empty() {
        return Ok(());
    }
    let folder = repository.join(".git").join(format!(
        "crazyagent-verify-{}",
        &crate::engine::owner::random_id()?[..24]
    ));
    std::fs::create_dir(&folder).map_err(|_| failure("changes_git_failed"))?;
    let verified = verify_into(&shared, &folder, &ids);
    let _ = std::fs::remove_dir_all(&folder);
    verified
}

fn verify_into(shared: &Repo, folder: &Path, ids: &BTreeSet<String>) -> Result<(), EngineError> {
    let refused = || failure("changes_workspace_invalid");
    let text = |path: &Path| path.to_str().map(str::to_owned).ok_or_else(refused);
    let mut input = String::new();
    for id in ids {
        input.push_str(id);
        input.push('\n');
    }
    let packed = shared.exec(
        &["pack-objects", "-q", &text(&folder.join("check"))?],
        Some(input.as_bytes()),
        &[],
        256,
    )?;
    if packed.code != 0 || packed.overflow {
        return Err(refused());
    }
    let checksum = object_id(&packed.stdout)?;
    let pack = folder.join(format!("check-{checksum}.pack"));
    let index = folder.join("verified.idx");
    let indexed = shared.exec(
        &["index-pack", "-o", &text(&index)?, &text(&pack)?],
        None,
        &[],
        256,
    )?;
    if indexed.code != 0 || indexed.overflow {
        return Err(refused());
    }
    let bytes = bounded_bytes(&index, INDEX_BYTES_MAX)?;
    let shown = shared.exec(&["show-index"], Some(&bytes), &[], LISTING_BYTES_MAX)?;
    if shown.code != 0 || shown.overflow {
        return Err(refused());
    }
    let mut found = BTreeSet::new();
    for line in shown
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let id = line
            .split(|byte| *byte == b' ')
            .nth(1)
            .ok_or_else(refused)?;
        found.insert(object_id(id)?);
    }
    if !ids.is_subset(&found) {
        return Err(refused());
    }
    assert!(found.len() >= ids.len());
    Ok(())
}

fn bounded_bytes(path: &Path, limit: u64) -> Result<Vec<u8>, EngineError> {
    use std::io::Read;
    let mut bytes = Vec::new();
    open_plain(path)
        .ok_or_else(|| failure("changes_git_failed"))?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("changes_git_failed"))?;
    if bytes.len() as u64 > limit {
        return Err(failure("changes_workspace_invalid"));
    }
    Ok(bytes)
}

/// Opens a file an agent may have replaced, for reading only, without following a link
/// and without waiting on a FIFO or device, and keeps it only when it is a regular file.
/// Input: the path. Output: the open file, or none.
#[cfg(unix)]
fn open_plain(path: &Path) -> Option<std::fs::File> {
    let file = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok()
    .map(std::fs::File::from)?;
    file.metadata().ok()?.is_file().then_some(file)
}

/// Opens a regular file for reading where the unix flags do not exist. Input: the path.
/// Output: the open file, or none.
#[cfg(not(unix))]
fn open_plain(path: &Path) -> Option<std::fs::File> {
    let file = std::fs::File::open(path).ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

/// Reads a file an agent may have replaced, at most `limit` bytes, through `open_plain`.
/// Inputs: the path and the limit. Output: the bytes, else `changes_workspace_invalid` for
/// a file that is not plain or is longer.
pub(super) fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, EngineError> {
    use std::io::Read;
    let file = open_plain(path).ok_or_else(invalid)?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > limit {
        return Err(invalid());
    }
    Ok(bytes)
}

fn name(text: &str) -> Result<String, EngineError> {
    if text.is_empty() || text.len() > NAME_BYTES_MAX || text.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(text.to_owned())
}

/// The message an Accept proposes from the commits above the set's measured-from point: the
/// latest subject, then earlier ones newest first as a list. Engine commits, merges and
/// commits on the landing branch are left out; a lead's worker on Land on main lists the
/// lead's unlanded commits too, which land in its squash. Inputs: the agent repository, the
/// record, that commit and the display roots. Output: the message, up to `PROPOSED_BYTES_MAX`.
fn proposed(
    repo: &Repo,
    record: &Record,
    from: &str,
    roots: &[&Path],
) -> Result<String, EngineError> {
    let count = format!("--max-count={AGENT_COMMITS_READ_MAX}");
    let range = format!("{from}..{}", record.head);
    let printed = repo.run(&[
        "log",
        "-z",
        "--no-merges",
        "--no-show-signature",
        "--format=%ae%x00%s",
        &count,
        &range,
        "--",
    ])?;
    let printed = String::from_utf8_lossy(&printed);
    let fields: Vec<&str> = printed.split_terminator('\0').collect();
    let mut subjects = fields
        .as_chunks::<2>()
        .0
        .iter()
        .filter(|commit| commit[0] != ENGINE.1)
        .map(|commit| super::commit_subject(commit[1], roots))
        .filter(|line| !line.is_empty());
    let Some(mut message) = subjects.next() else {
        return Ok(String::new());
    };
    let mut cut = fields.len() / 2 >= AGENT_COMMITS_READ_MAX;
    let mut list = String::new();
    for earlier in subjects {
        if message.len() + 2 + list.len() + earlier.len() + 3 + MORE_COMMITS.len()
            > PROPOSED_BYTES_MAX
        {
            cut = true;
            break;
        }
        list.push_str("\n- ");
        list.push_str(&earlier);
    }
    if cut {
        list.push_str(MORE_COMMITS);
    }
    if !list.is_empty() {
        message.push('\n');
        message.push_str(&list);
    }
    assert!(message.len() <= PROPOSED_BYTES_MAX);
    Ok(message)
}

pub(super) fn now_ms() -> Result<u64, EngineError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_millis()).ok())
        .ok_or_else(|| failure("changes_unavailable"))
}

fn new_id() -> Result<ChangeSetId, EngineError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| failure("changes_unavailable"))?;
    ChangeSetId::new(
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    )
}

/// Stores the commit the next change set of a worktree opens above and, for an agent's own
/// clone, first points the clone's base ref at it in the owner repository (GA-R5).
/// Inputs: the store, the owner checkout, the worktree, its git folder and the commit.
/// Output: Ok once both are stored.
pub(super) fn set_base(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
    base: &str,
) -> Result<(), EngineError> {
    if git_dir == agent_git::git_dir(worktree) {
        set_engine_ref(
            &shared(repository)?,
            repository,
            &clone::base_ref(worktree)?,
            Some(base),
        )?;
    }
    store::set_base(db, worktree, base)
}

pub(super) fn pending_ref(id: &ChangeSetId) -> String {
    format!("refs/crazyagent/pending/{}", id.as_str())
}

pub(super) fn discarded_ref(id: &ChangeSetId) -> String {
    format!("refs/crazyagent/discarded/{}", id.as_str())
}

/// Points one of the engine's own refs at a commit, or deletes it, in the owner's shared
/// repository, never through a symbolic ref or a reflog link that anyone able to write
/// the repository's refs left there. Inputs: the shared repository, its root, the ref and
/// the commit, none to delete. Output: Ok once done.
pub(super) fn set_engine_ref(
    shared: &Repo,
    repository: &Path,
    reference: &str,
    commit: Option<&str>,
) -> Result<(), EngineError> {
    assert!(reference.starts_with("refs/crazyagent/"));
    plain_log(&repository.join(".git"), reference)?;
    match commit {
        Some(commit) => shared.run(&["update-ref", "--no-deref", reference, commit])?,
        None => shared.run(&["update-ref", "--no-deref", "-d", reference])?,
    };
    Ok(())
}

/// Opens the owner repository's shared git folder for object and ref work.
pub(super) fn shared(record_repository: &Path) -> Result<Repo, EngineError> {
    Repo::open(record_repository, &record_repository.join(".git"), None)
}

/// The HEAD an agent worktree must hold for the engine to read or move it: a detached
/// commit or its own `agent_git::linked_branch` for a sub-agent, exactly its lease
/// branch for the master.
pub(super) enum Pointer {
    Detached,
    Branch(String),
}

/// An agent worktree opened for engine work. Its HEAD is read and moved only through
/// [`Agent::head`] and [`Agent::move_head`], never through a ref or link the agent chose.
pub(super) struct Agent {
    pub(super) repo: Repo,
    repository: PathBuf,
    git_dir: PathBuf,
    pointer: Pointer,
}

impl Agent {
    /// Names the branch this agent worktree has checked out, the one its commits move.
    /// Takes nothing. Returns the branch, or None for a detached HEAD.
    pub(super) fn branch(&self) -> Option<&str> {
        match &self.pointer {
            Pointer::Detached => None,
            Pointer::Branch(branch) => Some(branch),
        }
    }

    /// Reads the commit HEAD names from the git folder without following a link,
    /// refusing `changes_workspace_invalid` unless HEAD has the pointer's shape.
    /// Takes nothing. Returns the commit.
    pub(super) fn head(&self) -> Result<String, EngineError> {
        let bytes = bounded_read(&self.git_dir.join("HEAD"), HEAD_BYTES_MAX)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
        let commit = match &self.pointer {
            Pointer::Detached => detached(text)?.to_owned(),
            Pointer::Branch(branch) if text == format!("ref: refs/heads/{branch}\n") => self
                .repo
                .commit_of(&format!("refs/heads/{branch}"))?
                .ok_or_else(|| failure("changes_git_failed"))?,
            Pointer::Branch(_) => return Err(invalid()),
        };
        if self.repo.commit_of(&commit)?.as_deref() != Some(commit.as_str()) {
            return Err(invalid());
        }
        Ok(commit)
    }

    /// Tells whether the agent's own pointer holds a commit or work built on it, whichever
    /// branch HEAD names now: its branch, or HEAD itself for a detached agent. A commit the
    /// repository no longer holds counts as no. Input: the commit. Output: the answer.
    pub(super) fn built_on(&self, target: &str) -> Result<bool, EngineError> {
        let pointer = match &self.pointer {
            Pointer::Detached => "HEAD".to_owned(),
            Pointer::Branch(branch) => format!("refs/heads/{branch}"),
        };
        let Some(commit) = self.repo.commit_of(&pointer)? else {
            return Ok(false);
        };
        Ok(self.repo.commit_of(target)?.is_some()
            && self
                .repo
                .test(&["merge-base", "--is-ancestor", target, &commit])?)
    }

    /// Moves HEAD from `old`, when given, to `new`. A detached HEAD is rewritten by the
    /// engine under git's own `HEAD.lock` with no reflog; the master's lease branch is
    /// moved by its name in the shared repository with no dereference, after its reflog
    /// is checked to be a real file. Inputs: both commits and the reason. Output: Ok once
    /// moved, or `changes_git_failed` when HEAD changed meanwhile or is locked.
    pub(super) fn move_head(
        &self,
        old: Option<&str>,
        new: &str,
        message: &str,
    ) -> Result<(), EngineError> {
        assert!(is_object_id(new));
        let branch = match &self.pointer {
            Pointer::Detached => return move_detached(&self.git_dir, old, new),
            Pointer::Branch(branch) => format!("refs/heads/{branch}"),
        };
        let common = self.repository.join(".git");
        plain_log(&common, &branch)?;
        let env = [
            ("GIT_COMMITTER_NAME", ENGINE.0),
            ("GIT_COMMITTER_EMAIL", ENGINE.1),
        ];
        let mut args = vec![
            "update-ref",
            "--no-deref",
            "-m",
            message,
            branch.as_str(),
            new,
        ];
        args.extend(old);
        let moved = shared(&self.repository)?.exec(&args, None, &env, 256)?;
        if moved.code != 0 {
            return Err(failure("changes_git_failed"));
        }
        Ok(())
    }
}

/// Reads a detached HEAD's text as its object id. Input: the text. Output: the id, or
/// `changes_workspace_invalid` for a symbolic or malformed HEAD.
fn detached(text: &str) -> Result<&str, EngineError> {
    text.strip_suffix('\n')
        .filter(|id| is_object_id(id))
        .ok_or_else(invalid)
}

/// Rewrites a detached HEAD to another commit. Inputs: the git folder and both commits.
/// Output: Ok once moved, or `changes_git_failed` with the lock removed.
fn move_detached(git_dir: &Path, old: Option<&str>, new: &str) -> Result<(), EngineError> {
    replace_head(git_dir, old, &format!("{new}\n"))
}

/// Rewrites a detached HEAD the way git's own lock does, without git: `HEAD.lock` is
/// created only when absent and never through a link, HEAD is checked under it to still
/// be detached and hold `old`, then the lock is renamed over HEAD, which replaces a link
/// rather than following it. No reflog is written. Inputs: the git folder, the commit HEAD
/// must hold, None for any, and the new HEAD text. Output: Ok once written, or
/// `changes_git_failed` with the lock removed.
fn replace_head(git_dir: &Path, old: Option<&str>, text: &str) -> Result<(), EngineError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let lock = git_dir.join("HEAD.lock");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&lock)
        .map_err(|_| failure("changes_git_failed"))?;
    let git_failed = |_: std::io::Error| failure("changes_git_failed");
    let moved = bounded_read(&git_dir.join("HEAD"), HEAD_BYTES_MAX)
        .and_then(|bytes| {
            let text = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
            match detached(text)? {
                current if old.is_some_and(|old| old != current) => {
                    Err(failure("changes_git_failed"))
                }
                _ => Ok(()),
            }
        })
        .and_then(|()| {
            file.write_all(text.as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(git_failed)
        })
        .and_then(|()| std::fs::rename(&lock, git_dir.join("HEAD")).map_err(git_failed));
    if moved.is_err() {
        let _ = std::fs::remove_file(&lock);
    }
    moved
}

/// Refuses a reflog git would append to through a link: each folder on the way to
/// `logs/<reference>` in the git folder that exists must be a real folder and the log,
/// when present, a regular file. Inputs: the git folder and the reference. Output: Ok
/// when git appends only to a real file or, with `core.logAllRefUpdates` off, logs nothing.
fn plain_log(git_dir: &Path, reference: &str) -> Result<(), EngineError> {
    let mut path = git_dir.join("logs");
    let mut parts = reference.split('/');
    loop {
        let found = match std::fs::symlink_metadata(&path) {
            Ok(found) => found,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(failure("changes_git_failed")),
        };
        let Some(part) = parts.next() else {
            return if found.is_file() {
                Ok(())
            } else {
                Err(invalid())
            };
        };
        if !found.is_dir() || part.is_empty() || part == "." || part == ".." {
            return Err(invalid());
        }
        path.push(part);
    }
}

/// Opens a linked agent worktree for engine work through the git folder the engine
/// computed, with the HEAD shape its kind requires: detached or on its own branch
/// ([`agent_branch`]) for a sub-agent, the lease branch of its worktree for the master. An agent's own clone is never opened: its
/// config is the agent's, so the engine runs no git there but the fetch in
/// [`clone::fetch`]. Inputs: the store, the kind and the three paths. Output: the agent,
/// or `changes_workspace_invalid`, also for a clone.
pub(super) fn open_agent(
    db: &Connection,
    kind: AgentKind,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<Agent, EngineError> {
    if check_places(repository, worktree, git_dir)? != Layout::Linked {
        return Err(invalid());
    }
    if !std::fs::symlink_metadata(git_dir.join("HEAD")).is_ok_and(|found| found.is_file()) {
        return Err(invalid());
    }
    let pointer = match kind {
        AgentKind::Sub => match agent_branch(db, worktree, Layout::Linked)? {
            Some(branch)
                if bounded_read(&git_dir.join("HEAD"), HEAD_BYTES_MAX)?
                    == format!("ref: refs/heads/{branch}\n").as_bytes() =>
            {
                Pointer::Branch(branch)
            }
            _ => Pointer::Detached,
        },
        AgentKind::Master => crate::worktrees::lease_branch(db, worktree)
            .ok()
            .filter(|branch| branch.strip_prefix("crazyagent/").is_some_and(is_object_id))
            .map(Pointer::Branch)
            .ok_or_else(invalid)?,
    };
    Ok(Agent {
        repo: Repo::open(repository, git_dir, Some(worktree))?,
        repository: repository.to_path_buf(),
        git_dir: git_dir.to_path_buf(),
        pointer,
    })
}

/// Puts a newly added linked agent worktree on its own branch, the task branch stored for
/// it or else `agent_git::linked_branch`:
/// creates the branch at the worktree's detached HEAD in the shared repository, only when
/// no branch of that name exists, then points HEAD at it under git's own `HEAD.lock`.
/// The engine runs this git; the agent writes no git metadata. A worktree already on the
/// branch is left as it is. Inputs: the store and the three paths. Output: the branch, or
/// `changes_agent_branch_exists`, `changes_agent_branch_invalid` or `changes_git_failed`.
pub(super) fn name_branch(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<String, EngineError> {
    let _held = hold(repository);
    let branch = agent_branch(db, worktree, Layout::Linked)?
        .ok_or_else(|| failure("changes_agent_branch_invalid"))?;
    let agent = open_agent(db, AgentKind::Sub, repository, worktree, git_dir)?;
    let head = agent.head()?;
    if agent.branch() == Some(branch.as_str()) {
        return Ok(branch);
    }
    let reference = format!("refs/heads/{branch}");
    plain_log(&repository.join(".git"), &reference)?;
    let env = [
        ("GIT_COMMITTER_NAME", ENGINE.0),
        ("GIT_COMMITTER_EMAIL", ENGINE.1),
    ];
    let created = shared(repository)?.exec(
        &[
            "update-ref",
            "--no-deref",
            "-m",
            "CrazyAgent: agent branch",
            &reference,
            &head,
            "",
        ],
        None,
        &env,
        256,
    )?;
    if created.code != 0 {
        return Err(failure("changes_agent_branch_exists"));
    }
    replace_head(git_dir, Some(&head), &format!("ref: {reference}\n"))?;
    let named = open_agent(db, AgentKind::Sub, repository, worktree, git_dir)?;
    assert_eq!(named.branch(), Some(branch.as_str()));
    assert_eq!(named.head()?, head);
    Ok(branch)
}

/// Names the task branch a new harness works on (DEC-100), `crazyagent/<name>`, or the
/// first free `-2` to `-64` form when another agent worktree of the repository or an owner
/// branch already holds that name, and stores it for the worktree, so its clone or linked
/// worktree commits there. The repository hold keeps two launches from taking one name.
/// Inputs: the store, the owner checkout root, the new worktree and the name, as
/// `agent_git::task_branch` takes it. Output: the branch, or `changes_agent_branch_invalid`
/// for a name no branch can take, or `changes_agent_branch_exists` when every form is
/// taken.
pub(super) fn claim_branch(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    name: &str,
) -> Result<String, EngineError> {
    store::initialize(db)?;
    let _held = hold(repository);
    let shared = shared(repository)?;
    for suffix in 1..=TASK_SUFFIXES_MAX {
        let candidate = match suffix {
            1 => name.to_owned(),
            _ => format!("{name}-{suffix}"),
        };
        let branch = agent_git::task_branch(&candidate)
            .ok_or_else(|| failure("changes_agent_branch_invalid"))?;
        let reference = format!("refs/heads/{branch}");
        let nested = shared.capture(
            &[
                "for-each-ref",
                "--count=1",
                "--format=%(refname)",
                &format!("{reference}/"),
            ],
            LINK_BYTES_MAX,
        )?;
        if store::agent_branch_taken(db, repository, worktree, &branch)?
            || shared.commit_of(&reference)?.is_some()
            || !nested.is_empty()
        {
            continue;
        }
        store::set_agent_branch(db, repository, worktree, &branch)?;
        return Ok(branch);
    }
    Err(failure("changes_agent_branch_exists"))
}

/// The branch an agent worktree commits on: the task branch the engine stored for it
/// (DEC-100), else the one its folder names for its layout, `agent_git::branch` for its
/// own clone and `agent_git::linked_branch` for a linked worktree. Inputs: the store, the
/// worktree and its layout. Output: the branch, None for a folder that names none.
pub(super) fn agent_branch(
    db: &Connection,
    worktree: &Path,
    layout: Layout,
) -> Result<Option<String>, EngineError> {
    let path = worktree.to_str().ok_or_else(invalid)?;
    if let Some(stored) = store::agent_branch(db, path)? {
        return Ok(Some(stored));
    }
    Ok(match layout {
        Layout::Clone => agent_git::branch(worktree),
        Layout::Linked => agent_git::linked_branch(worktree),
    })
}

/// The branch an agent's own clone commits on, as [`agent_branch`] reads it. Inputs: the
/// store and the worktree. Output: the branch, or `changes_workspace_invalid`.
pub(super) fn clone_branch(db: &Connection, worktree: &Path) -> Result<String, EngineError> {
    agent_branch(db, worktree, Layout::Clone)?.ok_or_else(invalid)
}

/// Deletes an agent's own branch once its worktree is gone, only while the branch still
/// names the commit the worktree was retired at, so a branch moved since keeps its work.
/// Inputs: the owner's checkout root, the branch and that commit. Output: true when
/// deleted, false when the branch had moved or was already gone.
pub(super) fn drop_branch(
    repository: &Path,
    branch: &str,
    commit: &str,
) -> Result<bool, EngineError> {
    assert!(is_object_id(commit));
    let reference = format!("refs/heads/{branch}");
    plain_log(&repository.join(".git"), &reference)?;
    let deleted = shared(repository)?.exec(
        &["update-ref", "--no-deref", "-d", &reference, commit],
        None,
        &[],
        256,
    )?;
    Ok(deleted.code == 0)
}

/// Reads the git folder a linked worktree's `.git` file names, as a plain file the engine's
/// own `git worktree add` wrote. Input: the worktree. Output: the folder, unchecked; the
/// caller checks it with [`check_places`], or `changes_workspace_invalid`.
pub(super) fn linked_git_dir(worktree: &Path) -> Result<PathBuf, EngineError> {
    let link = bounded_read(&worktree.join(".git"), LINK_BYTES_MAX)?;
    let text = std::str::from_utf8(&link).map_err(|_| invalid())?;
    text.strip_prefix("gitdir: ")
        .and_then(|path| path.strip_suffix('\n'))
        .map(PathBuf::from)
        .ok_or_else(invalid)
}

/// Opens the agent worktree of a change set through the git folder the engine recorded
/// for it. Inputs: the store and the change set. Output: the agent.
pub(super) fn agent(db: &Connection, record: &Record) -> Result<Agent, EngineError> {
    open_agent(
        db,
        record.agent.kind,
        &record.repository,
        &record.worktree,
        &record.git_dir,
    )
}

/// Says whether an agent worktree holds exactly a commit: nothing staged or changed in
/// tracked files and no untracked file outside its ignore rules. A submodule counts by
/// the commit it records, never by its own content, so no git runs inside it under
/// config the engine did not write. Inputs: the worktree and the commit. Output: true
/// when it holds nothing else.
pub(super) fn holds_only(repo: &Repo, commit: &str) -> Result<bool, EngineError> {
    holds_only_except(repo, commit, &[])
}

/// Says whether an agent worktree holds exactly a commit as [`holds_only`] does, leaving
/// more patterns out of its untracked files, such as a harness clone's teammates' worktree
/// folder, which cleanup checks on its own, or the code graph's copy, which is never the
/// agent's work. Inputs: the worktree, the commit and the patterns, in git's `--exclude`
/// form. Output: the answer.
pub(super) fn holds_only_except(
    repo: &Repo,
    commit: &str,
    excluded: &[&str],
) -> Result<bool, EngineError> {
    repo.run(&["update-index", "-q", "--refresh"])?;
    if !repo.test(&[
        "diff-index",
        "--quiet",
        "--ignore-submodules=all",
        commit,
        "--",
    ])? {
        return Ok(false);
    }
    let exclude: Vec<String> = excluded
        .iter()
        .map(|pattern| format!("--exclude={pattern}"))
        .collect();
    let mut listing = vec!["ls-files", "-z", "--others", "--exclude-standard"];
    listing.extend(exclude.iter().map(String::as_str));
    let untracked = repo.exec(&listing, None, &[], 1)?;
    if untracked.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    Ok(untracked.stdout.is_empty())
}

/// An edit a test plants for [`reset_agent`] to make, the way an agent writes.
#[cfg(test)]
pub(super) type PlantedEdit = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Test seam (RESET-KEEP): edits [`reset_agent`] makes first for an agent git folder, on
/// whichever thread runs it, after its caller checked the worktree, the way an agent can
/// write in that window.
#[cfg(test)]
pub(super) static BEFORE_RESET: std::sync::Mutex<Vec<(PathBuf, PlantedEdit)>> =
    std::sync::Mutex::new(Vec::new());

/// Test seam (land-2 r2): edits [`reset_agent`] makes after it checked HEAD and refreshed
/// the index and before its two-tree read-tree, the way an agent can write in that window.
#[cfg(test)]
pub(super) static BEFORE_READ_TREE: std::sync::Mutex<Vec<(PathBuf, PlantedEdit)>> =
    std::sync::Mutex::new(Vec::new());

/// Test seam (RESET-KEEP r2): edits [`reset_agent`] makes right after its two-tree
/// read-tree moved the files and before HEAD moves, the way an agent can write in that
/// window.
#[cfg(test)]
pub(super) static AFTER_READ_TREE: std::sync::Mutex<Vec<(PathBuf, PlantedEdit)>> =
    std::sync::Mutex::new(Vec::new());

/// Test seam (RK-R2-S1): edits [`reset_agent`] makes right after HEAD moved and before it
/// reads HEAD again, the way an agent can write in that window.
#[cfg(test)]
pub(super) static AFTER_HEAD_MOVE: std::sync::Mutex<Vec<(PathBuf, PlantedEdit)>> =
    std::sync::Mutex::new(Vec::new());

/// Runs the edit a test planted in `planted` for this agent's git folder, if any. Inputs:
/// the planted edits and the agent. Output: none.
#[cfg(test)]
fn run_planted(planted: &std::sync::Mutex<Vec<(PathBuf, PlantedEdit)>>, agent: &Agent) {
    let same = |planted: &Path| {
        std::fs::canonicalize(planted)
            .ok()
            .is_some_and(|planted| std::fs::canonicalize(&agent.git_dir).ok() == Some(planted))
    };
    let edit = planted
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(git_dir, _)| same(git_dir))
        .map(|(_, edit)| std::sync::Arc::clone(edit));
    if let Some(edit) = edit {
        edit();
    }
}

/// The refusal [`reset_agent`] gives when the agent's pointer moved on from a reset it
/// reached. The owner reads it as the agent still writing, which it is.
pub(super) const POINTER_MOVED_ON: &str = "changes_agent_busy";

/// Moves an agent worktree from the commit it holds to another, never over the agent's
/// work: an index git cannot refresh (locked, mid-merge) refuses; git's two-tree
/// read-tree moves its index and tracked files and refuses, changing nothing, when a path
/// it would change was edited, staged or is in its way untracked; then HEAD moves only
/// while it still holds `from` and must still be the agent's pointer after. When HEAD did
/// not move, the files go back unless the agent committed them meanwhile; when the pointer
/// left, HEAD goes back and the files too when the agent's checkout holds `from`. A stop
/// between the steps leaves a state an engine start can finish. Inputs: the worktree and
/// both commits.
/// Output: true once it holds `target`, false when it refused, [`POINTER_MOVED_ON`] when
/// the pointer cannot go back, so the caller leaves the step stored and undoes nothing.
pub(super) fn reset_agent(agent: &Agent, from: &str, target: &str) -> Result<bool, EngineError> {
    assert!(is_object_id(from) && is_object_id(target));
    #[cfg(test)]
    run_planted(&BEFORE_RESET, agent);
    if agent.head()? != from {
        return Ok(false);
    }
    let refreshed = agent
        .repo
        .exec(&["update-index", "-q", "--refresh"], None, &[], 4096)?;
    if refreshed.code != 0 {
        return Ok(false);
    }
    #[cfg(test)]
    run_planted(&BEFORE_READ_TREE, agent);
    let moved = agent.repo.confined(Writes::WorkTree)?.exec(
        &["read-tree", "-u", "-m", from, target],
        None,
        &[],
        4096,
    )?;
    if moved.code != 0 {
        return Ok(false);
    }
    #[cfg(test)]
    run_planted(&AFTER_READ_TREE, agent);
    if agent
        .move_head(Some(from), target, "CrazyAgent: reset")
        .is_err()
    {
        files_back(agent, from, target);
        return Ok(false);
    }
    #[cfg(test)]
    run_planted(&AFTER_HEAD_MOVE, agent);
    if agent.head().is_err() {
        eprintln!("landing_reset_head_left_its_pointer");
        if agent
            .move_head(Some(target), from, "CrazyAgent: reset back")
            .is_err()
        {
            eprintln!("landing_reset_pointer_not_put_back");
            return Err(failure(POINTER_MOVED_ON));
        }
        if agent.repo.commit_of("HEAD").ok().flatten().as_deref() == Some(from) {
            files_back(agent, from, target);
        }
        return Ok(false);
    }
    Ok(true)
}

/// As [`reset_agent`], for a caller with nobody left to refuse: a pointer that moved on
/// from the reset counts as done, because the agent built on what the reset reached.
/// Inputs: the worktree and both commits. Output: true once it holds `target` or moved on
/// from it, false when it refused.
pub(super) fn reset_settling(agent: &Agent, from: &str, target: &str) -> Result<bool, EngineError> {
    match reset_agent(agent, from, target) {
        Err(moved) if moved.0 == POINTER_MOVED_ON => Ok(true),
        other => other,
    }
}

/// Puts an agent worktree's index and files back from `target` to `from` after its reset
/// did not hold, with the same two-tree read-tree, so an edit the agent made meanwhile is
/// never overwritten: git refuses, writing nothing, and the refusal is logged. An index
/// equal to HEAD stays: the agent committed what the reset moved, and putting it back would
/// stage a reversal of that commit. Inputs: the worktree and both commits. Output: none.
fn files_back(agent: &Agent, from: &str, target: &str) {
    let committed = agent.repo.exec(
        &["diff-index", "--quiet", "--cached", "HEAD", "--"],
        None,
        &[],
        4096,
    );
    if committed.is_ok_and(|committed| committed.code == 0) {
        return;
    }
    let back = agent
        .repo
        .confined(Writes::WorkTree)
        .and_then(|repo| repo.exec(&["read-tree", "-u", "-m", target, from], None, &[], 4096));
    if !back.is_ok_and(|back| back.code == 0) {
        eprintln!("landing_reset_files_back_refused");
    }
}

/// Records a reset of the agent's worktree to `target` after its change set closed, runs
/// it and clears the record, or skips it when the worktree holds more than `expected` or
/// the agent writes while it runs, so the agent's newer work stays; the worktree's next
/// change set opens above whichever commit it now starts from. Inputs: the store, the closed change set and both commits.
/// Output: Ok once saved.
pub(super) fn reset_if_quiet(
    db: &Connection,
    record: &mut Record,
    expected: &str,
    target: &str,
) -> Result<(), EngineError> {
    assert!(!record.is_open());
    let agent = if in_clone(record) {
        None
    } else {
        match agent(db, record) {
            Err(refused) if refused.0 == "changes_workspace_invalid" => None,
            opened => Some(opened?),
        }
    };
    let quiet = match &agent {
        Some(agent) => {
            agent.head().ok().as_deref() == Some(expected) && holds_only(&agent.repo, expected)?
        }
        None => false,
    };
    let (true, Some(agent)) = (quiet, agent) else {
        record.intent = None;
        store::save(db, record)?;
        return set_base(
            db,
            &record.repository,
            &record.worktree,
            &record.git_dir,
            expected,
        );
    };
    record.intent = Some(Intent::Reset {
        from: Some(expected.to_owned()),
        target: target.to_owned(),
        earlier: None,
    });
    store::save(db, record)?;
    let start = if reset_settling(&agent, expected, target)? {
        target
    } else {
        expected
    };
    set_base(
        db,
        &record.repository,
        &record.worktree,
        &record.git_dir,
        start,
    )?;
    record.intent = None;
    store::save(db, record)
}

/// The pathspec that leaves the teammates' folder at the top of an agent worktree out of
/// what the engine lists and commits there (OP-9b): each native teammate's own worktree
/// sits in `.worktrees/<name>` and lands through its own branch, never as an embedded
/// repository in its lead's turn. It is a glob over everything below the folder whose
/// first character is a one-character class, so it has no literal prefix: git then never
/// reports the folder as an ignored path named on the command line and fails the add when
/// an ignore rule hides it, and it still leaves out what the index tracks there. Output:
/// the pathspec.
fn teammates_left_out() -> String {
    let (first, rest) = agent_git::TEAMMATES_FOLDER.split_at(1);
    format!(":(exclude,top,glob)[{first}]{rest}/**")
}

/// Refuses a change past the bounds before git stores any of it: too many changed
/// paths, a path too deep, too long or not UTF-8, or too many bytes in the files.
/// Inputs: the worktree, its root and the bounds. Output: Ok within the bounds.
fn check_bounds(repo: &Repo, worktree: &Path, bounds: &TreeBounds) -> Result<(), EngineError> {
    let teammates = teammates_left_out();
    let output = repo.exec(
        &[
            "ls-files",
            "-z",
            "--modified",
            "--others",
            "--deleted",
            "--exclude-standard",
            "--",
            ":(top)",
            &teammates,
        ],
        None,
        &[],
        LISTING_BYTES_MAX,
    )?;
    if output.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    if output.overflow {
        return Err(failure("changes_too_large"));
    }
    let paths: BTreeSet<&[u8]> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .collect();
    if paths.len() > bounds.files {
        return Err(failure("changes_too_large"));
    }
    let mut bytes: u64 = 0;
    for path in paths {
        let path = std::str::from_utf8(path).map_err(|_| failure("changes_git_unsupported"))?;
        if path.len() > PATH_BYTES_MAX || path.split('/').count() > bounds.components {
            return Err(failure("changes_too_large"));
        }
        if let Ok(metadata) = std::fs::symlink_metadata(worktree.join(path)) {
            bytes = bytes.saturating_add(metadata.len());
        }
        if bytes > bounds.bytes {
            return Err(failure("changes_too_large"));
        }
    }
    assert!(bytes <= bounds.bytes);
    Ok(())
}

/// Refuses a change set past the bounds `check_bounds` applies to a worktree, whatever
/// put the change in git: too many changed paths, a path too deep, too long or not
/// UTF-8, too many bytes in the changed files, a file added since `fresh` that the
/// owner's ignore rules exclude, which `add -A` never takes, a gitlink (mode 160000)
/// that `add -A` recorded from an embedded repository, which no lease tree starts with
/// and the owner's own git would enter unsandboxed (SM1-R backstop to the sandbox deny),
/// a folder the change makes into a layout git would open as a bare repository
/// (`repository_layout`, SM-R1), or a link pointing outside the tree or into the agents'
/// folder (`check_links`, SR1-2). The bounds count the whole range from `from`; the gitlink,
/// layout, link and ignore checks read only the agent's own work from `owned`, so owner work
/// the agent took in from the landing tip is not refused as its own (LAND-17-L1).
/// Inputs: the repository, the owner's checkout, the trees the set spans, the commit its own
/// work starts from, the tree its newest part starts from and the bounds. Output: Ok within
/// the bounds.
fn check_change(
    repo: &Repo,
    checkout: &Path,
    from: &str,
    owned: &str,
    to: &str,
    fresh: &str,
    bounds: &TreeBounds,
) -> Result<(), EngineError> {
    let listing = changes_listing(repo, from, to)?;
    let whole = changed_entries(&listing)?;
    let mut blobs = Vec::new();
    for (count, &(mode, id, _, path)) in whole.iter().enumerate() {
        if count >= bounds.files {
            return Err(failure("changes_too_large"));
        }
        let path = std::str::from_utf8(path).map_err(|_| failure("changes_git_unsupported"))?;
        if path.len() > PATH_BYTES_MAX || path.split('/').count() > bounds.components {
            return Err(failure("changes_too_large"));
        }
        if matches!(mode, "100644" | "100755" | "120000") {
            blobs.push(id);
        }
    }
    assert!(whole.len() <= bounds.files && blobs.len() <= whole.len());
    let own_listing = if owned == from {
        Vec::new()
    } else {
        changes_listing(repo, owned, to)?
    };
    let own = if owned == from {
        whole
    } else {
        changed_entries(&own_listing)?
    };
    let mut added = Vec::new();
    let (mut folders, mut links): (BTreeSet<String>, BTreeSet<String>) =
        (BTreeSet::new(), BTreeSet::new());
    for &(mode, _, status, path) in &own {
        if mode == "160000" {
            return Err(failure("changes_embedded_repository"));
        }
        let path = std::str::from_utf8(path).map_err(|_| failure("changes_git_unsupported"))?;
        if status == "A" {
            added.push(path);
        }
        if status != "D" {
            layout_folders(&mut folders, path);
            if mode == "120000" {
                links.insert(path.to_owned());
            }
        }
    }
    assert!(added.len() <= own.len() && links.len() <= own.len());
    if repository_layout(repo, to, &folders)? {
        return Err(failure("changes_bare_repository"));
    }
    check_links(repo, checkout, to, &links)?;
    if blob_bytes(repo, &blobs)? > bounds.bytes {
        return Err(failure("changes_too_large"));
    }
    if fresh != owned && !added.is_empty() {
        let since = repo.run(&[
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--name-only",
            "--diff-filter=A",
            fresh,
            to,
        ])?;
        let since: BTreeSet<&[u8]> = since.split(|byte| *byte == 0).collect();
        added.retain(|path| since.contains(path.as_bytes()));
    }
    if ignores_any(checkout, &added)? {
        return Err(failure("changes_ignored_files"));
    }
    Ok(())
}

/// Lists what changed between two trees, `diff-tree -r -z --no-renames`. Inputs: the
/// repository and the two trees. Output: the listing, `changes_git_failed` when git fails,
/// or `changes_too_large` past `LISTING_BYTES_MAX`.
fn changes_listing(repo: &Repo, from: &str, to: &str) -> Result<Vec<u8>, EngineError> {
    let output = repo.exec(
        &["diff-tree", "-r", "-z", "--no-renames", from, to],
        None,
        &[],
        LISTING_BYTES_MAX,
    )?;
    if output.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    if output.overflow {
        return Err(failure("changes_too_large"));
    }
    Ok(output.stdout)
}

/// One entry of a `changes_listing`: the changed path's new mode, new object, status
/// letter and path.
type Changed<'a> = (&'a str, &'a str, &'a str, &'a [u8]);

/// The entries of a `changes_listing`. Input: the listing. Output: the entries, or
/// `changes_git_failed` for a field git would not write.
fn changed_entries(listing: &[u8]) -> Result<Vec<Changed<'_>>, EngineError> {
    let mut fields = listing.split(|byte| *byte == 0);
    let mut entries = Vec::new();
    while let Some(header) = fields.next().filter(|header| !header.is_empty()) {
        let path = fields.next().ok_or_else(|| failure("changes_git_failed"))?;
        let header = std::str::from_utf8(header).map_err(|_| failure("changes_git_failed"))?;
        let parts: Vec<&str> = header
            .strip_prefix(':')
            .unwrap_or_default()
            .split(' ')
            .collect();
        let [_, mode, _, id, status] = parts[..] else {
            return Err(failure("changes_git_failed"));
        };
        entries.push((mode, id, status, path));
    }
    Ok(entries)
}

/// Refuses a change set in which one new top-level folder, one the owner's branch does
/// not have, gains more files than the owner's limit (DEC-107 rule 3), such as a scratch
/// folder an agent committed. Inputs: the repository, the commit whose top names count as
/// the owner's, the commit the set is measured from, its head and the limit. Output: Ok,
/// else [`super::NEW_FOLDER_REFUSED`] carrying the folder, its file count and the limit
/// for the agent, as [`super::refusal_for_agent`] reads them.
fn check_new_folders(
    repo: &Repo,
    owner: &str,
    from: &str,
    to: &str,
    limit: u32,
) -> Result<(), EngineError> {
    if from == to {
        return Ok(());
    }
    let output = repo.exec(
        &[
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--name-only",
            "--diff-filter=A",
            from,
            to,
        ],
        None,
        &[],
        LISTING_BYTES_MAX,
    )?;
    if output.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    if output.overflow {
        return Err(failure("changes_too_large"));
    }
    let mut added: BTreeMap<&[u8], u32> = BTreeMap::new();
    for path in output.stdout.split(|byte| *byte == 0) {
        if let Some(at) = path.iter().position(|byte| *byte == b'/') {
            let files = added.entry(&path[..at]).or_default();
            *files = files.saturating_add(1);
        }
    }
    added.retain(|_, files| *files > limit);
    if added.is_empty() {
        return Ok(());
    }
    let listed = repo.run(&["ls-tree", "-z", "--name-only", owner])?;
    let kept: BTreeSet<&[u8]> = listed
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .collect();
    let Some((folder, files)) = added
        .into_iter()
        .filter(|(folder, _)| !kept.contains(folder))
        .max_by_key(|(_, files)| *files)
    else {
        return Ok(());
    };
    assert!(files > limit);
    let name: String = plain(&String::from_utf8_lossy(folder))
        .replace(['\n', '\t'], "\u{FFFD}")
        .chars()
        .take(FOLDER_CHARS_MAX)
        .collect();
    Err(EngineError(format!(
        "{}\n{files}\n{limit}\n{name}",
        super::NEW_FOLDER_REFUSED
    )))
}

/// The most files one new top-level folder may gain in a project's change set: the owner's
/// Team setting, else its default where the engine keeps no team tables. Inputs: the store
/// and the project. Output: the limit.
fn new_folder_limit(db: &Connection, project: &ProjectId) -> Result<u32, EngineError> {
    let kept: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='team_new_folder_limit')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| failure("changes_unavailable"))?;
    if !kept {
        return Ok(crate::team::NEW_FOLDER_FILES_DEFAULT);
    }
    crate::team::new_folder_files_max(db, project)
}

/// Adds the folders above each component of a path named like a part of a git folder
/// (`LAYOUT_NAMES`, any case): the folders `repository_layout` lists.
/// Inputs: the set and one added or changed path.
fn layout_folders(folders: &mut BTreeSet<String>, path: &str) {
    let names: Vec<&str> = path.split('/').collect();
    for (at, name) in names.iter().enumerate() {
        if LAYOUT_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
            folders.insert(names[..at].join("/"));
        }
    }
}

/// Refuses a merged tree in which a change adds or changes a gitlink, which no set may land
/// and the turn end's checks may have read from another start (land-19 r1 M1, L1), completes
/// a bare-repository layout (SR1-1) or links the owner could follow out of the project
/// (SR1-2): Accept's merge can join a
/// change set's half of a layout or of a link chain with the other half another change set
/// already landed. A layout the merge makes holds a name the change added or changed, and a
/// chain the merge makes passes through a link the change added or changed, so the
/// change's own kept paths name every folder and link to check.
/// Inputs: the repository, the owner's checkout, the change's start and end commits and
/// the merged tree. Output: Ok, else `changes_embedded_repository`,
/// `changes_bare_repository`, `changes_link_outside` or the listing failure.
pub(super) fn check_merged(
    repo: &Repo,
    checkout: &Path,
    from: &str,
    to: &str,
    tree: &str,
) -> Result<(), EngineError> {
    let output = repo.exec(
        &[
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--diff-filter=d",
            from,
            to,
        ],
        None,
        &[],
        LISTING_BYTES_MAX,
    )?;
    if output.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    if output.overflow {
        return Err(failure("changes_too_large"));
    }
    let (mut folders, mut links) = (BTreeSet::new(), BTreeSet::new());
    let mut fields = output.stdout.split(|byte| *byte == 0);
    while let Some(header) = fields.next().filter(|header| !header.is_empty()) {
        let path = fields.next().ok_or_else(|| failure("changes_git_failed"))?;
        let path = std::str::from_utf8(path).map_err(|_| failure("changes_git_unsupported"))?;
        let mode = header.split(|byte| *byte == b' ').nth(1);
        if mode == Some(b"160000".as_slice()) {
            return Err(failure("changes_embedded_repository"));
        }
        layout_folders(&mut folders, path);
        if mode == Some(b"120000".as_slice()) {
            links.insert(path.to_owned());
        }
    }
    if repository_layout(repo, tree, &folders)? {
        return Err(failure("changes_bare_repository"));
    }
    check_links(repo, checkout, tree, &links)
}

/// Tells whether a tree holds a folder git would open as a bare repository (SM-R1): a
/// `HEAD` file or link beside a `commondir`, or beside both `objects` and `refs`, each
/// name in any case, `objects` and `refs` of any type since git takes an executable file
/// for either folder. The owner's own git run in that folder would read its config.
/// Inputs: the repository, the tree and the folders a change put such a name in.
/// Output: the answer, `changes_too_large` past `LAYOUT_FOLDERS_MAX` folders, else
/// `changes_git_failed` when git cannot list one.
fn repository_layout(
    repo: &Repo,
    tree: &str,
    folders: &BTreeSet<String>,
) -> Result<bool, EngineError> {
    if folders.len() > LAYOUT_FOLDERS_MAX {
        return Err(failure("changes_too_large"));
    }
    for folder in folders {
        let spec = if folder.is_empty() {
            tree.to_owned()
        } else {
            format!("{tree}:{folder}")
        };
        let output = repo.exec(&["ls-tree", "-z", &spec], None, &[], LISTING_BYTES_MAX)?;
        if output.code != 0 || output.overflow {
            return Err(failure("changes_git_failed"));
        }
        let mut entries: Vec<(String, bool)> = Vec::new();
        for entry in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let tab = entry
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(|| failure("changes_git_failed"))?;
            let file = !entry[..tab].starts_with(b"040000 ");
            entries.push((
                String::from_utf8_lossy(&entry[tab + 1..]).to_ascii_lowercase(),
                file,
            ));
        }
        let has = |name: &str| entries.iter().any(|(entry, _)| entry == name);
        let head = entries.iter().any(|(entry, file)| entry == "head" && *file);
        if head && (has("commondir") || (has("objects") && has("refs"))) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Refuses a tree whose links an owner could follow out of the project or into a folder an
/// agent writes (SR1-2): the agents' worktrees folder, such as to a bare-repository layout
/// in an ignored folder of an agent's worktree, which no landing check reads, and the
/// owner's `.git`, where a Linked agent writes `.git/worktrees/<id>` and `.git/objects`.
/// Each link the change added or changed needs an ASCII path and a relative, UTF-8 target
/// of at most `PATH_BYTES_MAX` bytes with ASCII names and `..` only before its first name;
/// resolved from the link's folder it stays in the tree, does not start with the agents'
/// folder, steps into no `.git` in any ASCII case, and neither its folders nor any step of
/// it is a link of the tree in any ASCII case. No other link of the tree may pass through
/// or end at one of the change's links. A tree link path or step that is not ASCII
/// matches any name the volume could fold it onto. Inputs: the repository, the owner's
/// checkout, the tree that lands and the paths of the change's links. Output: Ok, else
/// `changes_link_outside`, `changes_too_large` or `changes_git_failed`.
fn check_links(
    repo: &Repo,
    checkout: &Path,
    tree: &str,
    changed: &BTreeSet<String>,
) -> Result<(), EngineError> {
    if changed.is_empty() {
        return Ok(());
    }
    if !changed.iter().all(|path| path.is_ascii()) {
        return Err(failure("changes_link_outside"));
    }
    let links = tree_links(repo, tree)?;
    let every: BTreeSet<String> = links
        .iter()
        .map(|(path, _)| path.to_ascii_lowercase())
        .collect();
    let fresh: BTreeSet<String> = changed
        .iter()
        .map(|path| path.to_ascii_lowercase())
        .collect();
    if !fresh.is_subset(&every) {
        return Err(failure("changes_git_failed"));
    }
    let folded: Vec<&str> = every
        .iter()
        .filter(|path| !path.is_ascii())
        .map(String::as_str)
        .collect();
    let agents = agents_root(checkout);
    let ids: Vec<&str> = links.iter().map(|(_, id)| id.as_str()).collect();
    let targets = link_targets(repo, &ids)?;
    assert_eq!(targets.len(), links.len());
    for ((path, _), target) in links.iter().zip(&targets) {
        let outside = if fresh.contains(&path.to_ascii_lowercase()) {
            !fresh_link_inside(path, target.as_deref(), &agents, &every, &folded)
        } else {
            target
                .as_deref()
                .is_some_and(|target| passes_through(path, target, &fresh))
        };
        if outside {
            return Err(failure("changes_link_outside"));
        }
    }
    Ok(())
}

/// Lists every link of a tree with its blob id, from one bounded `ls-tree -r`.
/// Inputs: the repository and the tree. Output: the paths and ids, else
/// `changes_too_large` or `changes_git_failed`.
fn tree_links(repo: &Repo, tree: &str) -> Result<Vec<(String, String)>, EngineError> {
    let output = repo.exec(&["ls-tree", "-r", "-z", tree], None, &[], LISTING_BYTES_MAX)?;
    if output.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    if output.overflow {
        return Err(failure("changes_too_large"));
    }
    let broken = || failure("changes_git_failed");
    let mut links = Vec::new();
    for entry in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(broken)?;
        let header = std::str::from_utf8(&entry[..tab]).map_err(|_| broken())?;
        let [mode, _, id] = header.split(' ').collect::<Vec<_>>()[..] else {
            return Err(broken());
        };
        if mode == "120000" {
            let path = String::from_utf8_lossy(&entry[tab + 1..]).into_owned();
            links.push((path, id.to_owned()));
        }
    }
    Ok(links)
}

/// Reads each link's target with one `cat-file --batch`, none for a target longer than
/// `PATH_BYTES_MAX`, whose size `blob_sizes` reads first. Inputs: the repository and the
/// link blob ids. Output: the targets in order, else `changes_git_failed`.
fn link_targets(repo: &Repo, ids: &[&str]) -> Result<Vec<Option<Vec<u8>>>, EngineError> {
    let sizes = blob_sizes(repo, ids)?;
    let short: Vec<(&str, usize)> = ids
        .iter()
        .zip(&sizes)
        .filter_map(|(id, size)| {
            usize::try_from(*size)
                .ok()
                .filter(|size| *size <= PATH_BYTES_MAX)
                .map(|size| (*id, size))
        })
        .collect();
    let mut read: Vec<Vec<u8>> = Vec::new();
    if !short.is_empty() {
        let input: String = short.iter().map(|(id, _)| format!("{id}\n")).collect();
        let limit = short
            .iter()
            .map(|(_, size)| size + LINK_HEADER_BYTES_MAX)
            .sum::<usize>();
        let output = repo.exec(&["cat-file", "--batch"], Some(input.as_bytes()), &[], limit)?;
        if output.code != 0 || output.overflow {
            return Err(failure("changes_git_failed"));
        }
        let broken = || failure("changes_git_failed");
        let mut rest: &[u8] = &output.stdout;
        for (id, size) in &short {
            let end = rest
                .iter()
                .position(|byte| *byte == b'\n')
                .ok_or_else(broken)?;
            let header = std::str::from_utf8(&rest[..end]).map_err(|_| broken())?;
            let wanted = format!("{id} blob {size}");
            if header != wanted {
                return Err(broken());
            }
            read.push(
                rest.get(end + 1..end + 1 + size)
                    .ok_or_else(broken)?
                    .to_vec(),
            );
            rest = rest.get(end + 2 + size..).ok_or_else(broken)?;
        }
        if !rest.is_empty() {
            return Err(broken());
        }
    }
    let mut read = read.into_iter();
    let targets: Vec<Option<Vec<u8>>> = sizes
        .iter()
        .map(|size| {
            (*size <= PATH_BYTES_MAX as u64)
                .then(|| read.next())
                .flatten()
        })
        .collect();
    assert!(read.next().is_none());
    Ok(targets)
}

/// The agents' worktrees folder as path parts from the owner's checkout, lowercased:
/// `.crazyagent/worktrees`, or `worktrees` for a project made before the projects home.
/// Input: the checkout. Output: the parts.
fn agents_root(checkout: &Path) -> Vec<String> {
    let folder = crate::projects_home::state_folder(checkout).join("worktrees");
    let parts: Vec<String> = folder
        .strip_prefix(checkout)
        .expect("the state folder sits in the checkout")
        .components()
        .map(|part| part.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    assert!(!parts.is_empty() && parts.len() <= 2);
    parts
}

/// Tells whether one of the change's links stays inside under [`check_links`]'s rule.
/// Inputs: the link's path, its target (none when too long), the agents' folder parts,
/// every link path of the tree in ASCII lower case and those of them that are not ASCII.
/// Output: true when it stays inside.
fn fresh_link_inside(
    path: &str,
    target: Option<&[u8]>,
    agents: &[String],
    links: &BTreeSet<String>,
    folded: &[&str],
) -> bool {
    let Some(Ok(target)) = target.map(std::str::from_utf8) else {
        return false;
    };
    if target.is_empty() || target.starts_with('/') {
        return false;
    }
    let mut walk: Vec<&str> = path.split('/').collect();
    walk.pop();
    let link = |walk: &[&str]| {
        links.contains(&walk.join("/").to_ascii_lowercase())
            || folded.iter().any(|name| folds_onto(name, walk))
    };
    if (1..=walk.len()).any(|end| link(&walk[..end])) {
        return false;
    }
    let mut named = false;
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." if named => return false,
            ".." => {
                if walk.pop().is_none() {
                    return false;
                }
            }
            name if !name.is_ascii() => return false,
            name => {
                walk.push(name);
                named = true;
                if link(&walk) {
                    return false;
                }
            }
        }
    }
    !entered(&walk, agents) && !walk.iter().any(|part| part.eq_ignore_ascii_case(".git"))
}

/// The code points outside ASCII that a case-insensitive APFS volume folds onto ASCII,
/// with the ASCII they fold onto: the full case fold of their canonical decomposition,
/// each checked on the volume (probe 2026-10-28). Every other code point stays outside ASCII.
const ASCII_FOLDS: [(char, &str); 13] = [
    ('\u{DF}', "ss"),
    ('\u{17F}', "s"),
    ('\u{37E}', ";"),
    ('\u{1E9E}', "ss"),
    ('\u{1FEF}', "`"),
    ('\u{212A}', "k"),
    ('\u{FB00}', "ff"),
    ('\u{FB01}', "fi"),
    ('\u{FB02}', "fl"),
    ('\u{FB03}', "ffi"),
    ('\u{FB04}', "ffl"),
    ('\u{FB05}', "st"),
    ('\u{FB06}', "st"),
];

/// Folds one path part the way the volume compares names, when the result is ASCII.
/// Input: the part. Output: the part in ASCII lower case with [`ASCII_FOLDS`] applied, or
/// None when a code point stays outside ASCII.
fn ascii_fold(part: &str) -> Option<String> {
    let mut folded = String::with_capacity(part.len());
    for point in part.chars() {
        if point.is_ascii() {
            folded.push(point.to_ascii_lowercase());
        } else {
            let (_, ascii) = ASCII_FOLDS.iter().find(|(from, _)| *from == point)?;
            folded.push_str(ascii);
        }
    }
    assert!(folded.is_ascii());
    Some(folded)
}

/// Tells whether a walk may name a link path on a volume that folds names: every part
/// folds onto the same ASCII name under [`ascii_fold`], or both stay outside ASCII, since
/// the volume may still fold one onto the other. A part that stays outside ASCII never
/// matches one that folds onto ASCII. Inputs: the link path and the walk's parts.
/// Output: true when it may.
fn folds_onto(link: &str, walk: &[&str]) -> bool {
    link.split('/').count() == walk.len()
        && link.split('/').zip(walk).all(|(part, step)| {
            match (ascii_fold(part), ascii_fold(step)) {
                (Some(part), Some(step)) => part == step,
                (None, None) => true,
                _ => false,
            }
        })
}

/// Tells whether a walk starts with the agents' folder. A compared part that is not ASCII
/// counts as entering it, since the volume may fold it to an ASCII name. Inputs: the
/// walk's parts and the agents' folder parts. Output: true when it enters.
fn entered(walk: &[&str], agents: &[String]) -> bool {
    for (part, agent) in walk.iter().zip(agents) {
        if !part.is_ascii() {
            return true;
        }
        if !part.eq_ignore_ascii_case(agent) {
            return false;
        }
    }
    walk.len() >= agents.len()
}

/// Tells whether another link of the tree, resolved by name from its folder, passes
/// through or ends at one of the change's links, so the change would carry it to where
/// the change's link points. A step that is not ASCII matches any of the change's links
/// the volume could fold it onto. Inputs: its path, its target and the change's link
/// paths in ASCII lower case. Output: true when it does.
fn passes_through(path: &str, target: &[u8], fresh: &BTreeSet<String>) -> bool {
    let Ok(target) = std::str::from_utf8(target) else {
        return false;
    };
    if target.starts_with('/') {
        return false;
    }
    let mut walk: Vec<&str> = path.split('/').collect();
    walk.pop();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if walk.pop().is_none() {
                    return false;
                }
            }
            name => {
                walk.push(name);
                if fresh.contains(&walk.join("/").to_ascii_lowercase())
                    || (!walk.iter().all(|step| step.is_ascii())
                        && fresh.iter().any(|link| folds_onto(link, &walk)))
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Sums the sizes of blobs as git stores them. Inputs: the repository and the blob ids.
/// Output: the bytes, or `changes_git_failed` when one is missing.
fn blob_bytes(repo: &Repo, blobs: &[&str]) -> Result<u64, EngineError> {
    Ok(blob_sizes(repo, blobs)?
        .into_iter()
        .fold(0, u64::saturating_add))
}

/// Reads the sizes of blobs as git stores them, with one `cat-file --batch-check`.
/// Inputs: the repository and the blob ids. Output: the sizes in order, or
/// `changes_git_failed` when one is missing.
fn blob_sizes(repo: &Repo, blobs: &[&str]) -> Result<Vec<u64>, EngineError> {
    if blobs.is_empty() {
        return Ok(Vec::new());
    }
    let mut input = blobs.join("\n");
    input.push('\n');
    let output = repo.exec(
        &["cat-file", "--batch-check=%(objectsize)"],
        Some(input.as_bytes()),
        &[],
        LISTING_BYTES_MAX,
    )?;
    if output.code != 0 || output.overflow {
        return Err(failure("changes_git_failed"));
    }
    let sizes = std::str::from_utf8(&output.stdout)
        .map_err(|_| failure("changes_git_failed"))?
        .lines()
        .map(|line| {
            line.parse::<u64>()
                .map_err(|_| failure("changes_git_failed"))
        })
        .collect::<Result<Vec<u64>, EngineError>>()?;
    if sizes.len() != blobs.len() {
        return Err(failure("changes_git_failed"));
    }
    Ok(sizes)
}

/// Tells whether the owner's ignore rules exclude any of `paths`, tracked or not: the
/// rules in the owner's checkout, never files in the agent's worktree, which the agent
/// can rewrite and hide with skip-worktree. Each name goes in behind `./`, since
/// check-ignore takes no literal magic and would read a name starting with `:` as
/// pathspec magic. Inputs: the owner's checkout and the paths. Output: the answer.
fn ignores_any(checkout: &Path, paths: &[&str]) -> Result<bool, EngineError> {
    if paths.is_empty() {
        return Ok(false);
    }
    let owner = Repo::open(checkout, &checkout.join(".git"), Some(checkout))?;
    let input: String = paths.iter().map(|path| format!("./{path}\0")).collect();
    let output = owner.exec(
        &["check-ignore", "-z", "--stdin", "--no-index"],
        Some(input.as_bytes()),
        &[],
        LISTING_BYTES_MAX,
    )?;
    match output.code {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(failure("changes_git_failed")),
    }
}

/// Stages everything in an agent worktree and makes it a commit object above `head`
/// without moving any ref, or nothing when the tree equals `head`'s. The whole change
/// set from `above` must stay within the bounds, staged files included, and its own work
/// from `owned` must pass the change checks.
/// Inputs: the worktree, its root, the owner's checkout, the set's start, where its own
/// work starts, HEAD, the bounds and the message. Output: the commit.
#[allow(clippy::too_many_arguments)]
fn commit_worktree(
    repo: &Repo,
    worktree: &Path,
    checkout: &Path,
    above: &str,
    owned: &str,
    head: &str,
    bounds: &TreeBounds,
    message: &str,
) -> Result<Option<String>, EngineError> {
    check_bounds(repo, worktree, bounds)?;
    repo.confined(Writes::Objects)?
        .run(&["add", "-A", "--", ":(top)", &teammates_left_out()])?;
    let tree = object_id(&repo.run(&["write-tree"])?)?;
    if tree == repo.tree_of(head)? {
        return Ok(None);
    }
    if let Err(refusal) = check_change(repo, checkout, above, owned, &tree, head, bounds) {
        repo.run(&["read-tree", head])?;
        return Err(refusal);
    }
    repo.commit_tree(&tree, &[head], message, ENGINE).map(Some)
}

/// Records the commit a newly added agent worktree holds as the base its change sets
/// open above: a linked worktree's HEAD, or the tip of a clone's branch as fetched into
/// the owner repository. Inputs: the store, the owner checkout, the worktree and its git
/// folder. Output: Ok once stored.
pub(super) fn start(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<(), EngineError> {
    store::initialize(db)?;
    let _held = hold(repository);
    let head = match check_places(repository, worktree, git_dir)? {
        Layout::Linked => open_agent(db, AgentKind::Sub, repository, worktree, git_dir)?.head()?,
        Layout::Clone => clone::fetch(
            &shared(repository)?,
            repository,
            worktree,
            &clone_branch(db, worktree)?,
            &crate::worktrees::TREE_BOUNDS,
        )?,
    };
    set_base(db, repository, worktree, git_dir, &head)
}

/// Records the commit a master's leased worktree holds as the base its change sets open
/// above, as [`start`] does for a team agent's new worktree, unless one is stored already,
/// which never moves past commits the master made and is read without the repository hold,
/// so a long holder never delays a master turn. Inputs: the store, the owner checkout, the
/// worktree on its lease branch and its git folder. Output: Ok once a base is stored.
pub(super) fn start_master(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<(), EngineError> {
    store::initialize(db)?;
    let stored = super::subagent::path_text(worktree)?;
    if store::base_of(db, stored)?.is_some() {
        return Ok(());
    }
    let _held = hold(repository);
    if store::base_of(db, stored)?.is_some() {
        return Ok(());
    }
    let head = open_agent(db, AgentKind::Master, repository, worktree, git_dir)?.head()?;
    set_base(db, repository, worktree, git_dir, &head)?;
    assert_eq!(store::base_of(db, stored)?.as_deref(), Some(head.as_str()));
    Ok(())
}

/// The commit an agent worktree holds now, for a worker cut from its lead (N1): a linked
/// worktree's HEAD, or the tip of a clone's branch as fetched into the owner repository,
/// so the worker's own copy reads it through the owner's objects. Inputs: the store, the
/// owner checkout, the lead's worktree and its git folder. Output: the commit id.
pub(super) fn tip(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<String, EngineError> {
    store::initialize(db)?;
    let _held = hold(repository);
    read_tip(db, repository, worktree, git_dir)
}

/// The commit an agent worktree holds now, read as [`tip`] reads it, without taking the
/// repository hold, for a caller that holds it already or reads before taking it.
/// Inputs: the store, the owner checkout, the worktree and its git folder. Output: the
/// commit id.
pub(super) fn read_tip(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<String, EngineError> {
    match check_places(repository, worktree, git_dir)? {
        Layout::Linked => open_agent(db, AgentKind::Sub, repository, worktree, git_dir)?.head(),
        Layout::Clone => clone::fetch(
            &shared(repository)?,
            repository,
            worktree,
            &clone_branch(db, worktree)?,
            &crate::worktrees::TREE_BOUNDS,
        ),
    }
}

/// The base a clone's next change set opens above once its agent restarted from main after
/// its last set landed (OS-8): while the recorded base left the fetched head and the commit
/// the engine landed for it is in that head, the merge base of the head and the landing tip
/// in the owner repository, so the owner's later main is not the agent's change; else the
/// recorded base, which GA-R3 then refuses. Inputs: the store, the owner repository, the
/// workspace, the recorded base, the fetched head and the tip. Output: the base.
fn restarted_base(
    db: &Connection,
    shared: &Repo,
    workspace: &Workspace,
    base: String,
    head: &str,
    tip: &str,
) -> Result<String, EngineError> {
    let ancestor =
        |older: &str, newer: &str| shared.test(&["merge-base", "--is-ancestor", older, newer]);
    if ancestor(&base, head)? {
        return Ok(base);
    }
    let Some(landed) = store::landed_head(db, &workspace.project, &workspace.repository, &base)?
    else {
        return Ok(base);
    };
    if shared.commit_of(&landed)?.as_deref() != Some(landed.as_str()) || !ancestor(&landed, head)? {
        return Ok(base);
    }
    let output = shared.exec(&["merge-base", head, tip], None, &[], 256)?;
    match output.code {
        0 => object_id(&output.stdout),
        1 => Ok(base),
        _ => Err(failure("changes_git_failed")),
    }
}

/// Commits an agent worktree's changes and adds them to its open change set, or opens
/// one above the base recorded for the worktree, else its HEAD, so commits the agent
/// made itself are part of it. An agent's own clone is never committed in: its change
/// set is the branch it committed on, fetched into the owner repository before the
/// repository hold is taken, so a slow agent-sized fetch never stalls the owner's landing,
/// and that branch must hold the set's base. The whole set, agent commits and staged
/// files included,
/// must stay within the bounds before anything is stored. With no set open, a head already
/// on the landing tip is not checked, since all its commits are on that branch: it opens no
/// set, and files left uncommitted above it are still checked with the whole range by the
/// commit that takes them in (LAND-8-L2). The gitlink, layout, link and ignore checks read
/// the set's own work from where Accept measures it against the tip
/// (`owner::measured_base`), so owner work the agent took in from the tip is not refused as
/// its own, while the bounds count the whole set (LAND-17-L1). The change set is stored
/// before HEAD moves, so a stop between the two loses nothing: the next turn end
/// commits the same tree again. Its counts are measured against the tip it lands on,
/// read before the hold as the clone fetch is (`owner::landing_tip`, DEC-107).
/// Inputs: the store, the workspace, the turn end, the bounds and the time now.
/// Output: the open change set, None when nothing is pending, or
/// `changes_branch_not_above_base` for a clone branch that dropped the base; an updating
/// set refused so waits for its agent again, so the owner's next Update hands the agent
/// the merge it skipped (GA-R3).
pub(super) fn record(
    db: &Connection,
    workspace: &Workspace,
    turn: &TurnEnd,
    bounds: &TreeBounds,
    now: u64,
) -> Result<Option<ChangeSetId>, EngineError> {
    store::initialize(db)?;
    let layout = check_places(
        &workspace.repository,
        &workspace.worktree,
        &workspace.git_dir,
    )?;
    let agent_id = name(&workspace.agent.id)?;
    let turn_id = name(&turn.turn)?;
    let worktree = workspace.worktree.to_str().ok_or_else(invalid)?;
    let shared = shared(&workspace.repository)?;
    let fetched = match layout {
        Layout::Clone => Some(clone::fetch(
            &shared,
            &workspace.repository,
            &workspace.worktree,
            &clone_branch(db, &workspace.worktree)?,
            bounds,
        )?),
        Layout::Linked => None,
    };
    let tip = super::owner::landing_tip(
        db,
        &shared,
        &workspace.repository,
        &workspace.project,
        &workspace.agent,
    );
    let _held = hold(&workspace.repository);
    let open = store::open_for(db, worktree)?;
    if open
        .as_ref()
        .is_some_and(|record| record.agent.id != agent_id)
    {
        return Err(failure("changes_agent_has_pending"));
    }
    let (agent, head) = match fetched {
        Some(tip) => (None, tip),
        None => {
            let agent = open_agent(
                db,
                workspace.agent.kind,
                &workspace.repository,
                &workspace.worktree,
                &workspace.git_dir,
            )?;
            let head = agent.head()?;
            (Some(agent), head)
        }
    };
    let repo = agent.as_ref().map_or(&shared, |agent| &agent.repo);
    let base = match store::base_of(db, worktree)? {
        Some(base) if repo.commit_of(&base)?.as_deref() == Some(base.as_str()) => base,
        _ => head.clone(),
    };
    let base = match (&agent, &open, &tip) {
        (None, None, Some(tip)) => restarted_base(db, &shared, workspace, base, &head, tip)?,
        _ => base,
    };
    let above = open
        .as_ref()
        .map_or(base.as_str(), |record| record.base.as_str());
    let fresh = open
        .as_ref()
        .map_or(base.as_str(), |record| record.head.as_str());
    if agent.is_none() && !shared.test(&["merge-base", "--is-ancestor", above, &head])? {
        if let Some(mut record) =
            open.filter(|record| record.state == ChangeState::Updating && !record.waiting)
        {
            record.waiting = true;
            store::save(db, &record)?;
        }
        return Err(failure("changes_branch_not_above_base"));
    }
    let fast_forwarded =
        open.is_none() && above != head && already_on_tip(&shared, &head, tip.as_deref())?;
    let owned = match tip.as_deref() {
        Some(tip) => super::owner::measured_base(
            db,
            &shared,
            &workspace.project,
            &workspace.repository,
            above,
            &head,
            tip,
        )?,
        None => above.to_owned(),
    };
    if above != head && !fast_forwarded {
        check_change(
            repo,
            &workspace.repository,
            above,
            &owned,
            &head,
            fresh,
            bounds,
        )?;
    }
    super::merges::refuse_outside_merges(db, workspace, &shared, &head)?;
    let roots = [workspace.worktree.as_path(), workspace.repository.as_path()];
    let line = super::commit_subject(&turn.prompt_line, &roots);
    let message = format!(
        "{}\n\nTurn {turn_id} ended: {}.\n",
        if line.is_empty() { "Agent turn" } else { &line },
        store::outcome_name(turn.outcome)
    );
    let committed = match &agent {
        Some(_) => commit_worktree(
            repo,
            &workspace.worktree,
            &workspace.repository,
            above,
            &owned,
            &head,
            bounds,
            &message,
        )?,
        None => None,
    };
    let mut record = match open {
        None if committed.is_none() && (head == base || fast_forwarded) => {
            return Ok(None);
        }
        None => Record {
            id: new_id()?,
            project: workspace.project.clone(),
            agent: workspace.agent.clone(),
            repository: workspace.repository.clone(),
            worktree: workspace.worktree.clone(),
            git_dir: workspace.git_dir.clone(),
            base: base.clone(),
            head: base,
            state: ChangeState::Pending,
            last_outcome: turn.outcome,
            turn: turn_id.clone(),
            prompt_line: line,
            message: String::new(),
            waiting: false,
            outside: false,
            made: false,
            undone: None,
            landed_on: None,
            restored: false,
            regenerated_head: None,
            regenerated_files: Vec::new(),
            accepted_in: None,
            files: 0,
            added: 0,
            removed: 0,
            conflicts: Vec::new(),
            created_ms: now,
            updated_ms: now,
            restorable_until_ms: None,
            pruned: false,
            landed: None,
            intent: None,
        },
        Some(record) => record,
    };
    record.agent.label = plain(&workspace.agent.label);
    let current = committed.clone().unwrap_or(head.clone());
    if committed.is_none() && record.head == current && record.state != ChangeState::Updating {
        return Ok(Some(record.id));
    }
    record.state = ChangeState::Pending;
    record.waiting = false;
    record.conflicts.clear();
    record.last_outcome = turn.outcome;
    record.turn = turn_id;
    record.head = current.clone();
    let from = match &tip {
        Some(tip) => super::owner::measured_from(db, &shared, &record, tip)?,
        None => record.base.clone(),
    };
    check_new_folders(
        &shared,
        tip.as_deref().unwrap_or(&from),
        &from,
        &record.head,
        new_folder_limit(db, &workspace.project)?,
    )?;
    let stats = diff::stats(repo, &from, &record.head)?;
    (record.files, record.added, record.removed) = (stats.files, stats.added, stats.removed);
    record.message = proposed(repo, &record, &from, &roots)?;
    record.updated_ms = now;
    store::save(db, &record)?;
    set_engine_ref(
        &shared,
        &workspace.repository,
        &pending_ref(&record.id),
        Some(&current),
    )?;
    if let (Some(commit), Some(agent)) = (committed, &agent) {
        agent.move_head(Some(&head), &commit, "CrazyAgent: turn end")?;
    }
    Ok(Some(record.id))
}

/// Says whether a commit is already on the branch a set would land on, so an agent that only
/// fast-forwarded its worktree to it has nothing to land. Inputs: the owner repository, the
/// commit and the landing tip, None while it cannot be read. Output: the answer.
fn already_on_tip(owner: &Repo, commit: &str, tip: Option<&str>) -> Result<bool, EngineError> {
    match tip {
        Some(tip) => owner.test(&["merge-base", "--is-ancestor", commit, tip]),
        None => Ok(false),
    }
}

/// Notes that the agent of an updating change set was handed the work that resolves it,
/// so the set no longer shows as waiting for its agent. Inputs: the store and the change
/// set. Output: Ok, also when the set is no longer updating.
pub(super) fn resolve_handed_over(db: &Connection, id: &ChangeSetId) -> Result<(), EngineError> {
    store::initialize(db)?;
    let repository = store::load(db, id)?.repository;
    let _held = hold(&repository);
    let mut record = store::load(db, id)?;
    if record.state == ChangeState::Updating && record.waiting {
        record.waiting = false;
        store::save(db, &record)?;
    }
    Ok(())
}

/// The task prompt for resolving the conflict markers an update left in a worktree,
/// naming the branch it merged: its lead's branch for a lead's worker while Lands is Lead,
/// else the owner's. Each conflicting file is on one line with its control characters
/// replaced, newlines included. Inputs: the store and the change set. Output: the prompt.
pub(super) fn resolve_prompt(db: &Connection, id: &ChangeSetId) -> Result<String, EngineError> {
    store::initialize(db)?;
    let record = store::load(db, id)?;
    if record.state != ChangeState::Updating {
        return Err(failure("changes_stale"));
    }
    let (merged, wait, whose) =
        match super::subagent::lead_of(db, &record.project, &record.agent, &record.repository) {
            Ok(Some((_, lead))) => (
                match super::subagent::lead_branch(db, &lead) {
                    Some(branch) => format!("Your lead's branch {branch}"),
                    None => String::from("Your lead's branch"),
                },
                "your lead's merge",
                "your lead's",
            ),
            _ => (String::from("The owner's branch"), "review", "the owner's"),
        };
    let clone = in_clone(&record);
    let mut prompt = if clone {
        format!(
            "{merged} changed while your work waited for {wait}, so CrazyAgent merged it with your branch as commit {}. First run `git merge {}` in your worktree on your branch.\n",
            record.head, record.head
        )
    } else {
        format!(
            "{merged} changed while your work waited for {wait}, so CrazyAgent merged it into your worktree.\n",
        )
    };
    if record.conflicts.is_empty() {
        prompt.push_str(&format!("The merge had no conflicts. Check that your work still fits {whose} changes and fix anything that no longer works.\n"));
    } else {
        prompt.push_str("These files have conflict markers (<<<<<<<, ======= and >>>>>>>):\n");
        for file in record.conflicts.iter().take(CONFLICTS_SHOWN_MAX) {
            prompt.push_str("- ");
            prompt.push_str(&crate::memory::visible_text(
                &file.replace(char::is_control, "\u{fffd}"),
            ));
            prompt.push('\n');
        }
        if record.conflicts.len() > CONFLICTS_SHOWN_MAX {
            prompt.push_str("- and more; search the worktree for <<<<<<<\n");
        }
        prompt.push_str("Resolve every conflict so both sides' intent is kept, remove every marker, and make sure the project still builds and its tests pass.\n");
    }
    prompt.push_str(if clone {
        "Commit the result on your branch; CrazyAgent takes what you committed when you finish.\n"
    } else {
        "Do not run git commit; CrazyAgent records your changes when you finish.\n"
    });
    Ok(prompt)
}

/// Discards an open change set: keeps its head under a discarded ref, then points the
/// agent's worktree at the owner's current commit when it holds nothing newer. A lead's
/// workers' open sets close with it (DEC-107). Inputs: the store, the change set, the head
/// that was reviewed, the time now and whether the owner pressed it. Output: the time it
/// stays restorable until.
pub(super) fn discard(
    db: &Connection,
    id: &ChangeSetId,
    expected_head: &str,
    now: u64,
    owner_pressed: bool,
) -> Result<u64, EngineError> {
    store::initialize(db)?;
    let repository = store::load(db, id)?.repository;
    let _held = hold(&repository);
    let mut record = super::owner::settled(db, id)?;
    if !record.is_open() || record.head != expected_head || record.intent.is_some() {
        return Err(failure("changes_stale"));
    }
    let until = discard_held(db, &mut record, now)?;
    if let Err(error) = super::subagent::close_with_lead(db, &record, owner_pressed) {
        eprintln!(
            "landing_settle_failed cause={}",
            crate::protocol::plain_code(&error.0)
        );
    }
    Ok(until)
}

/// Releases the claims a change set held for its agent's ended work once it closes, landed
/// or discarded, so no claim outlives the edits it guarded and claims another set holds
/// stay (OS-8, DEC-117 Q7). A failure is logged by its plain code, never fails the close,
/// and is tried again at the agent's next turn end. Inputs: the store and the closed set.
pub(super) fn release_agent_claims(db: &Connection, record: &Record) {
    assert!(!record.is_open());
    logged_claims(
        i64::try_from(record.updated_ms)
            .map_err(|_| failure("changes_unavailable"))
            .and_then(|now| crate::coord::release_set(db, record.id.as_str(), now))
            .map(drop),
    );
}

/// Holds an agent's claims on its ended work for the change set its turn end leaves
/// open, or releases them when the turn leaves nothing to land, after releasing those of
/// any set that closed while its release failed (OS-8, DEC-117 Q7). A failure is logged by
/// its plain code, never fails the turn end, and is tried again at the next one. Inputs:
/// the store, the workspace, the open change set and the time now.
pub(super) fn hold_agent_claims(
    db: &Connection,
    workspace: &Workspace,
    open: Option<&ChangeSetId>,
    now: u64,
) {
    logged_claims(
        i64::try_from(now)
            .map_err(|_| failure("changes_unavailable"))
            .and_then(|now| {
                let agent = match workspace.agent.kind {
                    AgentKind::Sub => workspace.agent.id.parse().ok(),
                    AgentKind::Master => crate::coord::master_of(db, &workspace.project)?,
                };
                let Some(agent) = agent else {
                    return Ok(0);
                };
                for set in crate::coord::held_sets(db, agent)? {
                    let closed = ChangeSetId::new(&set)
                        .ok()
                        .and_then(|id| store::load(db, &id).ok())
                        .is_some_and(|record| !record.is_open());
                    if closed {
                        crate::coord::release_set(db, &set, now)?;
                    }
                }
                match open {
                    Some(id) => crate::coord::hold_for_set(db, agent, id.as_str()),
                    None => crate::coord::release_unheld(db, agent, now),
                }
            })
            .map(drop),
    );
}

/// Settles a project's claims no turn end will, as [`super::sweep_claims`] says. Reads
/// enough held sets that the open ones never crowd out `SWEPT_SETS_MAX` closed ones. A chat
/// master's set records its session id, so a master takes the newest open master set.
/// Inputs: the store, the project and the time now. Output: Ok or the first failure.
pub(super) fn sweep_claims(
    db: &Connection,
    project: &ProjectId,
    now: u64,
) -> Result<(), EngineError> {
    let now = i64::try_from(now).map_err(|_| failure("changes_unavailable"))?;
    let open = store::open_of(db, Some(project))?;
    for set in crate::coord::held_sets_in(db, project, open.len() + SWEPT_SETS_MAX)? {
        let closed = ChangeSetId::new(&set)
            .ok()
            .and_then(|id| store::load(db, &id).ok())
            .is_some_and(|record| !record.is_open());
        if closed {
            crate::coord::release_set(db, &set, now)?;
        }
    }
    for record in &open {
        crate::coord::restore_set(db, record.id.as_str())?;
    }
    for agent in crate::coord::stopped_with_unheld(db, project)? {
        let id = agent.to_string();
        let master = crate::coord::is_master(db, agent)?;
        let set = open.iter().rev().find(|record| match record.agent.kind {
            AgentKind::Master => master,
            AgentKind::Sub => record.agent.id == id,
        });
        match set {
            Some(record) => crate::coord::hold_for_set(db, agent, record.id.as_str())?,
            None => crate::coord::release_unheld(db, agent, now)?,
        };
    }
    Ok(())
}

/// Logs a failed claim hold or release by its plain code, so it never fails the landing
/// step that ran it. Input: the result.
fn logged_claims(result: Result<(), EngineError>) {
    if let Err(error) = result {
        eprintln!(
            "landing_claims_failed cause={}",
            crate::protocol::plain_code(&error.0)
        );
    }
}

/// Discards one open change set holding no intent, as [`discard`] does once it checked the
/// owner's reviewed head. When the store refuses the closed state, the change set is left
/// open as the store still holds it, and its discarded ref goes again, since pruning reads
/// discarded sets only; a failed drop of that ref is logged. The caller holds [`hold`] on
/// its repository. Inputs: the store, the change set and the time now. Output: the time it
/// stays restorable until.
pub(super) fn discard_held(
    db: &Connection,
    record: &mut Record,
    now: u64,
) -> Result<u64, EngineError> {
    assert!(record.is_open() && record.intent.is_none());
    let open = record.clone();
    let id = record.id.clone();
    let shared = shared(&record.repository)?;
    let target = shared
        .commit_of("HEAD")?
        .ok_or_else(|| failure("changes_git_failed"))?;
    let until = now
        .checked_add(RESTORE_WINDOW_MS)
        .ok_or_else(|| failure("changes_unavailable"))?;
    set_engine_ref(
        &shared,
        &record.repository,
        &discarded_ref(&id),
        Some(&record.head),
    )?;
    record.state = ChangeState::Discarded;
    record.restorable_until_ms = Some(until);
    record.pruned = false;
    record.conflicts.clear();
    record.updated_ms = now;
    if let Err(error) = store::save(db, record) {
        *record = open;
        if let Err(dropped) = set_engine_ref(&shared, &record.repository, &discarded_ref(&id), None)
        {
            eprintln!(
                "landing_discard_ref_failed cause={}",
                crate::protocol::plain_code(&dropped.0)
            );
        }
        return Err(error);
    }
    release_agent_claims(db, record);
    set_engine_ref(&shared, &record.repository, &pending_ref(&id), None)?;
    let head = record.head.clone();
    reset_if_quiet(db, record, &head, &target)?;
    Ok(until)
}

/// Whether a discarded change set can no longer come back: pruned, never given a restore
/// window, or past it. Inputs: the record and the time now. Output: true when expired.
fn restore_expired(record: &Record, now: u64) -> bool {
    record.pruned || record.restorable_until_ms.is_none_or(|until| now >= until)
}

/// Brings a discarded change set back as pending, takes back the claims its discard
/// released, and puts its head back in the agent's worktree. An agent's own clone is read
/// by the engine's fetch of its branch, before the repository hold, and only for a change
/// set still inside its restore window, so a set that can never come back is refused as
/// expired and not by its clone's branch. Inputs: the store, the change set and the time
/// now. Output: Ok once
/// pending; refused once the restore window passed, while the worktree has another
/// open change set, or while it holds unrecorded work or a HEAD, or a clone a branch tip,
/// other than the commit the worktree's next change set would open above, so commits
/// made since are kept and a later turn end cannot replace the restored head (GA-R4).
pub(super) fn restore(db: &Connection, id: &ChangeSetId, now: u64) -> Result<(), EngineError> {
    store::initialize(db)?;
    let loaded = store::load(db, id)?;
    let clone_tip = if in_clone(&loaded) && loaded.state == ChangeState::Discarded {
        if restore_expired(&loaded, now) {
            return Err(failure("changes_restore_expired"));
        }
        Some(clone::fetch(
            &shared(&loaded.repository)?,
            &loaded.repository,
            &loaded.worktree,
            &clone_branch(db, &loaded.worktree)?,
            &crate::worktrees::TREE_BOUNDS,
        )?)
    } else {
        None
    };
    let repository = loaded.repository;
    let _held = hold(&repository);
    let mut record = super::owner::settled(db, id)?;
    if record.state != ChangeState::Discarded {
        return Err(failure("changes_stale"));
    }
    if restore_expired(&record, now) {
        return Err(failure("changes_restore_expired"));
    }
    let worktree = record.worktree.to_str().ok_or_else(invalid)?;
    if store::open_for(db, worktree)?.is_some() {
        return Err(failure("changes_agent_has_pending"));
    }
    if in_clone(&record) {
        if clone_tip.is_none() || store::base_of(db, worktree)? != clone_tip {
            return Err(failure("changes_agent_busy"));
        }
        let shared = shared(&record.repository)?;
        set_engine_ref(
            &shared,
            &record.repository,
            &pending_ref(id),
            Some(&record.head),
        )?;
        record.state = ChangeState::Pending;
        record.restorable_until_ms = None;
        record.restored = true;
        record.updated_ms = now;
        store::save(db, &record)?;
        return finish_restore(db, &record);
    }
    let agent = agent(db, &record)?;
    let current = agent.head()?;
    if store::base_of(db, worktree)?.as_deref() != Some(current.as_str())
        || !holds_only(&agent.repo, &current)?
    {
        return Err(failure("changes_agent_busy"));
    }
    let before = record.clone();
    let shared = shared(&record.repository)?;
    set_engine_ref(
        &shared,
        &record.repository,
        &pending_ref(id),
        Some(&record.head),
    )?;
    record.state = ChangeState::Pending;
    record.restorable_until_ms = None;
    record.restored = true;
    record.updated_ms = now;
    record.intent = Some(Intent::Reset {
        from: Some(current.clone()),
        target: record.head.clone(),
        earlier: Some(store::Earlier::of(&before)),
    });
    store::save(db, &record)?;
    if !reset_agent(&agent, &current, &record.head)? {
        store::save(db, &before)?;
        set_engine_ref(&shared, &record.repository, &pending_ref(id), None)?;
        return Err(failure("changes_agent_busy"));
    }
    record.intent = None;
    store::save(db, &record)?;
    finish_restore(db, &record)
}

/// Does what a restore does last, once its set is pending again: the claims the close
/// released hold again and its discarded ref goes, which pruning never would, since it reads
/// discarded sets only. A failed claim step is logged and never fails the restore. Inputs:
/// the store and the restored set. Output: Ok once the ref is gone.
pub(super) fn finish_restore(db: &Connection, record: &Record) -> Result<(), EngineError> {
    logged_claims(crate::coord::restore_set(db, record.id.as_str()).map(drop));
    set_engine_ref(
        &shared(&record.repository)?,
        &record.repository,
        &discarded_ref(&record.id),
        None,
    )
}

/// Deletes the discarded refs whose restore window ended by `now`, once each. A change set
/// whose ref cannot be deleted now is logged by its plain code and tried again at the next
/// run, so it never stops the others. Inputs: the store and the time now. Output: how many
/// were deleted.
pub(super) fn prune(db: &Connection, now: u64) -> Result<usize, EngineError> {
    store::initialize(db)?;
    let mut pruned = 0;
    for found in store::discarded_due(db, now)? {
        let _held = hold(&found.repository);
        match prune_one(db, &found.id) {
            Ok(true) => pruned += 1,
            Ok(false) => {}
            Err(error) => eprintln!(
                "landing_prune_failed cause={}",
                crate::protocol::plain_code(&error.0)
            ),
        }
    }
    Ok(pruned)
}

/// Deletes one discarded change set's ref, marks it pruned and forgets the owner edits
/// settled on it. The caller holds [`hold`] on its repository. Inputs: the store and the change set. Output: whether it was pruned
/// now, false when it is no longer discarded or already pruned.
fn prune_one(db: &Connection, id: &ChangeSetId) -> Result<bool, EngineError> {
    let mut record = store::load(db, id)?;
    if record.state != ChangeState::Discarded || record.pruned {
        return Ok(false);
    }
    if std::fs::symlink_metadata(record.repository.join(".git")).is_ok_and(|found| found.is_dir()) {
        set_engine_ref(
            &shared(&record.repository)?,
            &record.repository,
            &discarded_ref(&record.id),
            None,
        )?;
    }
    record.pruned = true;
    store::save(db, &record)?;
    super::owner_edit::logged("forget", super::owner_edit::forget(db, &record.id));
    Ok(true)
}
