use super::clone;
use super::diff;
use super::flow::{self, ENGINE, hold, pending_ref, reset_if_quiet, shared, verify_objects};
use super::git::{Repo, failure};
use super::store::{self, Intent, Record};
use super::{
    AcceptOutcome, AgentKind, BRANCH_BYTES_MAX, BRANCHES_MAX, ChangeSetId, ChangeState,
    MESSAGE_BYTES_MAX, TargetBranches,
};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};

const IDENTITY_BYTES_MAX: usize = 256;
const JUDGED_MAX: usize = 256;
const BUSY_STATE: [&str; 11] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "BISECT_LOG",
    "BISECT_START",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
    "index.lock",
    "HEAD.lock",
];
const BUILT_IN_MERGES: [&str; 6] = ["unspecified", "unset", "set", "text", "binary", "union"];

/// A point inside an Accept after which a test can stop it, as an engine stop would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Recorded,
    FilesWritten,
    BranchMoved,
}

/// Reads the owner's own git identity with no system config: the repository's, then
/// the owner's global config in their home. Runs only `git config --get`, which
/// executes nothing. Inputs: the owner repository and home. Output: name and email,
/// or `changes_owner_identity_missing`.
pub(super) fn owner_identity(
    repository: &Path,
    home: &Path,
) -> Result<(String, String), EngineError> {
    let read = |key: &str| -> Result<String, EngineError> {
        let missing = || failure("changes_owner_identity_missing");
        let output = crate::spawn_gate::output(
            Command::new(crate::research::runner::setup_git())
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("LC_ALL", "C")
                .env("HOME", home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .arg("--git-dir")
                .arg(repository.join(".git"))
                .args(["config", "--get", key])
                .current_dir(repository)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .map_err(|_| missing())?;
        let value = String::from_utf8(output.stdout).map_err(|_| missing())?;
        let value = value.trim_end_matches('\n');
        if !output.status.success()
            || value.trim().is_empty()
            || value.len() > IDENTITY_BYTES_MAX
            || value
                .chars()
                .any(|c| c.is_control() || c == '<' || c == '>')
        {
            return Err(missing());
        }
        Ok(value.to_owned())
    };
    Ok((read("user.name")?, read("user.email")?))
}

/// Says whether any path has a filter attribute, or a merge attribute naming a driver,
/// in any of the trees, reading attributes from each tree and the repository's own
/// info/attributes. Inputs: the repository, the paths and the trees. Output: true when
/// one does, so the change cannot land without running a configured command.
fn uses_configured_commands(
    repo: &Repo,
    paths: &[String],
    trees: &[&str],
) -> Result<bool, EngineError> {
    if paths.is_empty() {
        return Ok(false);
    }
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    for tree in trees {
        let output = repo.exec(
            &[
                "check-attr",
                "-z",
                "--stdin",
                "--source",
                tree,
                "filter",
                "merge",
            ],
            Some(&input),
            &[],
            super::git::OUTPUT_BYTES_MAX,
        )?;
        if output.code != 0 || output.overflow {
            return Err(failure("changes_git_failed"));
        }
        let fields: Vec<&[u8]> = output.stdout.split(|byte| *byte == 0).collect();
        for triple in fields.chunks(3).filter(|triple| triple.len() == 3) {
            let value = String::from_utf8_lossy(triple[2]);
            let configured = match triple[1] {
                b"filter" => !matches!(value.as_ref(), "unspecified" | "unset"),
                b"merge" => !BUILT_IN_MERGES.contains(&value.as_ref()),
                _ => false,
            };
            if configured {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Three-way merges a change set's work onto a commit with git's own merge and no
/// merge driver. Inputs: the repository, the merge base and both sides.
/// Output: the merged tree, holding conflict markers, and the conflicting paths.
fn merge(
    repo: &Repo,
    base: &str,
    ours: &str,
    theirs: &str,
) -> Result<(String, Vec<String>), EngineError> {
    let output = repo.exec(
        &[
            "merge-tree",
            "--write-tree",
            "-z",
            "--name-only",
            "--no-messages",
            "--merge-base",
            base,
            ours,
            theirs,
        ],
        None,
        &[],
        super::git::OUTPUT_BYTES_MAX,
    )?;
    if !matches!(output.code, 0 | 1) || output.overflow {
        return Err(failure("changes_git_failed"));
    }
    let mut fields = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let tree = super::git::object_id(fields.next().ok_or_else(|| failure("changes_git_failed"))?)?;
    let conflicts: Vec<String> = fields
        .map(|path| diff::plain(&String::from_utf8_lossy(path)))
        .collect();
    if (output.code == 1) == conflicts.is_empty() {
        return Err(failure("changes_git_failed"));
    }
    Ok((tree, conflicts))
}

/// Refuses an owner repository in the middle of a merge, rebase, cherry-pick, revert or
/// bisect, or holding git's index or HEAD lock. Input: the owner checkout root. Output:
/// Ok, or `changes_git_busy`.
fn idle(repository: &Path) -> Result<(), EngineError> {
    let metadata = repository.join(".git");
    if BUSY_STATE
        .iter()
        .any(|name| std::fs::symlink_metadata(metadata.join(name)).is_ok())
    {
        return Err(failure("changes_git_busy"));
    }
    Ok(())
}

/// The ref the owner's checkout has checked out. Input: the owner checkout. Output: the
/// `refs/heads/` ref, or None for a detached HEAD.
fn current_branch(owner: &Repo) -> Result<Option<String>, EngineError> {
    let output = owner.exec(&["symbolic-ref", "-q", "HEAD"], None, &[], 1024)?;
    let reference = match output.code {
        0 => String::from_utf8(output.stdout).map_err(|_| failure("changes_git_unsupported"))?,
        1 => return Ok(None),
        _ => return Err(failure("changes_git_failed")),
    };
    let reference = reference.trim_end_matches('\n').to_owned();
    Ok(reference.starts_with("refs/heads/").then_some(reference))
}

/// The owner's checked-out branch and its commit, refusing a detached HEAD or an
/// unfinished git operation. Input: the owner checkout. Output: the ref and commit.
fn owner_branch(owner: &Repo, repository: &Path) -> Result<(String, String), EngineError> {
    idle(repository)?;
    let reference = current_branch(owner)?.ok_or_else(|| failure("changes_detached_head"))?;
    let tip = owner
        .commit_of(&reference)?
        .ok_or_else(|| failure("changes_git_unsupported"))?;
    Ok((reference, tip))
}

/// Where an Accept writes: the owner's checked-out branch through the owner's checkout, a
/// branch checked out nowhere as its ref alone, or an agent's own branch through that
/// agent's linked worktree.
enum Target {
    Checkout,
    Ref,
    Agent(flow::Agent),
}

/// Finds the worktree, other than the owner's checkout, that has a branch checked out.
/// Inputs: the owner checkout and the branch ref. Output: the worktree, or None when no
/// other worktree has it checked out.
fn checked_out_elsewhere(
    owner: &Repo,
    repository: &Path,
    reference: &str,
) -> Result<Option<PathBuf>, EngineError> {
    let listing = owner.capture(
        &["worktree", "list", "--porcelain", "-z"],
        super::git::OUTPUT_BYTES_MAX,
    )?;
    let wanted = format!("branch {reference}");
    let mut current: Option<PathBuf> = None;
    for field in listing.split(|byte| *byte == 0) {
        let field = std::str::from_utf8(field).map_err(|_| failure("changes_git_unsupported"))?;
        if let Some(path) = field.strip_prefix("worktree ") {
            current = Some(PathBuf::from(path));
        } else if field == wanted {
            return Ok(current.filter(|path| path != repository));
        }
    }
    Ok(None)
}

/// Resolves the branch an Accept lands on: the owner's checked-out branch when none is
/// named, else the named local branch, refusing one that is not a plain local branch or
/// that another worktree than an engine agent worktree on its own branch has checked out.
/// Inputs: the store, the owner checkout, its root and the named branch. Output: the ref,
/// its commit and where to write, or `changes_git_busy`, `changes_detached_head`,
/// `changes_target_invalid` or `changes_target_checked_out`.
fn landing_target(
    db: &Connection,
    owner: &Repo,
    repository: &Path,
    target: Option<&str>,
) -> Result<(String, String, Target), EngineError> {
    let Some(name) = target else {
        let (reference, tip) = owner_branch(owner, repository)?;
        return Ok((reference, tip, Target::Checkout));
    };
    idle(repository)?;
    let reference = format!("refs/heads/{name}");
    if name.is_empty()
        || name.len() > BRANCH_BYTES_MAX
        || name.starts_with('-')
        || !owner.test(&["check-ref-format", &reference])?
        || owner.test(&["symbolic-ref", "-q", &reference])?
    {
        return Err(failure("changes_target_invalid"));
    }
    let tip = owner
        .commit_of(&reference)?
        .ok_or_else(|| failure("changes_target_invalid"))?;
    if current_branch(owner)?.as_deref() == Some(reference.as_str()) {
        return Ok((reference, tip, Target::Checkout));
    }
    let Some(worktree) = checked_out_elsewhere(owner, repository, &reference)? else {
        return Ok((reference, tip, Target::Ref));
    };
    let agents = crate::projects_home::state_folder(repository).join("worktrees");
    if worktree.parent() != Some(agents.as_path())
        && worktree.parent() != Some(repository.join("worktrees").as_path())
    {
        return Err(failure("changes_target_checked_out"));
    }
    let agent = flow::linked_git_dir(&worktree)
        .and_then(|git_dir| flow::open_agent(db, AgentKind::Sub, repository, &worktree, &git_dir))
        .map_err(|_| failure("changes_target_checked_out"))?;
    if agent.branch() != Some(name) {
        return Err(failure("changes_target_checked_out"));
    }
    Ok((reference, tip, Target::Agent(agent)))
}

/// The branch the master works on and merges harnesses into (DEC-100), with its tip: the
/// owner's choice, else the repository's default branch, the local branch its `origin`
/// remote's HEAD names, else the branch the owner's checkout has checked out. Inputs: the
/// owner checkout root and the owner's choice, a branch name without `refs/heads/`.
/// Output: the branch and tip, None when no such local branch exists and no choice was
/// made, or `changes_target_invalid` for a choice that is no local branch.
pub(super) fn master_branch(
    repository: &Path,
    chosen: Option<&str>,
) -> Result<Option<(String, String)>, EngineError> {
    let owner = Repo::open(repository, &repository.join(".git"), Some(repository))?;
    let local = |name: &str| -> Result<Option<String>, EngineError> {
        let reference = format!("refs/heads/{name}");
        if name.is_empty()
            || name.len() > BRANCH_BYTES_MAX
            || name.starts_with('-')
            || !owner.test(&["check-ref-format", &reference])?
        {
            return Ok(None);
        }
        owner.commit_of(&reference)
    };
    if let Some(name) = chosen {
        let tip = local(name)?.ok_or_else(|| failure("changes_target_invalid"))?;
        return Ok(Some((name.to_owned(), tip)));
    }
    let remote = owner.exec(
        &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"],
        None,
        &[],
        1024,
    )?;
    let named = std::str::from_utf8(&remote.stdout)
        .ok()
        .filter(|_| remote.code == 0)
        .and_then(|text| {
            text.trim_end_matches('\n')
                .strip_prefix("refs/remotes/origin/")
        })
        .map(str::to_owned);
    if let Some(name) = named
        && let Some(tip) = local(&name)?
    {
        return Ok(Some((name, tip)));
    }
    let Some(reference) = current_branch(&owner)? else {
        return Ok(None);
    };
    let name = reference
        .strip_prefix("refs/heads/")
        .map(str::to_owned)
        .ok_or_else(|| failure("changes_git_unsupported"))?;
    Ok(local(&name)?.map(|tip| (name, tip)))
}

/// Lists the local branches of a change set's repository, in git's order and at most
/// `BRANCHES_MAX`, with the owner's checked-out one. Inputs: the store and the change
/// set. Output: the branches.
pub(super) fn branches(db: &Connection, id: &ChangeSetId) -> Result<TargetBranches, EngineError> {
    store::initialize(db)?;
    let repository = store::load(db, id)?.repository;
    let owner = Repo::open(&repository, &repository.join(".git"), Some(&repository))?;
    let listing = owner.capture(
        &["for-each-ref", "--format=%(refname)", "refs/heads/"],
        super::git::OUTPUT_BYTES_MAX,
    )?;
    let branches: Vec<String> = String::from_utf8_lossy(&listing)
        .lines()
        .filter_map(|line| line.strip_prefix("refs/heads/"))
        .filter(|name| !name.is_empty() && name.len() <= BRANCH_BYTES_MAX)
        .take(BRANCHES_MAX)
        .map(str::to_owned)
        .collect();
    let current = current_branch(&owner)?
        .and_then(|reference| reference.strip_prefix("refs/heads/").map(str::to_owned));
    assert!(branches.len() <= BRANCHES_MAX);
    Ok(TargetBranches { branches, current })
}

/// Refuses an owner checkout with tracked changes, staged or not, or with an untracked
/// file where the squash would write. A submodule counts by the commit it records, so no
/// git runs inside it under config the engine did not write. Inputs: the checkout, its
/// root, the tip and the squash tree. Output: Ok when Accept can write without touching
/// the owner's work.
fn check_clean(owner: &Repo, repository: &Path, tip: &str, tree: &str) -> Result<(), EngineError> {
    if !owner.test(&["diff-index", "--cached", "--quiet", tip, "--"])?
        || !owner.test(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=all",
            "--quiet",
            "--",
        ])?
    {
        return Err(failure("changes_owner_dirty"));
    }
    let added = owner.run(&[
        "diff-tree",
        "-r",
        "-z",
        "--name-only",
        "--diff-filter=A",
        tip,
        tree,
    ])?;
    for path in added
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).map_err(|_| failure("changes_git_unsupported"))?;
        match std::fs::symlink_metadata(repository.join(path)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(failure("changes_owner_dirty")),
        }
    }
    Ok(())
}

/// Commits the deletion of files the owner's checkout no longer has, and nothing else, as
/// one commit on its checked-out branch: the tree comes from the branch tip in a private
/// index, so nothing else the owner changed goes in; the owner's own index then stages
/// the same deletions and the branch moves by compare and swap. Refuses an index that
/// stages any other path. Inputs: the checkout root, the owner's home, the paths and the
/// message. Output: the commit, the branch and the paths deleted, or
/// `changes_import_none`, `changes_import_staged`, `changes_git_busy`,
/// `changes_detached_head`, `changes_branch_moved` or another git code.
pub(super) fn commit_deletions(
    repository: &Path,
    home: &Path,
    paths: &[&str],
    message: &str,
) -> Result<(String, String, Vec<String>), EngineError> {
    assert!(paths.iter().enumerate().all(|(at, path)| {
        !path.is_empty() && !path.starts_with(':') && !paths[..at].contains(path)
    }));
    let _held = hold(repository);
    let git_dir = repository.join(".git");
    let owner = Repo::open(repository, &git_dir, Some(repository))?;
    let (reference, tip) = owner_branch(&owner, repository)?;
    let mut arguments = vec![
        "ls-tree",
        "-z",
        "--name-only",
        "--full-tree",
        tip.as_str(),
        "--",
    ];
    arguments.extend_from_slice(paths);
    let listing = owner.capture(&arguments, super::git::LISTING_BYTES_MAX)?;
    let listed: Vec<&[u8]> = listing
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .collect();
    let deleted: Vec<&str> = paths
        .iter()
        .copied()
        .filter(|path| {
            listed.contains(&path.as_bytes())
                && matches!(
                    std::fs::symlink_metadata(repository.join(path)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                )
        })
        .collect();
    if deleted.is_empty() {
        return Err(failure("changes_import_none"));
    }
    let staged = owner.exec(
        &[
            "diff-index",
            "--cached",
            "--name-only",
            "-z",
            "--no-renames",
            &tip,
            "--",
        ],
        None,
        &[],
        super::git::LISTING_BYTES_MAX,
    )?;
    if staged.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    if staged.overflow
        || staged
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
            .any(|entry| !deleted.iter().any(|path| path.as_bytes() == entry))
    {
        return Err(failure("changes_import_staged"));
    }
    let folder = git_dir.join(format!(
        "crazyagent-import-{}",
        &crate::engine::owner::random_id()?[..24]
    ));
    std::fs::create_dir(&folder).map_err(|_| failure("changes_git_failed"))?;
    let tree = Repo::open(repository, &git_dir, Some(repository)).and_then(|repo| {
        let repo = repo.private_index(&folder.join("index"));
        repo.run(&["read-tree", &tip])?;
        let mut arguments = vec!["update-index", "--force-remove", "--"];
        arguments.extend_from_slice(&deleted);
        repo.run(&arguments)?;
        super::git::object_id(&repo.run(&["write-tree"])?)
    });
    let _ = std::fs::remove_dir_all(&folder);
    let tree = tree?;
    assert_ne!(tree, owner.tree_of(&tip)?);
    let identity = owner_identity(repository, home)?;
    let identity = (identity.0.as_str(), identity.1.as_str());
    let commit = owner.commit_tree(&tree, &[&tip], message, identity)?;
    let mut arguments = vec!["update-index", "--force-remove", "--"];
    arguments.extend_from_slice(&deleted);
    if owner.exec(&arguments, None, &[], 4096)?.code != 0 {
        return Err(failure("changes_git_busy"));
    }
    let env = [
        ("GIT_COMMITTER_NAME", identity.0),
        ("GIT_COMMITTER_EMAIL", identity.1),
    ];
    let moved = owner.exec(
        &[
            "update-ref",
            "-m",
            "CrazyAgent: commit the instruction import",
            &reference,
            &commit,
            &tip,
        ],
        None,
        &env,
        256,
    )?;
    if moved.code != 0 {
        return Err(failure("changes_branch_moved"));
    }
    let branch = reference
        .strip_prefix("refs/heads/")
        .unwrap_or(&reference)
        .to_owned();
    Ok((
        commit,
        branch,
        deleted.into_iter().map(str::to_owned).collect(),
    ))
}

/// Marks a change set landed as `landed`, closes its agent's workers' open sets with it
/// (F3, DEC-107), drops its pending ref and points the agent's worktree at the landed
/// commit when the worktree holds nothing newer than the set. A worker set that cannot be
/// closed now is logged by its plain code and left open, so the Accept itself never fails
/// on it.
fn finish(db: &Connection, record: &mut Record, landed: &str) -> Result<(), EngineError> {
    record.state = ChangeState::Accepted;
    record.landed = Some(landed.to_owned());
    record.conflicts.clear();
    record.intent = None;
    record.updated_ms = flow::now_ms()?;
    store::save(db, record)?;
    if let Err(error) = super::subagent::close_with_lead(db, record) {
        eprintln!(
            "landing_settle_failed cause={}",
            crate::protocol::plain_code(&error.0)
        );
    }
    flow::set_engine_ref(
        &shared(&record.repository)?,
        &record.repository,
        &pending_ref(&record.id),
        None,
    )?;
    let head = record.head.clone();
    reset_if_quiet(db, record, &head, landed)
}

/// The owner's edited Accept message: line ends made `\n`, trimmed, not empty, at most
/// `MESSAGE_BYTES_MAX` bytes and without a character `memory::hidden_text` finds. Input:
/// the text. Output: the message, else `changes_message_invalid`.
fn edited(text: &str) -> Result<String, EngineError> {
    let message = text.replace("\r\n", "\n");
    let message = message.trim();
    if message.is_empty()
        || message.len() > MESSAGE_BYTES_MAX
        || crate::memory::hidden_text(message)
    {
        return Err(failure("changes_message_invalid"));
    }
    Ok(message.to_owned())
}

/// Lands a change set as one squash commit on the owner's checked-out branch: refuses
/// an unusable checkout or a configured filter or merge driver, merges with merge-tree
/// from the merge base of the branch tip and the set's head, so only what would change
/// on that branch lands (DEC-107), refuses a merge that completes a bare-repository
/// layout (SR1-1), makes the commit with the owner's identity, records the intent, writes
/// the index and files with a two-tree read-tree and moves the branch by compare and
/// swap. Inputs: the store, the owner's home, the change set, the reviewed head, the
/// owner's edited message and where a test stops it. Output: the landed commit and
/// branch, or the conflicts.
pub(super) fn accept(
    db: &Connection,
    home: &Path,
    id: &ChangeSetId,
    expected_head: &str,
    message: Option<&str>,
    stopped: &dyn Fn(Step) -> bool,
) -> Result<AcceptOutcome, EngineError> {
    accept_to(db, home, id, expected_head, message, None, stopped)
}

/// Lands a change set as one squash commit on a target branch, the owner's checked-out
/// branch when none is named: as [`accept`] through the owner's checkout, by moving the
/// ref alone for a branch checked out nowhere, or through an agent's worktree for that
/// agent's own branch. Inputs: as [`accept`], plus the target branch. Output: the landed
/// commit and branch, or the conflicts.
pub(super) fn accept_to(
    db: &Connection,
    home: &Path,
    id: &ChangeSetId,
    expected_head: &str,
    message: Option<&str>,
    target: Option<&str>,
    stopped: &dyn Fn(Step) -> bool,
) -> Result<AcceptOutcome, EngineError> {
    let edited = message.map(edited).transpose()?;
    store::initialize(db)?;
    let repository = store::load(db, id)?.repository;
    let _held = hold(&repository);
    let mut record = settled(db, id)?;
    if !matches!(record.state, ChangeState::Pending | ChangeState::Conflict)
        || record.head != expected_head
        || record.intent.is_some()
    {
        return Err(failure("changes_stale"));
    }
    flow::check_places(&record.repository, &record.worktree, &record.git_dir)?;
    let git_dir = repository.join(".git");
    let owner = Repo::open(&repository, &git_dir, Some(&repository))?;
    let (reference, tip, target) = landing_target(db, &owner, &repository, target)?;
    verify_objects(&repository, &[&record.head, &tip], &[&record.base])?;
    let from = measured_from(&owner, &record.base, &record.head, &tip)?;
    let paths = diff::changed_paths(&owner, &from, &record.head)?;
    if uses_configured_commands(&owner, &paths, &[&from, &tip, &record.head])? {
        return Err(failure("changes_git_unsupported"));
    }
    let (tree, conflicts) = merge(&owner, &from, &tip, &record.head)?;
    let branch = reference.trim_start_matches("refs/heads/").to_owned();
    if !conflicts.is_empty() {
        record.state = ChangeState::Conflict;
        record.conflicts = conflicts.clone();
        record.updated_ms = flow::now_ms()?;
        store::save(db, &record)?;
        return Ok(AcceptOutcome::Conflict { files: conflicts });
    }
    if uses_configured_commands(&owner, &paths, &[&tree])? {
        return Err(failure("changes_git_unsupported"));
    }
    flow::check_layout(&owner, &from, &record.head, &tree)?;
    match &target {
        Target::Checkout => check_clean(&owner, &repository, &tip, &tree)?,
        Target::Ref => {}
        Target::Agent(agent) => {
            if agent.head()? != tip || !flow::holds_only(&agent.repo, &tip)? {
                return Err(failure("changes_agent_busy"));
            }
        }
    }
    if tree == owner.tree_of(&tip)? {
        finish(db, &mut record, &tip)?;
        return Ok(AcceptOutcome::Landed {
            commit: tip,
            branch,
        });
    }
    let identity = owner_identity(&repository, home)?;
    let message = edited.unwrap_or_else(|| record.message());
    let identity = (identity.0.as_str(), identity.1.as_str());
    let commit = owner.commit_tree(&tree, &[&tip], &format!("{message}\n"), identity)?;
    verify_objects(&repository, &[&commit], &[&tip, &record.head, &record.base])?;
    record.intent = Some(Intent::Land {
        reference: reference.clone(),
        old: tip.clone(),
        new: commit.clone(),
    });
    store::save(db, &record)?;
    if stopped(Step::Recorded) {
        return Err(failure("changes_git_failed"));
    }
    let written = match &target {
        Target::Checkout => Some((&owner, "changes_owner_dirty")),
        Target::Ref => None,
        Target::Agent(agent) => Some((&agent.repo, "changes_agent_busy")),
    };
    if let Some((checkout, refused)) = written
        && checkout
            .exec(&["read-tree", "-u", "-m", &tip, &commit], None, &[], 4096)?
            .code
            != 0
    {
        record.intent = None;
        store::save(db, &record)?;
        return Err(failure(refused));
    }
    if stopped(Step::FilesWritten) {
        return Err(failure("changes_git_failed"));
    }
    let env = [
        ("GIT_COMMITTER_NAME", identity.0),
        ("GIT_COMMITTER_EMAIL", identity.1),
    ];
    let message = format!("CrazyAgent: accept {}", id.as_str());
    let moved = match &target {
        Target::Checkout | Target::Ref => {
            owner
                .exec(
                    &["update-ref", "-m", &message, &reference, &commit, &tip],
                    None,
                    &env,
                    256,
                )?
                .code
                == 0
        }
        Target::Agent(agent) => agent
            .move_head(Some(tip.as_str()), &commit, &message)
            .is_ok(),
    };
    if !moved {
        let checkout = match &target {
            Target::Checkout => Some(&owner),
            Target::Ref => None,
            Target::Agent(agent) => Some(&agent.repo),
        };
        if let Some(checkout) = checkout
            && checkout.test(&["diff-index", "--cached", "--quiet", &commit, "--"])?
        {
            let _ = checkout.exec(&["read-tree", "-u", "-m", &commit, &tip], None, &[], 4096);
        }
        record.intent = None;
        store::save(db, &record)?;
        return Err(failure("changes_branch_moved"));
    }
    if stopped(Step::BranchMoved) {
        return Err(failure("changes_git_failed"));
    }
    finish(db, &mut record, &commit)?;
    Ok(AcceptOutcome::Landed { commit, branch })
}

/// Merges the owner's branch tip into a change set's agent worktree with conflict
/// markers as an engine merge commit, from the merge base of the tip and the set's head,
/// so the agent can resolve them in its next turn. Inputs: the store and the change set.
/// Output: Ok once the worktree holds the merge.
pub(super) fn update(db: &Connection, id: &ChangeSetId) -> Result<(), EngineError> {
    store::initialize(db)?;
    let repository = store::load(db, id)?.repository;
    let _held = hold(&repository);
    let mut record = settled(db, id)?;
    if !matches!(record.state, ChangeState::Pending | ChangeState::Conflict)
        || record.intent.is_some()
    {
        return Err(failure("changes_stale"));
    }
    let git_dir = repository.join(".git");
    let agent = if flow::in_clone(&record) {
        flow::check_places(&record.repository, &record.worktree, &record.git_dir)?;
        let fetched = clone::fetch(
            &shared(&repository)?,
            &repository,
            &record.worktree,
            &flow::clone_branch(db, &record.worktree)?,
            &crate::worktrees::TREE_BOUNDS,
        )?;
        if fetched != record.head {
            return Err(failure("changes_agent_busy"));
        }
        None
    } else {
        let agent = flow::agent(db, &record)?;
        if agent.head()? != record.head || !flow::holds_only(&agent.repo, &record.head)? {
            return Err(failure("changes_agent_busy"));
        }
        Some(agent)
    };
    let owner = Repo::open(&repository, &git_dir, Some(&repository))?;
    let (_, tip) = owner_branch(&owner, &repository)?;
    verify_objects(&repository, &[&record.head, &tip], &[&record.base])?;
    let from = measured_from(&owner, &record.base, &record.head, &tip)?;
    let paths = diff::changed_paths(&owner, &from, &record.head)?;
    if uses_configured_commands(&owner, &paths, &[&from, &tip, &record.head])? {
        return Err(failure("changes_git_unsupported"));
    }
    let (tree, conflicts) = merge(&owner, &from, &tip, &record.head)?;
    let message = "Merge the owner's branch into this agent's work\n";
    let merged = owner.commit_tree(&tree, &[&record.head, &tip], message, ENGINE)?;
    verify_objects(&repository, &[&merged], &[&record.head, &tip, &record.base])?;
    record.state = ChangeState::Updating;
    record.waiting = true;
    record.base = tip;
    record.head = merged.clone();
    record.conflicts = conflicts;
    let stats = diff::stats(&owner, &record.base, &record.head)?;
    (record.files, record.added, record.removed) = (stats.files, stats.added, stats.removed);
    record.updated_ms = flow::now_ms()?;
    flow::set_engine_ref(&owner, &repository, &pending_ref(id), Some(&merged))?;
    let Some(agent) = agent else {
        return store::save(db, &record);
    };
    record.intent = Some(Intent::Reset {
        target: merged.clone(),
    });
    store::save(db, &record)?;
    flow::reset_agent(&agent, &merged)?;
    record.intent = None;
    store::save(db, &record)
}

/// Finishes or undoes owner-checkout and worktree work an engine stop interrupted, one
/// change set at a time: a change set that cannot be settled now is logged by its plain
/// code and stays in flight for the next run, so it never stops the others. Input: the
/// store. Output: Ok once every change set it could settle holds no intent.
pub(super) fn reconcile(db: &Connection) -> Result<(), EngineError> {
    store::initialize(db)?;
    for found in store::in_flight(db)? {
        let _held = hold(&found.repository);
        let settled = store::load(db, &found.id).and_then(|mut record| settle(db, &mut record));
        if let Err(error) = settled {
            eprintln!(
                "landing_reconcile_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
        }
    }
    Ok(())
}

/// Loads a change set for an owner action, first settling an intent a failed or stopped
/// Accept, update or restore left on it, so the owner's next click repairs the change set
/// instead of finding it stale until the next upkeep. The caller holds [`hold`] on its
/// repository. Inputs: the store and the change set. Output: the record, holding no intent.
pub(super) fn settled(db: &Connection, id: &ChangeSetId) -> Result<Record, EngineError> {
    let mut record = store::load(db, id)?;
    if record.intent.is_some() {
        settle(db, &mut record)?;
        record = store::load(db, id)?;
    }
    assert!(record.intent.is_none());
    Ok(record)
}

/// Finishes or undoes the one Accept or worktree reset a change set holds as its intent:
/// an Accept whose branch moved is finished, one whose files were written but whose
/// branch did not move is moved by compare and swap, one that wrote nothing is dropped,
/// and an agent worktree reset is run again while the worktree still holds the old or new
/// commit. The caller holds [`hold`] on its repository. Inputs: the store and the loaded
/// record. Output: Ok once the stored record holds no intent.
fn settle(db: &Connection, record: &mut Record) -> Result<(), EngineError> {
    match record.intent.clone() {
        Some(Intent::Land {
            reference,
            old,
            new,
        }) => {
            let owner = Repo::open(
                &record.repository,
                &record.repository.join(".git"),
                Some(&record.repository),
            )?;
            let tip = owner.commit_of(&reference)?;
            let written = tip.as_deref() == Some(old.as_str())
                && owner.test(&["diff-index", "--cached", "--quiet", &new, "--"])?;
            let moved = written
                && owner
                    .exec(
                        &[
                            "update-ref",
                            "-m",
                            "CrazyAgent: accept",
                            &reference,
                            &new,
                            &old,
                        ],
                        None,
                        &[],
                        256,
                    )?
                    .code
                    == 0;
            if moved || tip.as_deref() == Some(new.as_str()) {
                finish(db, record, &new)?;
            } else {
                record.intent = None;
                store::save(db, record)?;
            }
        }
        Some(Intent::Reset { target }) => {
            let agent = match flow::agent(db, record) {
                Err(refused) if refused.0 == "changes_workspace_invalid" => None,
                opened => Some(opened?),
            };
            let mut reset = false;
            if let Some(agent) = &agent
                && let Ok(current) = agent.head()
                && (flow::holds_only(&agent.repo, &target)?
                    || flow::holds_only(&agent.repo, &current)?)
            {
                flow::reset_agent(agent, &target)?;
                reset = true;
            }
            if !record.is_open() {
                let start = if reset { &target } else { &record.head };
                store::set_base(db, &record.worktree, start)?;
            }
            record.intent = None;
            store::save(db, record)?;
        }
        None => {}
    }
    assert!(record.intent.is_none());
    Ok(())
}

/// What the owner's branch tip leaves of one open change set's work (DEC-107): the two
/// trees its diff shows, the tip and the tree merging the work onto it makes when that
/// merge is clean and adds something, else the change from where it is measured against
/// the tip to its head, and the counts the Changes view shows of the same.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shown {
    range: (String, String),
    counts: (u32, u32, u32),
}

/// The commit a change set's work is measured from against a branch tip (DEC-107): the
/// merge base of the tip and the set's head, so work the tip already holds is never shown,
/// counted or landed again, whatever the set started from; the stored base for a head that
/// shares no history with the tip. Inputs: the repository, the stored base, the head and
/// the tip. Output: the commit.
pub(super) fn measured_from(
    owner: &Repo,
    base: &str,
    head: &str,
    tip: &str,
) -> Result<String, EngineError> {
    let output = owner.exec(&["merge-base", tip, head], None, &[], 256)?;
    match output.code {
        0 => super::git::object_id(&output.stdout),
        1 => Ok(base.to_owned()),
        _ => Err(failure("changes_git_failed")),
    }
}

/// The tip of the owner's checked-out branch, None while it cannot be read: a detached
/// HEAD, an unfinished git operation or a failed read. Inputs: the owner repository and
/// its root. Output: the tip.
pub(super) fn checked_out_tip(owner: &Repo, repository: &Path) -> Option<String> {
    owner_branch(owner, repository).ok().map(|(_, tip)| tip)
}

/// What judging one open change set against the owner's branch tip found.
enum Judged {
    Landed,
    Removed,
    Open(Shown),
}

/// The change set judgements made for a repository, base, head and tip, newest last, so a
/// Changes read whose owner branch and change sets did not move runs one head check for
/// the whole repository and no git for each set.
type JudgedKey = (PathBuf, String, String, String);
static JUDGED: Mutex<VecDeque<(JudgedKey, Shown)>> = Mutex::new(VecDeque::new());

/// Says whether a change set's agent copy is gone, read from the disk with no git run: its
/// worktree folder or its git folder no longer exists, or an agent's own clone no longer
/// holds its branch. Inputs: the store and the change set. Output: the answer.
fn copy_gone(db: &Connection, record: &Record) -> Result<bool, EngineError> {
    let missing = |path: &Path| matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
    if missing(&record.worktree) || missing(&record.git_dir) {
        return Ok(true);
    }
    if flow::in_clone(record) {
        let branch = flow::clone_branch(db, &record.worktree)?;
        return Ok(!clone::has_branch(&record.worktree, &branch)?);
    }
    Ok(false)
}

/// Judges one open change set whose head commit exists against the owner's branch tip with
/// engine git only, reading and writing objects but never a work tree: landed when the tip
/// holds its head or merging its work onto the tip changes nothing, else the remainder a
/// clean merge adds. An empty change, a conflicting merge or a configured filter or merge
/// driver leaves the whole change shown, measured from the merge base of the tip and its
/// head (DEC-107). Inputs: the owner repository, the change set and the tip. Output: the
/// judgement.
fn judge(owner: &Repo, record: &Record, tip: &str) -> Result<Judged, EngineError> {
    let whole = |from: &str| -> Result<Judged, EngineError> {
        let stats = diff::stats(owner, from, &record.head)?;
        Ok(Judged::Open(Shown {
            range: (from.to_owned(), record.head.clone()),
            counts: (stats.files, stats.added, stats.removed),
        }))
    };
    if owner.tree_of(&record.base)? == owner.tree_of(&record.head)? {
        return whole(&record.base);
    }
    if owner.test(&["merge-base", "--is-ancestor", &record.head, tip])? {
        return Ok(Judged::Landed);
    }
    let from = measured_from(owner, &record.base, &record.head, tip)?;
    let paths = diff::changed_paths(owner, &from, &record.head)?;
    if uses_configured_commands(owner, &paths, &[&from, tip, &record.head])? {
        return whole(&from);
    }
    let (tree, conflicts) = merge(owner, &from, tip, &record.head)?;
    if !conflicts.is_empty() {
        return whole(&from);
    }
    if tree == owner.tree_of(tip)? {
        return Ok(Judged::Landed);
    }
    assert!(super::git::is_object_id(&tree));
    let stats = diff::stats(owner, tip, &tree)?;
    Ok(Judged::Open(Shown {
        range: (tip.to_owned(), tree),
        counts: (stats.files, stats.added, stats.removed),
    }))
}

/// Judges a change set: removed when its head commit is gone, else the judgement made for
/// the same repository, base, head and tip when one is kept. A head the repository's one
/// batch check found is not checked again; any other head is checked on its own. Inputs:
/// the owner repository, the change set, the tip and the heads found. Output: the
/// judgement.
fn judged(
    owner: &Repo,
    record: &Record,
    tip: &str,
    present: &BTreeSet<String>,
) -> Result<Judged, EngineError> {
    if !present.contains(&record.head)
        && owner.commit_of(&record.head)?.as_deref() != Some(record.head.as_str())
    {
        return Ok(Judged::Removed);
    }
    let key = (
        record.repository.clone(),
        record.base.clone(),
        record.head.clone(),
        tip.to_owned(),
    );
    let kept = JUDGED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(seen, _)| *seen == key)
        .map(|(_, shown)| shown.clone());
    if let Some(shown) = kept {
        return Ok(Judged::Open(shown));
    }
    let found = judge(owner, record, tip)?;
    if let Judged::Open(shown) = &found {
        let mut recent = JUDGED.lock().unwrap_or_else(PoisonError::into_inner);
        recent.retain(|(seen, _)| *seen != key);
        recent.push_back((key, shown.clone()));
        while recent.len() > JUDGED_MAX {
            recent.pop_front();
        }
        assert!(recent.len() <= JUDGED_MAX);
    }
    Ok(found)
}

/// Closes a change set whose agent copy, branch or commit is gone as removed outside the
/// app: discarded, never restorable, its pending ref dropped while the owner repository is
/// still there. Inputs: the store and the open change set. Output: Ok once closed.
fn close_removed(db: &Connection, record: &mut Record) -> Result<(), EngineError> {
    assert!(record.is_open() && record.intent.is_none());
    record.state = ChangeState::Discarded;
    record.outside = true;
    record.restorable_until_ms = None;
    record.pruned = true;
    record.waiting = false;
    record.conflicts.clear();
    record.updated_ms = flow::now_ms()?;
    store::save(db, record)?;
    if std::fs::symlink_metadata(record.repository.join(".git")).is_ok_and(|found| found.is_dir()) {
        flow::set_engine_ref(
            &shared(&record.repository)?,
            &record.repository,
            &pending_ref(&record.id),
            None,
        )?;
    }
    Ok(())
}

/// Notices what the owner did outside the app to one open change set. The caller holds
/// [`hold`] on its repository. A gone agent copy closes it as removed outside; against the
/// owner's branch tip, when one is readable, a pending or conflicting set the tip holds
/// closes as landed outside like an Accept onto that tip, and otherwise its counts become
/// what is not yet on the tip. Inputs: the store, the change set, the owner repository
/// with its tip, None while the owner's HEAD is detached, busy or unreadable, and the
/// heads its batch check found. Output: the two trees its diff shows against the tip
/// (DEC-107), None to show its stored change.
fn observe_one(
    db: &Connection,
    id: &ChangeSetId,
    owner: Option<&(Repo, String)>,
    present: &BTreeSet<String>,
) -> Result<Option<(String, String)>, EngineError> {
    let mut record = settled(db, id)?;
    if !record.is_open() {
        return Ok(None);
    }
    if copy_gone(db, &record)? {
        close_removed(db, &mut record)?;
        return Ok(None);
    }
    let Some((owner, tip)) = owner else {
        return Ok(None);
    };
    if !matches!(record.state, ChangeState::Pending | ChangeState::Conflict) {
        return Ok(None);
    }
    match judged(owner, &record, tip, present)? {
        Judged::Removed => {
            close_removed(db, &mut record)?;
            Ok(None)
        }
        Judged::Landed => {
            record.outside = true;
            finish(db, &mut record, tip)?;
            assert!(!record.is_open());
            Ok(None)
        }
        Judged::Open(shown) => {
            let (files, added, removed) = shown.counts;
            if (record.files, record.added, record.removed) != shown.counts {
                (record.files, record.added, record.removed) = (files, added, removed);
                store::save(db, &record)?;
            }
            Ok(Some(shown.range))
        }
    }
}

/// Opens the owner repository for object and ref work and reads its checked-out branch
/// tip. Input: the owner checkout root. Output: the repository and tip, None while its
/// HEAD is detached, an operation is unfinished or it cannot be read; an unreadable one is
/// logged by its plain code.
fn owner_tip(repository: &Path) -> Option<(Repo, String)> {
    let found = shared(repository)
        .and_then(|owner| owner_branch(&owner, repository).map(|(_, tip)| (owner, tip)));
    match found {
        Ok(found) => Some(found),
        Err(error) => {
            if !matches!(
                error.0.as_str(),
                "changes_git_busy" | "changes_detached_head"
            ) {
                eprintln!(
                    "landing_observe_failed cause={}",
                    crate::protocol::plain_code(&error.0)
                );
            }
            None
        }
    }
}

/// Notices what the owner did outside the app to every open change set of one project, or
/// of every project, one repository at a time under its hold, reading each owner tip once
/// and checking every open head with one git run, after closing lead workers' sets their
/// lead's closed set left open (DEC-107).
/// A change set that cannot be judged now is logged by its plain code and left as it was.
/// Inputs: the store and the project, None for every project. Output: Ok once each was
/// judged or left.
pub(super) fn observe(db: &Connection, project: Option<&ProjectId>) -> Result<(), EngineError> {
    store::initialize(db)?;
    let mut by_repository: BTreeMap<PathBuf, (Vec<ChangeSetId>, Vec<String>)> = BTreeMap::new();
    for record in store::open_of(db, project)? {
        let (ids, heads) = by_repository.entry(record.repository).or_default();
        ids.push(record.id);
        heads.push(record.head);
    }
    for (repository, (ids, heads)) in by_repository {
        let _held = hold(&repository);
        if let Err(error) = super::subagent::close_orphaned(db, &ids) {
            eprintln!(
                "landing_settle_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
        }
        let owner = owner_tip(&repository);
        let present = match &owner {
            Some((repo, _)) => {
                let asked: Vec<&str> = heads.iter().map(String::as_str).collect();
                repo.commits_among(&asked).unwrap_or_else(|error| {
                    eprintln!(
                        "landing_observe_failed cause={}",
                        crate::protocol::plain_code(&error.0)
                    );
                    BTreeSet::new()
                })
            }
            None => BTreeSet::new(),
        };
        for id in ids {
            if let Err(error) = observe_one(db, &id, owner.as_ref(), &present) {
                eprintln!(
                    "landing_observe_failed cause={}",
                    crate::protocol::plain_code(&error.0)
                );
            }
        }
    }
    Ok(())
}

/// Notices what the owner did outside the app to one change set before its diff is read.
/// Inputs: the store and the change set. Output: the two trees to show against the owner's
/// branch tip, None to show its stored change, also once it closed.
pub(super) fn observe_for_diff(
    db: &Connection,
    id: &ChangeSetId,
) -> Result<Option<(String, String)>, EngineError> {
    let repository = store::load(db, id)?.repository;
    let _held = hold(&repository);
    let owner = owner_tip(&repository);
    observe_one(db, id, owner.as_ref(), &BTreeSet::new())
}
