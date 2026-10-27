use super::diff;
use super::flow::{self, ENGINE, hold, pending_ref, reset_if_quiet, shared};
use super::git::{Repo, failure};
use super::store::{self, Intent, Record};
use super::{AcceptOutcome, ChangeSetId, ChangeState};
use crate::engine::EngineError;
use rusqlite::Connection;
use std::path::Path;
use std::process::{Command, Stdio};

const IDENTITY_BYTES_MAX: usize = 256;
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
fn owner_identity(repository: &Path, home: &Path) -> Result<(String, String), EngineError> {
    let read = |key: &str| -> Result<String, EngineError> {
        let missing = || failure("changes_owner_identity_missing");
        let output = crate::spawn_gate::output(
            Command::new("/usr/bin/git")
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

/// The owner's checked-out branch and its commit, refusing a detached HEAD or an
/// unfinished git operation. Input: the owner checkout. Output: the ref and commit.
fn owner_branch(owner: &Repo, repository: &Path) -> Result<(String, String), EngineError> {
    let metadata = repository.join(".git");
    if BUSY_STATE
        .iter()
        .any(|name| std::fs::symlink_metadata(metadata.join(name)).is_ok())
    {
        return Err(failure("changes_git_busy"));
    }
    let output = owner.exec(&["symbolic-ref", "-q", "HEAD"], None, &[], 1024)?;
    let reference = match output.code {
        0 => String::from_utf8(output.stdout).map_err(|_| failure("changes_git_unsupported"))?,
        1 => return Err(failure("changes_detached_head")),
        _ => return Err(failure("changes_git_failed")),
    };
    let reference = reference.trim_end_matches('\n').to_owned();
    if !reference.starts_with("refs/heads/") {
        return Err(failure("changes_detached_head"));
    }
    let tip = owner
        .commit_of(&reference)?
        .ok_or_else(|| failure("changes_git_unsupported"))?;
    Ok((reference, tip))
}

/// Refuses an owner checkout with tracked changes, staged or not, or with an untracked
/// file where the squash would write. Inputs: the checkout, its root, the tip and the
/// squash tree. Output: Ok when Accept can write without touching the owner's work.
fn check_clean(owner: &Repo, repository: &Path, tip: &str, tree: &str) -> Result<(), EngineError> {
    if !owner.test(&["diff-index", "--cached", "--quiet", tip, "--"])?
        || !owner.test(&["diff", "--no-ext-diff", "--no-textconv", "--quiet", "--"])?
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

/// Marks a change set landed as `landed`, drops its pending ref and points the agent's
/// worktree at the landed commit when the worktree holds nothing newer than the set.
fn finish(db: &Connection, record: &mut Record, landed: &str) -> Result<(), EngineError> {
    record.state = ChangeState::Accepted;
    record.landed = Some(landed.to_owned());
    record.conflicts.clear();
    record.intent = None;
    record.updated_ms = flow::now_ms()?;
    store::save(db, record)?;
    shared(&record.repository)?.run(&["update-ref", "-d", &pending_ref(&record.id)])?;
    let head = record.head.clone();
    reset_if_quiet(db, record, &head, landed)
}

/// Lands a change set as one squash commit on the owner's checked-out branch: refuses
/// an unusable checkout or a configured filter or merge driver, merges with
/// merge-tree, makes the commit with the owner's identity, records the intent, writes
/// the index and files with a two-tree read-tree and moves the branch by compare and
/// swap. Inputs: the store, the owner's home, the change set, the reviewed head and
/// where a test stops it. Output: the landed commit and branch, or the conflicts.
pub(super) fn accept(
    db: &Connection,
    home: &Path,
    id: &ChangeSetId,
    expected_head: &str,
    stopped: &dyn Fn(Step) -> bool,
) -> Result<AcceptOutcome, EngineError> {
    store::initialize(db)?;
    let repository = store::load(db, id)?.repository;
    let _held = hold(&repository);
    let mut record = store::load(db, id)?;
    if !matches!(record.state, ChangeState::Pending | ChangeState::Conflict)
        || record.head != expected_head
        || record.intent.is_some()
    {
        return Err(failure("changes_stale"));
    }
    flow::check_places(&record.repository, &record.worktree, &record.git_dir)?;
    let owner = Repo::open(&repository.join(".git"), Some(&repository))?;
    let (reference, tip) = owner_branch(&owner, &repository)?;
    let paths = diff::changed_paths(&owner, &record.base, &record.head)?;
    if uses_configured_commands(&owner, &paths, &[&record.base, &tip, &record.head])? {
        return Err(failure("changes_git_unsupported"));
    }
    let (tree, conflicts) = merge(&owner, &record.base, &tip, &record.head)?;
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
    check_clean(&owner, &repository, &tip, &tree)?;
    if tree == owner.tree_of(&tip)? {
        finish(db, &mut record, &tip)?;
        return Ok(AcceptOutcome::Landed {
            commit: tip,
            branch,
        });
    }
    let identity = owner_identity(&repository, home)?;
    let subject = if record.prompt_line.is_empty() {
        format!("Changes from {}", flow::subject(&record.agent.label))
    } else {
        record.prompt_line.clone()
    };
    let identity = (identity.0.as_str(), identity.1.as_str());
    let commit = owner.commit_tree(&tree, &[&tip], &format!("{subject}\n"), identity)?;
    record.intent = Some(Intent::Land {
        reference: reference.clone(),
        old: tip.clone(),
        new: commit.clone(),
    });
    store::save(db, &record)?;
    if stopped(Step::Recorded) {
        return Err(failure("changes_git_failed"));
    }
    let written = owner.exec(&["read-tree", "-u", "-m", &tip, &commit], None, &[], 4096)?;
    if written.code != 0 {
        record.intent = None;
        store::save(db, &record)?;
        return Err(failure("changes_owner_dirty"));
    }
    if stopped(Step::FilesWritten) {
        return Err(failure("changes_git_failed"));
    }
    let env = [
        ("GIT_COMMITTER_NAME", identity.0),
        ("GIT_COMMITTER_EMAIL", identity.1),
    ];
    let message = format!("CrazyAgent: accept {}", id.as_str());
    let moved = owner.exec(
        &["update-ref", "-m", &message, &reference, &commit, &tip],
        None,
        &env,
        256,
    )?;
    if moved.code != 0 {
        if owner.test(&["diff-index", "--cached", "--quiet", &commit, "--"])? {
            let _ = owner.exec(&["read-tree", "-u", "-m", &commit, &tip], None, &[], 4096);
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
/// markers as an engine merge commit, so the agent can resolve them in its next turn.
/// Inputs: the store and the change set. Output: Ok once the worktree holds the merge.
pub(super) fn update(db: &Connection, id: &ChangeSetId) -> Result<(), EngineError> {
    store::initialize(db)?;
    let repository = store::load(db, id)?.repository;
    let _held = hold(&repository);
    let mut record = store::load(db, id)?;
    if !matches!(record.state, ChangeState::Pending | ChangeState::Conflict)
        || record.intent.is_some()
    {
        return Err(failure("changes_stale"));
    }
    let agent = flow::agent(&record)?;
    if agent.commit_of("HEAD")?.as_deref() != Some(record.head.as_str())
        || !flow::holds_only(&agent, &record.head)?
    {
        return Err(failure("changes_agent_busy"));
    }
    let owner = Repo::open(&repository.join(".git"), Some(&repository))?;
    let (_, tip) = owner_branch(&owner, &repository)?;
    let paths = diff::changed_paths(&owner, &record.base, &record.head)?;
    if uses_configured_commands(&owner, &paths, &[&record.base, &tip, &record.head])? {
        return Err(failure("changes_git_unsupported"));
    }
    let (tree, conflicts) = merge(&owner, &record.base, &tip, &record.head)?;
    let message = "Merge the owner's branch into this agent's work\n";
    let merged = owner.commit_tree(&tree, &[&record.head, &tip], message, ENGINE)?;
    record.state = ChangeState::Updating;
    record.base = tip;
    record.head = merged.clone();
    record.conflicts = conflicts;
    let stats = diff::stats(&owner, &record.base, &record.head)?;
    (record.files, record.added, record.removed) = (stats.files, stats.added, stats.removed);
    record.updated_ms = flow::now_ms()?;
    owner.run(&["update-ref", &pending_ref(id), &merged])?;
    record.intent = Some(Intent::Reset {
        target: merged.clone(),
    });
    store::save(db, &record)?;
    flow::reset_agent(&agent, &merged)?;
    record.intent = None;
    store::save(db, &record)
}

/// Finishes or undoes owner-checkout and worktree work an engine stop interrupted:
/// an Accept whose branch moved is finished, one whose files were written but whose
/// branch did not move is moved by compare and swap, one that wrote nothing is
/// dropped, and an agent worktree reset is run again while the worktree still holds
/// the old or new commit. Input: the store. Output: Ok once nothing is in flight.
pub(super) fn reconcile(db: &Connection) -> Result<(), EngineError> {
    store::initialize(db)?;
    for found in store::in_flight(db)? {
        let _held = hold(&found.repository);
        let mut record = store::load(db, &found.id)?;
        match record.intent.clone() {
            Some(Intent::Land {
                reference,
                old,
                new,
            }) => {
                let owner = Repo::open(&record.repository.join(".git"), Some(&record.repository))?;
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
                    finish(db, &mut record, &new)?;
                } else {
                    record.intent = None;
                    store::save(db, &record)?;
                }
            }
            Some(Intent::Reset { target }) => {
                let agent = flow::agent(&record)?;
                let current = agent
                    .commit_of("HEAD")?
                    .ok_or_else(|| failure("changes_git_failed"))?;
                let reset =
                    flow::holds_only(&agent, &target)? || flow::holds_only(&agent, &current)?;
                if reset {
                    flow::reset_agent(&agent, &target)?;
                }
                if !record.is_open() {
                    let start = if reset { &target } else { &record.head };
                    store::set_base(db, &record.worktree, start)?;
                }
                record.intent = None;
                store::save(db, &record)?;
            }
            None => {}
        }
    }
    Ok(())
}
