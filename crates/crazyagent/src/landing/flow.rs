use super::diff::{self, plain};
use super::git::{Repo, failure, object_id};
use super::store::{self, Intent, Record};
use super::{ChangeSetId, ChangeState, RESTORE_WINDOW_MS, TurnEnd, Workspace};
use crate::engine::EngineError;
use crate::worktrees::TreeBounds;
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};

pub(super) const ENGINE: (&str, &str) = ("CrazyAgent", "engine@crazyagent.invalid");
const LISTING_BYTES_MAX: usize = 64 * 1024 * 1024;
const PATH_BYTES_MAX: usize = 1024;
const NAME_BYTES_MAX: usize = 128;
const SUBJECT_CHARS_MAX: usize = 72;
const PROPOSED_BYTES_MAX: usize = 1024;
const AGENT_COMMITS_READ_MAX: usize = 200;
const MORE_COMMITS: &str = "\n- and more commits";
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
const LINK_BYTES_MAX: usize = 4096;
const CONFLICTS_SHOWN_MAX: usize = 50;
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

fn invalid() -> EngineError {
    failure("changes_workspace_invalid")
}

fn canonical(path: &Path) -> Result<(), EngineError> {
    if path.is_absolute() && std::fs::canonicalize(path).ok().as_deref() == Some(path) {
        return Ok(());
    }
    Err(invalid())
}

/// Checks that a worktree is one the owner repository added and that its git folder is
/// that repository's record of it: every path absolute and free of links, the
/// repository's `.git` a real folder, the git folder directly under its `worktrees`
/// and naming this worktree's `.git` as its own. Inputs: the three paths.
/// Output: Ok, or `changes_workspace_invalid`.
pub(super) fn check_places(
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<(), EngineError> {
    for path in [repository, worktree, git_dir] {
        canonical(path)?;
    }
    let metadata = repository.join(".git");
    if !std::fs::symlink_metadata(&metadata).is_ok_and(|found| found.is_dir())
        || git_dir.parent() != Some(metadata.join("worktrees").as_path())
        || worktree == repository
        || worktree.starts_with(&metadata)
        || !std::fs::symlink_metadata(worktree).is_ok_and(|found| found.is_dir())
    {
        return Err(invalid());
    }
    let link = bounded_read(&git_dir.join("gitdir"), LINK_BYTES_MAX)?;
    let named = std::str::from_utf8(&link).map_err(|_| invalid())?;
    if Path::new(named.trim_end_matches('\n')) != worktree.join(".git") {
        return Err(invalid());
    }
    Ok(())
}

fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, EngineError> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| invalid())?;
    if !file.metadata().is_ok_and(|found| found.is_file()) {
        return Err(invalid());
    }
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

/// A commit subject from free text: its first line, plain, with paths under `roots`
/// made relative to them and any other absolute local path cut to its last component,
/// ended at the last whole word within `SUBJECT_CHARS_MAX` characters. A first word
/// longer than that is cut to fit. Inputs: the text and the folders, innermost first.
/// Output: the subject, maybe empty.
pub(super) fn subject(text: &str, roots: &[&Path]) -> String {
    let mut line = plain(text.lines().next().unwrap_or_default());
    for root in roots.iter().filter_map(|root| root.to_str()) {
        if root.len() > 1 {
            line = line.replace(&format!("{}/", root.trim_end_matches('/')), "");
        }
    }
    let mut subject = String::new();
    let words = line
        .split(|c: char| c.is_whitespace() || c == '\u{FFFD}')
        .map(local_path)
        .filter(|word| !word.is_empty());
    for word in words {
        let (used, more) = (subject.chars().count(), word.chars().count());
        if used == 0 && more > SUBJECT_CHARS_MAX {
            return word.chars().take(SUBJECT_CHARS_MAX).collect();
        }
        if used + usize::from(used > 0) + more > SUBJECT_CHARS_MAX {
            break;
        }
        if used > 0 {
            subject.push(' ');
        }
        subject.push_str(&word);
    }
    assert!(subject.chars().count() <= SUBJECT_CHARS_MAX);
    subject
}

/// One word with an absolute local path in it cut to the path's last component, the
/// punctuation around it kept. Input: the word. Output: the word, maybe empty.
fn local_path(word: &str) -> String {
    let core = word.trim_start_matches(['(', '"', '\'', '<', '[', '`']);
    let path = core.trim_end_matches([')', '"', '\'', '>', ']', '`', ',', '.', ';', ':', '!', '?']);
    if !LOCAL_ROOTS.iter().any(|root| path.starts_with(root)) {
        return word.to_owned();
    }
    let (lead, tail) = (&word[..word.len() - core.len()], &core[path.len()..]);
    match path.rsplit('/').find(|part| !part.is_empty()) {
        Some(last) => format!("{lead}{last}{tail}"),
        None => String::new(),
    }
}

/// The message an Accept proposes from the commits the agent made itself above the
/// base: the latest subject, then the earlier ones newest first as a list. Commits the
/// engine made and merges are left out. Inputs: the agent repository, the record and
/// the folders paths are shown relative to. Output: the message, empty when the agent
/// made no commit, at most `PROPOSED_BYTES_MAX` bytes.
fn proposed(repo: &Repo, record: &Record, roots: &[&Path]) -> Result<String, EngineError> {
    let count = format!("--max-count={AGENT_COMMITS_READ_MAX}");
    let range = format!("{}..{}", record.base, record.head);
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
        .map(|commit| subject(commit[1], roots))
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

pub(super) fn pending_ref(id: &ChangeSetId) -> String {
    format!("refs/crazyagent/pending/{}", id.as_str())
}

pub(super) fn discarded_ref(id: &ChangeSetId) -> String {
    format!("refs/crazyagent/discarded/{}", id.as_str())
}

/// Opens the owner repository's shared git folder for object and ref work.
pub(super) fn shared(record_repository: &Path) -> Result<Repo, EngineError> {
    Repo::open(&record_repository.join(".git"), None)
}

/// Opens an agent worktree through the git folder the engine recorded for it.
pub(super) fn agent(record: &Record) -> Result<Repo, EngineError> {
    check_places(&record.repository, &record.worktree, &record.git_dir)?;
    Repo::open(&record.git_dir, Some(&record.worktree))
}

/// Says whether an agent worktree holds exactly a commit: nothing staged or changed in
/// tracked files and no untracked file outside its ignore rules. Inputs: the worktree
/// and the commit. Output: true when it holds nothing else.
pub(super) fn holds_only(repo: &Repo, commit: &str) -> Result<bool, EngineError> {
    repo.run(&["update-index", "-q", "--refresh"])?;
    if !repo.test(&["diff-index", "--quiet", commit, "--"])? {
        return Ok(false);
    }
    let untracked = repo.exec(
        &["ls-files", "-z", "--others", "--exclude-standard"],
        None,
        &[],
        1,
    )?;
    if untracked.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    Ok(untracked.stdout.is_empty())
}

/// Points an agent worktree at a commit: its index and tracked files first, then its
/// HEAD, so a stop between the two leaves a state an engine start can finish.
/// Inputs: the worktree and the commit. Output: Ok once it holds the commit.
pub(super) fn reset_agent(repo: &Repo, target: &str) -> Result<(), EngineError> {
    repo.run(&["read-tree", "-u", "--reset", target])?;
    let env = [
        ("GIT_COMMITTER_NAME", ENGINE.0),
        ("GIT_COMMITTER_EMAIL", ENGINE.1),
    ];
    let moved = repo.exec(
        &["update-ref", "-m", "CrazyAgent: reset", "HEAD", target],
        None,
        &env,
        256,
    )?;
    if moved.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    assert_eq!(repo.commit_of("HEAD")?.as_deref(), Some(target));
    Ok(())
}

/// Records a reset of the agent's worktree to `target` after its change set closed, runs
/// it and clears the record, or skips it when the worktree holds more than `expected`, so
/// the agent's newer work stays; the worktree's next change set opens above whichever
/// commit it now starts from. Inputs: the store, the closed change set and both commits.
/// Output: Ok once saved.
pub(super) fn reset_if_quiet(
    db: &Connection,
    record: &mut Record,
    expected: &str,
    target: &str,
) -> Result<(), EngineError> {
    assert!(!record.is_open());
    let repo = agent(record)?;
    if repo.commit_of("HEAD")?.as_deref() != Some(expected) || !holds_only(&repo, expected)? {
        record.intent = None;
        store::save(db, record)?;
        return store::set_base(db, &record.worktree, expected);
    }
    record.intent = Some(Intent::Reset {
        target: target.to_owned(),
    });
    store::save(db, record)?;
    reset_agent(&repo, target)?;
    store::set_base(db, &record.worktree, target)?;
    record.intent = None;
    store::save(db, record)
}

/// Refuses a change past the bounds before git stores any of it: too many changed
/// paths, a path too deep, too long or not UTF-8, or too many bytes in the files.
/// Inputs: the worktree, its root and the bounds. Output: Ok within the bounds.
fn check_bounds(repo: &Repo, worktree: &Path, bounds: &TreeBounds) -> Result<(), EngineError> {
    let output = repo.exec(
        &[
            "ls-files",
            "-z",
            "--modified",
            "--others",
            "--deleted",
            "--exclude-standard",
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
/// UTF-8, too many bytes in the changed files, or a file added since `fresh` that the
/// owner's ignore rules exclude, which `add -A` never takes.
/// Inputs: the repository, the owner's checkout, the trees the set spans, the tree its
/// newest part starts from and the bounds. Output: Ok within the bounds.
fn check_change(
    repo: &Repo,
    checkout: &Path,
    from: &str,
    to: &str,
    fresh: &str,
    bounds: &TreeBounds,
) -> Result<(), EngineError> {
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
    let mut fields = output.stdout.split(|byte| *byte == 0);
    let (mut blobs, mut added) = (Vec::new(), Vec::new());
    let mut paths: usize = 0;
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
        paths += 1;
        if paths > bounds.files {
            return Err(failure("changes_too_large"));
        }
        let path = std::str::from_utf8(path).map_err(|_| failure("changes_git_unsupported"))?;
        if path.len() > PATH_BYTES_MAX || path.split('/').count() > bounds.components {
            return Err(failure("changes_too_large"));
        }
        if matches!(mode, "100644" | "100755" | "120000") {
            blobs.push(id);
        }
        if status == "A" {
            added.push(path);
        }
    }
    assert!(paths <= bounds.files && blobs.len() <= paths && added.len() <= paths);
    if blob_bytes(repo, &blobs)? > bounds.bytes {
        return Err(failure("changes_too_large"));
    }
    if fresh != from && !added.is_empty() {
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

/// Sums the sizes of blobs as git stores them. Inputs: the repository and the blob ids.
/// Output: the bytes, or `changes_git_failed` when one is missing.
fn blob_bytes(repo: &Repo, blobs: &[&str]) -> Result<u64, EngineError> {
    if blobs.is_empty() {
        return Ok(0);
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
    Ok(sizes.into_iter().fold(0, u64::saturating_add))
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
    let owner = Repo::open(&checkout.join(".git"), Some(checkout))?;
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
/// set from `above` must stay within the bounds, staged files included.
/// Inputs: the worktree, its root, the owner's checkout, the set's start, HEAD, the
/// bounds and the message. Output: the commit.
fn commit_worktree(
    repo: &Repo,
    worktree: &Path,
    checkout: &Path,
    above: &str,
    head: &str,
    bounds: &TreeBounds,
    message: &str,
) -> Result<Option<String>, EngineError> {
    check_bounds(repo, worktree, bounds)?;
    repo.run(&["add", "-A"])?;
    let tree = object_id(&repo.run(&["write-tree"])?)?;
    if tree == repo.tree_of(head)? {
        return Ok(None);
    }
    if let Err(refusal) = check_change(repo, checkout, above, &tree, head, bounds) {
        repo.run(&["read-tree", head])?;
        return Err(refusal);
    }
    repo.commit_tree(&tree, &[head], message, ENGINE).map(Some)
}

/// Records the commit a newly added agent worktree holds as the base its change sets
/// open above. Inputs: the store, the owner checkout, the worktree and its git folder.
/// Output: Ok once stored.
pub(super) fn start(
    db: &Connection,
    repository: &Path,
    worktree: &Path,
    git_dir: &Path,
) -> Result<(), EngineError> {
    store::initialize(db)?;
    check_places(repository, worktree, git_dir)?;
    let _held = hold(repository);
    let head = Repo::open(git_dir, Some(worktree))?
        .commit_of("HEAD")?
        .ok_or_else(|| failure("changes_git_failed"))?;
    store::set_base(db, worktree, &head)
}

/// Commits an agent worktree's changes and adds them to its open change set, or opens
/// one above the base recorded for the worktree, else its HEAD, so commits the agent
/// made itself are part of it. The whole set, agent commits and staged files included,
/// must stay within the bounds before anything is stored. The change set is stored
/// before HEAD moves, so a stop between the two loses nothing: the next turn end
/// commits the same tree again.
/// Inputs: the store, the workspace, the turn end, the bounds and the time now.
/// Output: the open change set, None when nothing is pending.
pub(super) fn record(
    db: &Connection,
    workspace: &Workspace,
    turn: &TurnEnd,
    bounds: &TreeBounds,
    now: u64,
) -> Result<Option<ChangeSetId>, EngineError> {
    store::initialize(db)?;
    check_places(
        &workspace.repository,
        &workspace.worktree,
        &workspace.git_dir,
    )?;
    let agent_id = name(&workspace.agent.id)?;
    let turn_id = name(&turn.turn)?;
    let worktree = workspace.worktree.to_str().ok_or_else(invalid)?;
    let _held = hold(&workspace.repository);
    let open = store::open_for(db, worktree)?;
    if open
        .as_ref()
        .is_some_and(|record| record.agent.id != agent_id)
    {
        return Err(failure("changes_agent_has_pending"));
    }
    let repo = Repo::open(&workspace.git_dir, Some(&workspace.worktree))?;
    let head = repo
        .commit_of("HEAD")?
        .ok_or_else(|| failure("changes_git_failed"))?;
    let base = match store::base_of(db, worktree)? {
        Some(base) if repo.commit_of(&base)?.as_deref() == Some(base.as_str()) => base,
        _ => head.clone(),
    };
    let above = open
        .as_ref()
        .map_or(base.as_str(), |record| record.base.as_str());
    let fresh = open
        .as_ref()
        .map_or(base.as_str(), |record| record.head.as_str());
    if above != head {
        check_change(&repo, &workspace.repository, above, &head, fresh, bounds)?;
    }
    let roots = [workspace.worktree.as_path(), workspace.repository.as_path()];
    let line = subject(&turn.prompt_line, &roots);
    let message = format!(
        "{}\n\nTurn {turn_id} ended: {}.\n",
        if line.is_empty() { "Agent turn" } else { &line },
        store::outcome_name(turn.outcome)
    );
    let committed = commit_worktree(
        &repo,
        &workspace.worktree,
        &workspace.repository,
        above,
        &head,
        bounds,
        &message,
    )?;
    let mut record = match open {
        None if committed.is_none() && head == base => return Ok(None),
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
    let stats = diff::stats(&repo, &record.base, &record.head)?;
    (record.files, record.added, record.removed) = (stats.files, stats.added, stats.removed);
    record.message = proposed(&repo, &record, &roots)?;
    record.updated_ms = now;
    store::save(db, &record)?;
    let shared = shared(&workspace.repository)?;
    shared.run(&["update-ref", &pending_ref(&record.id), &current])?;
    if let Some(commit) = committed {
        let env = [
            ("GIT_COMMITTER_NAME", ENGINE.0),
            ("GIT_COMMITTER_EMAIL", ENGINE.1),
        ];
        let message = "CrazyAgent: turn end";
        let moved = repo.exec(
            &["update-ref", "-m", message, "HEAD", &commit, &head],
            None,
            &env,
            256,
        )?;
        if moved.code != 0 {
            return Err(failure("changes_git_failed"));
        }
    }
    Ok(Some(record.id))
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

/// The task prompt for resolving the conflict markers an update left in a worktree.
/// Inputs: the store and the change set. Output: the prompt.
pub(super) fn resolve_prompt(db: &Connection, id: &ChangeSetId) -> Result<String, EngineError> {
    store::initialize(db)?;
    let record = store::load(db, id)?;
    if record.state != ChangeState::Updating {
        return Err(failure("changes_stale"));
    }
    let mut prompt = String::from(
        "The owner's branch changed while your work waited for review, so CrazyAgent merged it into your worktree.\n",
    );
    if record.conflicts.is_empty() {
        prompt.push_str("The merge had no conflicts. Check that your work still fits the owner's changes and fix anything that no longer works.\n");
    } else {
        prompt.push_str("These files have conflict markers (<<<<<<<, ======= and >>>>>>>):\n");
        for file in record.conflicts.iter().take(CONFLICTS_SHOWN_MAX) {
            prompt.push_str("- ");
            prompt.push_str(file);
            prompt.push('\n');
        }
        if record.conflicts.len() > CONFLICTS_SHOWN_MAX {
            prompt.push_str("- and more; search the worktree for <<<<<<<\n");
        }
        prompt.push_str("Resolve every conflict so both sides' intent is kept, remove every marker, and make sure the project still builds and its tests pass.\n");
    }
    prompt.push_str("Do not run git commit; CrazyAgent records your changes when you finish.\n");
    Ok(prompt)
}

/// Discards an open change set: keeps its head under a discarded ref, then points the
/// agent's worktree at the owner's current commit when it holds nothing newer.
/// Inputs: the store, the change set, the head the owner reviewed and the time now.
/// Output: the time it stays restorable until.
pub(super) fn discard(
    db: &Connection,
    id: &ChangeSetId,
    expected_head: &str,
    now: u64,
) -> Result<u64, EngineError> {
    store::initialize(db)?;
    let repository = store::load(db, id)?.repository;
    let _held = hold(&repository);
    let mut record = super::owner::settled(db, id)?;
    if !record.is_open() || record.head != expected_head || record.intent.is_some() {
        return Err(failure("changes_stale"));
    }
    let shared = shared(&record.repository)?;
    let target = shared
        .commit_of("HEAD")?
        .ok_or_else(|| failure("changes_git_failed"))?;
    shared.run(&["update-ref", &discarded_ref(id), &record.head])?;
    let until = now
        .checked_add(RESTORE_WINDOW_MS)
        .ok_or_else(|| failure("changes_unavailable"))?;
    record.state = ChangeState::Discarded;
    record.restorable_until_ms = Some(until);
    record.pruned = false;
    record.conflicts.clear();
    record.updated_ms = now;
    store::save(db, &record)?;
    shared.run(&["update-ref", "-d", &pending_ref(id)])?;
    let head = record.head.clone();
    reset_if_quiet(db, &mut record, &head, &target)?;
    Ok(until)
}

/// Brings a discarded change set back as pending and puts its head back in the agent's
/// worktree. Inputs: the store, the change set and the time now. Output: Ok once
/// pending; refused once the restore window passed, while the worktree has another
/// open change set, or while it holds unrecorded work.
pub(super) fn restore(db: &Connection, id: &ChangeSetId, now: u64) -> Result<(), EngineError> {
    store::initialize(db)?;
    let repository = store::load(db, id)?.repository;
    let _held = hold(&repository);
    let mut record = super::owner::settled(db, id)?;
    if record.state != ChangeState::Discarded {
        return Err(failure("changes_stale"));
    }
    if record.pruned || record.restorable_until_ms.is_none_or(|until| now >= until) {
        return Err(failure("changes_restore_expired"));
    }
    let worktree = record.worktree.to_str().ok_or_else(invalid)?;
    if store::open_for(db, worktree)?.is_some() {
        return Err(failure("changes_agent_has_pending"));
    }
    let repo = agent(&record)?;
    let current = repo
        .commit_of("HEAD")?
        .ok_or_else(|| failure("changes_git_failed"))?;
    if !holds_only(&repo, &current)? {
        return Err(failure("changes_agent_busy"));
    }
    let shared = shared(&record.repository)?;
    shared.run(&["update-ref", &pending_ref(id), &record.head])?;
    record.state = ChangeState::Pending;
    record.restorable_until_ms = None;
    record.updated_ms = now;
    record.intent = Some(Intent::Reset {
        target: record.head.clone(),
    });
    store::save(db, &record)?;
    reset_agent(&repo, &record.head)?;
    record.intent = None;
    store::save(db, &record)?;
    shared.run(&["update-ref", "-d", &discarded_ref(id)])?;
    Ok(())
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

/// Deletes one discarded change set's ref and marks it pruned. The caller holds [`hold`]
/// on its repository. Inputs: the store and the change set. Output: whether it was pruned
/// now, false when it is no longer discarded or already pruned.
fn prune_one(db: &Connection, id: &ChangeSetId) -> Result<bool, EngineError> {
    let mut record = store::load(db, id)?;
    if record.state != ChangeState::Discarded || record.pruned {
        return Ok(false);
    }
    if std::fs::symlink_metadata(record.repository.join(".git")).is_ok_and(|found| found.is_dir()) {
        shared(&record.repository)?.run(&["update-ref", "-d", &discarded_ref(&record.id)])?;
    }
    record.pruned = true;
    store::save(db, &record)?;
    Ok(true)
}
