use super::clone;
use super::diff;
use super::flow::{self, ENGINE, hold, pending_ref, reset_if_quiet, shared, verify_objects};
use super::git::{Repo, Writes, failure};
use super::store::{self, Intent, Record};
use super::{
    AcceptOutcome, AgentKind, BRANCH_BYTES_MAX, BRANCHES_MAX, ChangeSetId, ChangeState,
    MESSAGE_BYTES_MAX, TargetBranches,
};
use crate::activity::{ActivityKind, ENGINE_ACTOR, OWNER_ACTOR};
use crate::agent_git::Layout;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};

const IDENTITY_BYTES_MAX: usize = 256;
const MAIN_PLACES_MAX: usize = 512;
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

/// A point inside an Accept, an undo or the master's own land after which a test can stop
/// it, as an engine stop would. `Gated` comes after the master's set has passed the checks
/// that let it land and before the land starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Gated,
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

/// The paths a merge of a change set onto a tip can touch: those the agent changed and
/// those main changed since the set's merge base, each side of a rename apart, so the
/// path git merges an edit at after main renamed the file is checked too. Inputs: the
/// repository, the merge base, the tip and the set's head. Output: the paths, sorted once.
pub(super) fn merge_paths(
    repo: &Repo,
    from: &str,
    tip: &str,
    head: &str,
) -> Result<Vec<String>, EngineError> {
    let mut paths = diff::changed_paths(repo, from, head)?;
    paths.extend(diff::changed_paths(repo, from, tip)?);
    paths.sort_unstable();
    paths.dedup();
    Ok(paths)
}

/// Says whether any path has a filter attribute, or a merge attribute naming a driver,
/// in any of the trees, reading attributes from each tree and the repository's own
/// info/attributes. Inputs: the repository, the paths and the trees. Output: true when
/// one does, so the change cannot land without running a configured command.
pub(super) fn uses_configured_commands(
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
pub(super) fn merge(
    repo: &Repo,
    base: &str,
    ours: &str,
    theirs: &str,
) -> Result<(String, Vec<String>), EngineError> {
    let (tree, conflicts) = merge_raw(repo, base, ours, theirs)?;
    let conflicts = conflicts
        .iter()
        .map(|path| diff::plain(&String::from_utf8_lossy(path)))
        .collect();
    Ok((tree, conflicts))
}

/// As [`merge`], with the conflicting paths exactly as git printed them, for comparing
/// with another git listing. Inputs: the repository, the merge base and both sides.
/// Output: the merged tree and the conflicting paths.
pub(super) fn merge_raw(
    repo: &Repo,
    base: &str,
    ours: &str,
    theirs: &str,
) -> Result<(String, Vec<Vec<u8>>), EngineError> {
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
    let conflicts: Vec<Vec<u8>> = fields.map(<[u8]>::to_vec).collect();
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

/// The short name of the branch a checkout has checked out, for the owner to read. Input:
/// the checkout root. Output: the name, none for a detached HEAD or a name over
/// `BRANCH_BYTES_MAX`, or a plain git code.
pub(super) fn checked_out_branch(checkout: &Path) -> Result<Option<String>, EngineError> {
    let owner = Repo::open(checkout, &checkout.join(".git"), None)?;
    let name = current_branch(&owner)?
        .and_then(|reference| reference.strip_prefix("refs/heads/").map(str::to_owned))
        .filter(|name| !name.is_empty() && name.len() <= BRANCH_BYTES_MAX);
    Ok(name)
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
pub(super) enum Target {
    Checkout,
    Ref,
    Agent(Box<flow::Agent>),
}

/// The checkout an Accept fills and the code it refuses with: the owner's, or an agent's
/// linked worktree confined to itself, since its agent may still be changing it. Inputs:
/// the target and the owner's repository. Output: the checkout and its code, none for a ref
/// alone, or what confining the worktree reported.
pub(super) fn filled_checkout(
    target: &Target,
    owner: &Repo,
) -> Result<Option<(Repo, &'static str)>, EngineError> {
    Ok(match target {
        Target::Checkout => Some((owner.clone(), "changes_owner_dirty")),
        Target::Ref => None,
        Target::Agent(agent) => {
            Some((agent.repo.confined(Writes::WorkTree)?, "changes_agent_busy"))
        }
    })
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
    Ok((reference, tip, Target::Agent(Box::new(agent))))
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
    branch_of(&owner, chosen)
}

/// Resolves the Master's branch in an open owner repository as [`master_branch`] says.
/// Inputs: the owner repository and the owner's choice. Output: as [`master_branch`].
fn branch_of(owner: &Repo, chosen: Option<&str>) -> Result<Option<(String, String)>, EngineError> {
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
    let Some(reference) = current_branch(owner)? else {
        return Ok(None);
    };
    let name = reference
        .strip_prefix("refs/heads/")
        .map(str::to_owned)
        .ok_or_else(|| failure("changes_git_unsupported"))?;
    Ok(local(&name)?.map(|tip| (name, tip)))
}

/// Resolves a project's Land on branch as [`land_on`] does, opening the owner repository.
/// Inputs: the store, the owner checkout root and the project. Output: as [`land_on`].
pub(super) fn land_on_branch(
    db: &Connection,
    repository: &Path,
    project: &ProjectId,
) -> Result<Option<(String, String)>, EngineError> {
    let owner = Repo::open(repository, &repository.join(".git"), Some(repository))?;
    land_on(db, &owner, project)
}

/// The branch a project's work lands on, with its tip (DEC-111 Q1): the owner's Land on
/// choice, else the Master's branch as [`master_branch`] resolves it. A store that keeps
/// no team tables has no choice. Inputs: the store, the owner repository and the project.
/// Output: the branch and tip, None when nothing was chosen and no Master's branch
/// exists, or `changes_target_invalid` for a chosen branch that is no local branch.
pub(super) fn land_on(
    db: &Connection,
    owner: &Repo,
    project: &ProjectId,
) -> Result<Option<(String, String)>, EngineError> {
    let kept: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='team_land_on')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| failure("changes_unavailable"))?;
    let chosen = match kept {
        false => None,
        true => match crate::team::land_on(db, project)? {
            Some(branch) => Some(branch),
            None => crate::team::master_branch_choice(db, project)?,
        },
    };
    branch_of(owner, chosen.as_deref())
}

/// Points `refs/crazyagent/main` at the tip of the project's Land on branch for every agent
/// worktree the team records: in each own clone through [`clone::give_main`], with no git
/// run there, and once in the owner repository for linked worktrees, which share its refs.
/// Each repository's tip is read and given under its hold, so a start never writes a tip
/// read before an Accept after that Accept gave the newer one: taken here when `held` is
/// None, else the caller's, and a repository other than the held one is logged and skipped.
/// A worktree or repository it cannot reach is logged by its plain code and skipped, so
/// one never stops the rest. Inputs: the store, the project and the repository the caller
/// holds. Output: the own clones given main once each was given or skipped, else
/// `changes_unavailable` when the team records cannot be read.
pub(super) fn refresh_main(
    db: &Connection,
    project: &ProjectId,
    held: Option<&Path>,
) -> Result<Vec<PathBuf>, EngineError> {
    let places = main_places(db, project)?;
    let mut by_repository: BTreeMap<&Path, Vec<&[PathBuf; 3]>> = BTreeMap::new();
    for place in &places {
        by_repository
            .entry(place[0].as_path())
            .or_default()
            .push(place);
    }
    let mut given = Vec::new();
    for (repository, places) in by_repository {
        let _hold = match held {
            None => Some(hold(repository)),
            Some(own) if own == repository => None,
            Some(_) => {
                eprintln!("landing_main_not_given code=changes_repository_not_held");
                continue;
            }
        };
        given.extend(give_main_in(db, project, repository, &places));
    }
    Ok(given)
}

/// Gives one owner repository's agent worktrees the project's Land on tip as
/// [`refresh_main`] says, logging by plain code each one it cannot reach. The caller holds
/// the repository's hold. Inputs: the store, the project, the repository and its places.
/// Output: the own clones it gave main.
fn give_main_in(
    db: &Connection,
    project: &ProjectId,
    repository: &Path,
    places: &[&[PathBuf; 3]],
) -> Vec<PathBuf> {
    let tip = match land_on_branch(db, repository, project) {
        Ok(found) => found.map(|(_, tip)| tip),
        Err(error) => {
            eprintln!(
                "landing_main_unresolved code={}",
                crate::protocol::plain_code(&error.0)
            );
            None
        }
    };
    let Some(tip) = tip else {
        return Vec::new();
    };
    let mut linked = false;
    let mut clones = Vec::new();
    for [_, worktree, git_dir] in places.iter().copied() {
        let given = match flow::check_places(repository, worktree, git_dir) {
            Ok(Layout::Clone) => {
                clone::give_main(repository, worktree, &tip).map(|()| clones.push(worktree.clone()))
            }
            Ok(Layout::Linked) => {
                linked = true;
                Ok(())
            }
            Err(error) => Err(error),
        };
        if let Err(error) = given {
            eprintln!(
                "landing_main_not_given code={}",
                crate::protocol::plain_code(&error.0)
            );
        }
    }
    if !linked {
        return clones;
    }
    let given = shared(repository).and_then(|repo| {
        flow::set_engine_ref(&repo, repository, clone::MAIN_REF, Some(tip.as_str()))
    });
    if let Err(error) = given {
        eprintln!(
            "landing_main_not_given code={}",
            crate::protocol::plain_code(&error.0)
        );
    }
    clones
}

/// The repository, worktree and git folder of each agent worktree the team records for a
/// project, at most `MAIN_PLACES_MAX`; none for a store that keeps no team tables. Inputs:
/// the store and the project. Output: the places, else `changes_unavailable`.
fn main_places(db: &Connection, project: &ProjectId) -> Result<Vec<[PathBuf; 3]>, EngineError> {
    let unavailable = |_| failure("changes_unavailable");
    let kept: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='team_worktrees')",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !kept {
        return Ok(Vec::new());
    }
    let mut statement = db
        .prepare(
            "SELECT repository,worktree,git_dir FROM team_worktrees WHERE project=?1
             ORDER BY agent LIMIT ?2",
        )
        .map_err(unavailable)?;
    let limit = i64::try_from(MAIN_PLACES_MAX).map_err(|_| failure("changes_unavailable"))?;
    let places = statement
        .query_map(rusqlite::params![project.as_str(), limit], |row| {
            Ok([
                PathBuf::from(row.get::<_, String>(0)?),
                PathBuf::from(row.get::<_, String>(1)?),
                PathBuf::from(row.get::<_, String>(2)?),
            ])
        })
        .map_err(unavailable)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(unavailable)?;
    assert!(places.len() <= MAIN_PLACES_MAX);
    Ok(places)
}

/// The tip of the branch a project's work lands on, None while it cannot be read: an
/// unfinished git operation in the owner's checkout, no such branch, or a failed read,
/// logged by its plain code. Inputs: the store, the owner repository opened for object
/// work, its root and the project. Output: the tip.
fn land_on_tip(
    db: &Connection,
    owner: &Repo,
    repository: &Path,
    project: &ProjectId,
) -> Option<String> {
    match idle(repository).and_then(|()| land_on(db, owner, project)) {
        Ok(found) => found.map(|(_, tip)| tip),
        Err(error) => {
            if error.0 != "changes_git_busy" {
                eprintln!(
                    "landing_land_on_failed cause={}",
                    crate::protocol::plain_code(&error.0)
                );
            }
            None
        }
    }
}

/// Lists the local branches of a change set's repository, in git's order and at most
/// `BRANCHES_MAX`, with the owner's checked-out one. Inputs: the store and the change
/// set. Output: the branches.
pub(super) fn branches(db: &Connection, id: &ChangeSetId) -> Result<TargetBranches, EngineError> {
    store::initialize(db)?;
    let record = store::load(db, id)?;
    let repository = record.repository;
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
    let (land_on, land_on_problem) = match land_on(db, &owner, &record.project) {
        Ok(found) => (found.map(|(branch, _)| branch), None),
        Err(error) => (None, Some(land_on_problem(db, &record.project, &error))),
    };
    Ok(TargetBranches {
        branches,
        current,
        land_on,
        land_on_problem,
    })
}

/// The plain sentence the Accept branch list carries when a project's Land on branch
/// cannot be resolved (DEC-111 Q1): the setting that names the missing branch and what to
/// do, or that the setting could not be read. Inputs: the store, the project and the
/// failure. Output: the sentence.
fn land_on_problem(db: &Connection, project: &ProjectId, error: &EngineError) -> String {
    let unread =
        "Your Land on branch could not be read. Pick a branch, or check Land on in Team settings.";
    if error.0 != "changes_target_invalid" {
        return unread.to_owned();
    }
    match crate::team::land_on(db, project) {
        Ok(Some(name)) => format!(
            "Your Land on branch {name} is not a local branch of this project. Pick a branch, or change Land on in Team settings."
        ),
        Ok(None) => match crate::team::master_branch_choice(db, project) {
            Ok(Some(name)) => format!(
                "Land on uses the Master's branch {name}, which is not a local branch of this project. Pick a branch, or change the Master's branch in Team settings."
            ),
            Ok(None) | Err(_) => unread.to_owned(),
        },
        Err(_) => unread.to_owned(),
    }
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
/// stages any other path. Inputs: the checkout root, the owner's home, the paths, the
/// message and what runs after the index is staged and before the branch moves, where a
/// test moves the branch. Output: the commit, the branch and the paths deleted, or
/// `changes_import_none`, `changes_import_staged`, `changes_git_busy`,
/// `changes_detached_head`, `changes_branch_moved` or another git code.
pub(super) fn commit_deletions(
    repository: &Path,
    home: &Path,
    paths: &[&str],
    message: &str,
    before_move: &dyn Fn(),
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
    before_move();
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
/// (F3, DEC-107), drops its pending ref, forgets the owner edits settled on it and points the
/// agent's worktree at the landed commit when the worktree holds nothing newer than the set,
/// gives every agent worktree the new main ([`refresh_main`]), tells the set's clone agent,
/// only when its clone was given that main, that the set landed ([`tell_landed`]), and
/// records the Accept in the project's activity by who accepted it. A worker set that cannot
/// be closed now, or a main that cannot be given, is logged by its plain code and left, so
/// the Accept itself never fails on it; so is a failure after the save for the master's own
/// land, which its caller still lists as landed.
fn finish(
    db: &Connection,
    record: &mut Record,
    landed: &str,
    by: By<'_>,
) -> Result<(), EngineError> {
    record.state = ChangeState::Accepted;
    record.accepted_in = match by {
        By::Owner(place) => place,
        By::Named(_) | By::Master => None,
    };
    record.landed_on = match &record.intent {
        Some(Intent::Land { reference, new, .. }) if new == landed => Some(reference.clone()),
        _ => None,
    };
    record.made = record.landed_on.is_some();
    record.landed = Some(landed.to_owned());
    record.conflicts.clear();
    record.intent = None;
    record.updated_ms = flow::now_ms()?;
    store::save(db, record)?;
    crate::roadmap_status::mark_stale(db, &record.project);
    crate::project_overview::mark_landed(db, &record.project);
    flow::release_agent_claims(db, record);
    if let Some(by) = by.named() {
        let closed = if record.agent.kind == AgentKind::Master {
            close_covered(db, record, landed)
        } else {
            Vec::new()
        };
        super::note_accepted(db, record, by, &closed);
    }
    let owner_pressed = matches!(by, By::Owner(_));
    if let Err(error) = super::subagent::close_with_lead(db, record, owner_pressed) {
        eprintln!(
            "landing_settle_failed cause={}",
            crate::protocol::plain_code(&error.0)
        );
    }
    match (tidy(db, record, landed), by) {
        (Err(error), By::Master) => {
            eprintln!(
                "master_land_tidy_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
            Ok(())
        }
        (tidied, _) => tidied,
    }
}

/// Drops a landed change set's pending ref, forgets the owner edits settled on it and
/// points its agent's worktree at the landed commit when the worktree holds nothing newer
/// than the set. Inputs: the store, the landed set and its landed commit. Output: Ok once
/// done.
fn tidy(db: &Connection, record: &mut Record, landed: &str) -> Result<(), EngineError> {
    flow::set_engine_ref(
        &shared(&record.repository)?,
        &record.repository,
        &pending_ref(&record.id),
        None,
    )?;
    super::owner_edit::logged("forget", super::owner_edit::forget(db, &record.id));
    let head = record.head.clone();
    reset_if_quiet(db, record, &head, landed)?;
    match refresh_main(db, &record.project, Some(record.repository.as_path())) {
        Ok(given) if given.contains(&record.worktree) => tell_landed(db, record, landed),
        Ok(_) => {}
        Err(error) => eprintln!(
            "landing_main_not_refreshed code={}",
            crate::protocol::plain_code(&error.0)
        ),
    }
    Ok(())
}

/// Tells the sub-agent of an accepted set in its own clone, which the engine never resets
/// (CC-N1), that the set landed, what its last commit was, so `git log <head>..HEAD` shows
/// any newer work, and to start from the main it was just given with `git reset --keep`,
/// which keeps uncommitted edits (OS-8, DEC-117): one Note from the engine keyed to the set,
/// naming no merge, since the agent's git keeps the "Catch up with main" it launched with. A
/// failed send is logged by its plain code and never fails the Accept. Inputs: the store,
/// the accepted set and the landed commit. Output: none.
pub(super) fn tell_landed(db: &Connection, record: &Record, landed: &str) {
    if !flow::in_clone(record) || record.agent.kind != AgentKind::Sub {
        return;
    }
    let Ok(agent) = record.agent.id.parse::<i64>() else {
        return;
    };
    let head = record.head.get(..12).unwrap_or(&record.head);
    let body = format!(
        "Your change set landed on main as {}; its last commit was {head}. If git log {head}..HEAD shows no newer commits, start your next work from main: git reset --keep crazyagent/main.",
        landed.get(..12).unwrap_or(landed)
    );
    let note = crate::coord::NewMessage {
        project: record.project.clone(),
        from: crate::coord::Party::Engine,
        to: crate::coord::Party::Agent(agent),
        kind: crate::coord::MessageKind::Note,
        question: None,
        work: None,
        reply_to: None,
        body,
        idempotency_key: Some(format!("landed-{}", record.id.as_str())),
    };
    if let Err(error) = crate::coord::send(db, note, crate::team::now_ms()) {
        eprintln!(
            "landing_note_not_sent code={}",
            crate::protocol::plain_code(&error.0)
        );
    }
}

/// Who lands a change set, as the project's activity names them: the owner, with where
/// Accept was pressed when known, or someone else, each with the Accept or conflict recorded
/// as it happens, or the master landing its own set, whose land its caller records once with
/// the sets it closed and whose conflict leaves the set as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum By<'a> {
    Owner(Option<crate::activity::AcceptPlace>),
    Named(&'a str),
    Master,
}

impl<'a> By<'a> {
    /// Who the project's activity names as acting now. Output: the actor, None for the
    /// master's own land, which its caller records.
    fn named(self) -> Option<&'a str> {
        match self {
            Self::Owner(_) => Some(OWNER_ACTOR),
            Self::Named(by) => Some(by),
            Self::Master => None,
        }
    }
}

/// Lands the master's own pending change set when `team::master_lands` says Merges and
/// "Ask me before merging" is off, both read now, and only when the set holds nothing but
/// fixes of real conflicts with other open sets of the project (MASTER-LAND-SPLIT). Lands
/// it as an Accept onto the Land on branch, then [`master_merged`] closes those sets and
/// lists the merge. Any other set is left for the owner's Accept, and a conflict changes
/// nothing. The checks, the land and the close run under one hold, so an owner action
/// cannot come between them (T3-R2-L1). Inputs: the store, the owner's home, the change set
/// and where a test stops it. Output: the outcome, or None when nothing was tried.
pub(super) fn land_master(
    db: &Connection,
    home: &Path,
    id: &ChangeSetId,
    stopped: &dyn Fn(Step) -> bool,
) -> Result<Option<AcceptOutcome>, EngineError> {
    store::initialize(db)?;
    let repository = store::load(db, id)?.repository;
    let _held = hold(&repository);
    let record = store::load(db, id)?;
    if record.agent.kind != AgentKind::Master
        || record.state != ChangeState::Pending
        || record.intent.is_some()
        || crate::team::master_lands(db, &record.project)? != crate::team::MasterLands::Merges
        || crate::team::ask_before_merge(db, &record.project)?
    {
        return Ok(None);
    }
    let sets = covered(db, &record)?;
    if sets.is_empty() || !only_fixes(db, &record, &sets)? {
        return Ok(None);
    }
    if stopped(Step::Gated) {
        return Err(failure("changes_git_failed"));
    }
    let outcome = accept_held(db, home, id, &record.head, None, None, By::Master, stopped)?;
    if let AcceptOutcome::Landed { commit, branch } = &outcome {
        master_merged(db, &record, commit, branch);
    }
    Ok(Some(outcome))
}

/// Tells whether the master's set holds nothing but conflict fixes, against the branch it
/// would land on, after the checks an Accept makes before reading objects an agent could
/// have planted. The caller holds [`hold`]. Inputs: the store, the master's set and the
/// sets its head holds. Output: the answer.
fn only_fixes(db: &Connection, master: &Record, sets: &[Record]) -> Result<bool, EngineError> {
    let repository = &master.repository;
    flow::check_places(repository, &master.worktree, &master.git_dir)?;
    let owner = Repo::open(repository, &repository.join(".git"), Some(repository))?;
    let named = land_on(db, &owner, &master.project)?.map(|(branch, _)| branch);
    let (_, tip, _) = landing_target(db, &owner, repository, named.as_deref())?;
    verify_objects(repository, &[&master.head, &tip], &[&master.base])?;
    let from = measured_from(db, &owner, master, &tip)?;
    super::merges::only_conflict_fixes(&owner, master, sets, &tip, &from)
}

/// Records a landed master set the way the project lists it: closes every other open set
/// of the project its head holds, then writes one MergedIntoMain row by the master naming
/// them. A failure is logged by its plain code and never hides the land. The caller holds
/// [`hold`] on the repository. Inputs: the store, the master's set, the commit it landed
/// as and the branch it landed on. Output: none.
fn master_merged(db: &Connection, master: &Record, landed: &str, branch: &str) {
    let closed = close_covered(db, master, landed);
    let mut text = format!("Master merged {} into {branch}", super::named(db, master));
    text.push_str(&super::closed_words(&closed));
    let item = crate::activity::NewActivity {
        kind: ActivityKind::MergedIntoMain,
        actor: crate::activity::MASTER_ACTOR,
        agent: None,
        goal: None,
        text: &text,
        change_set: Some(master.id.as_str()),
    };
    let at = flow::now_ms().map_or(i64::MAX, |ms| i64::try_from(ms).unwrap_or(i64::MAX));
    crate::activity::note(db, &master.project, &item, at);
}

/// The other open change sets of the master's project whose head the master's set holds.
/// A set with an owner action in flight is left out. Inputs: the store and the master's
/// set. Output: the sets.
fn covered(db: &Connection, master: &Record) -> Result<Vec<Record>, EngineError> {
    let mut covered = Vec::new();
    for record in store::open_of(db, Some(&master.project))? {
        assert_eq!(record.project, master.project);
        if record.id != master.id
            && record.intent.is_none()
            && super::subagent::covers(&record, &master.head)?
        {
            covered.push(record);
        }
    }
    Ok(covered)
}

/// Closes, as landed in `landed`, every set [`covered`] finds for the master's landed set
/// whose clean changes reached it, the way a lead's merge closes its workers' sets; any
/// other stays open for the owner's Accept or Discard. A set that fails to close is logged
/// by its plain code and named only when the store holds it closed. The caller holds
/// [`hold`] on the repository. Inputs: the store, the master's landed set and the commit it
/// landed as. Output: the labels of the agents whose sets closed.
fn close_covered(db: &Connection, master: &Record, landed: &str) -> Vec<String> {
    assert_eq!(master.agent.kind, AgentKind::Master);
    let logged = |error: &EngineError| {
        eprintln!(
            "master_close_failed cause={}",
            crate::protocol::plain_code(&error.0)
        );
    };
    let sets = match covered(db, master) {
        Ok(sets) => sets,
        Err(error) => {
            logged(&error);
            return Vec::new();
        }
    };
    let owner = match shared(&master.repository) {
        Ok(owner) => owner,
        Err(error) => {
            logged(&error);
            return Vec::new();
        }
    };
    let found = sets.len();
    let mut closed = Vec::new();
    for mut record in sets {
        match super::merges::holds_set_work(&owner, landed, &record.base, &record.head) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                logged(&error);
                continue;
            }
        }
        let shut = match super::subagent::land_covered(db, &mut record, landed) {
            Ok(()) => true,
            Err(error) => {
                logged(&error);
                store::load(db, &record.id).is_ok_and(|stored| !stored.is_open())
            }
        };
        if shut {
            closed.push(record.agent.label.clone());
        }
    }
    assert!(closed.len() <= found);
    closed
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
/// layout or a link chain out of the project (SR1-1, SR1-2), makes the commit with the
/// owner's identity, records the intent, writes the index and files with a two-tree
/// read-tree and moves the branch by compare and swap. Inputs: the store, the owner's
/// home, the change set, the reviewed head, the owner's edited message, where the owner
/// pressed Accept and where a test stops it. Output: the landed commit and branch, or the
/// conflicts.
pub(super) fn accept(
    db: &Connection,
    home: &Path,
    id: &ChangeSetId,
    expected_head: &str,
    message: Option<&str>,
    place: crate::activity::AcceptPlace,
    stopped: &dyn Fn(Step) -> bool,
) -> Result<AcceptOutcome, EngineError> {
    accept_to(
        db,
        home,
        id,
        expected_head,
        message,
        None,
        By::Owner(Some(place)),
        stopped,
    )
}

/// Lands a change set as one squash commit on a target branch, the project's Land on
/// branch when none is named (DEC-111 Q1): as [`accept`] through the owner's checkout, by
/// moving the ref alone for a branch checked out nowhere, or through an agent's worktree
/// for that agent's own branch. Anyone but the owner is refused with
/// [`super::WORKER_OPEN`] while a worker below the set's agent holds an open set, since only
/// the owner's Accept may close that set with it (DEC-107). Inputs: as [`accept`], plus the
/// target branch and who accepts, as the project's activity names them. Output: the landed
/// commit and branch, or the conflicts.
#[allow(clippy::too_many_arguments)]
pub(super) fn accept_to(
    db: &Connection,
    home: &Path,
    id: &ChangeSetId,
    expected_head: &str,
    message: Option<&str>,
    target: Option<&str>,
    by: By<'_>,
    stopped: &dyn Fn(Step) -> bool,
) -> Result<AcceptOutcome, EngineError> {
    let edited = message.map(edited).transpose()?;
    store::initialize(db)?;
    let repository = store::load(db, id)?.repository;
    let _held = flow::hold_unmoved(db, &repository)?;
    accept_held(db, home, id, expected_head, edited, target, by, stopped)
}

/// The body of [`accept_to`], for a caller that already holds [`hold`] on the repository.
/// Inputs: as [`accept_to`], with the message already checked. Output: the landed commit
/// and branch, or the conflicts.
#[allow(clippy::too_many_arguments)]
fn accept_held(
    db: &Connection,
    home: &Path,
    id: &ChangeSetId,
    expected_head: &str,
    edited: Option<String>,
    target: Option<&str>,
    by: By<'_>,
    stopped: &dyn Fn(Step) -> bool,
) -> Result<AcceptOutcome, EngineError> {
    let mut record = settled(db, id)?;
    let repository = record.repository.clone();
    if !matches!(record.state, ChangeState::Pending | ChangeState::Conflict)
        || record.head != expected_head
        || record.intent.is_some()
    {
        return Err(failure("changes_stale"));
    }
    if !matches!(by, By::Owner(_)) && super::subagent::has_open_worker(db, &record)? {
        return Err(failure(super::WORKER_OPEN));
    }
    flow::check_places(&record.repository, &record.worktree, &record.git_dir)?;
    let git_dir = repository.join(".git");
    let owner = Repo::open(&repository, &git_dir, Some(&repository))?;
    let named = match target {
        Some(target) => Some(target.to_owned()),
        None => land_on(db, &owner, &record.project)?.map(|(branch, _)| branch),
    };
    let (reference, tip, target) = landing_target(db, &owner, &repository, named.as_deref())?;
    verify_objects(&repository, &[&record.head, &tip], &[&record.base])?;
    let from = measured_from(db, &owner, &record, &tip)?;
    let paths = merge_paths(&owner, &from, &tip, &record.head)?;
    if uses_configured_commands(&owner, &paths, &[&from, &tip, &record.head])? {
        return Err(failure("changes_git_unsupported"));
    }
    let (tree, conflicts) = merge(&owner, &from, &tip, &record.head)?;
    let branch = reference.trim_start_matches("refs/heads/").to_owned();
    if !conflicts.is_empty() {
        if let Some(by) = by.named() {
            record.state = ChangeState::Conflict;
            record.conflicts = conflicts.clone();
            record.updated_ms = flow::now_ms()?;
            store::save(db, &record)?;
            super::note_change(db, &record, ActivityKind::Conflict, by);
        }
        return Ok(AcceptOutcome::Conflict { files: conflicts });
    }
    if uses_configured_commands(&owner, &paths, &[&tree])? {
        return Err(failure("changes_git_unsupported"));
    }
    flow::check_merged(&owner, &repository, &from, &record.head, &tree)?;
    match &target {
        Target::Checkout => check_clean(&owner, &repository, &tip, &tree)?,
        Target::Ref => {}
        Target::Agent(agent) => {
            let teammates = format!("/{}/", crate::agent_git::TEAMMATES_FOLDER);
            if agent.head()? != tip || !flow::holds_only_except(&agent.repo, &tip, &[&teammates])? {
                return Err(failure("changes_agent_busy"));
            }
        }
    }
    if tree == owner.tree_of(&tip)? {
        finish(db, &mut record, &tip, by)?;
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
        master: by == By::Master,
    });
    store::save(db, &record)?;
    if stopped(Step::Recorded) {
        return Err(failure("changes_git_failed"));
    }
    let written = filled_checkout(&target, &owner)?;
    if let Some((checkout, refused)) = &written
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
        if let Some((checkout, _)) = &written
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
    finish(db, &mut record, &commit, by)?;
    Ok(AcceptOutcome::Landed { commit, branch })
}

/// Reverts the commit a change set's own Accept made, on the branch that Accept moved,
/// through the owner's checkout or as its ref alone: merge-tree from the landed commit onto
/// the branch tip with the landed commit's parent as the other side, the same checkout and
/// configured-command checks as an Accept, one commit `Undo: <landed subject>` with the
/// owner's identity, the intent recorded before the checkout is written, and a
/// compare-and-swap move of the branch. Then [`undone`] records it. A conflict changes
/// nothing. Inputs: the store, the owner's home, the change set and where a test stops it.
/// Output: the undo commit and branch, the conflicts, or `changes_undo_unavailable` for a
/// set whose Accept made no commit, that is undone already, whose commit is not on that
/// branch or that lands in an agent's branch.
pub(super) fn undo(
    db: &Connection,
    home: &Path,
    id: &ChangeSetId,
    stopped: &dyn Fn(Step) -> bool,
) -> Result<AcceptOutcome, EngineError> {
    store::initialize(db)?;
    let repository = store::load(db, id)?.repository;
    let _held = flow::hold_unmoved(db, &repository)?;
    let mut record = settled(db, id)?;
    let unavailable = || failure("changes_undo_unavailable");
    let (landed, named) = match (&record.landed, &record.landed_on) {
        (Some(landed), Some(reference))
            if record.state == ChangeState::Accepted && record.made && record.undone.is_none() =>
        {
            let named = reference
                .strip_prefix("refs/heads/")
                .ok_or_else(unavailable)?;
            (landed.clone(), named.to_owned())
        }
        _ => return Err(unavailable()),
    };
    let git_dir = repository.join(".git");
    let owner = Repo::open(&repository, &git_dir, Some(&repository))?;
    let (reference, tip, target) = landing_target(db, &owner, &repository, Some(named.as_str()))?;
    assert_eq!(record.landed_on.as_deref(), Some(reference.as_str()));
    if matches!(target, Target::Agent(_))
        || !owner.test(&["merge-base", "--is-ancestor", &landed, &tip])?
    {
        return Err(unavailable());
    }
    let parent = owner
        .commit_of(&format!("{landed}^1"))?
        .ok_or_else(unavailable)?;
    let paths = diff::changed_paths(&owner, &parent, &landed)?;
    if uses_configured_commands(&owner, &paths, &[&parent, &tip, &landed])? {
        return Err(failure("changes_git_unsupported"));
    }
    let (tree, conflicts) = merge(&owner, &landed, &tip, &parent)?;
    let branch = reference.trim_start_matches("refs/heads/").to_owned();
    if !conflicts.is_empty() {
        return Ok(AcceptOutcome::Conflict { files: conflicts });
    }
    if uses_configured_commands(&owner, &paths, &[&tree])? {
        return Err(failure("changes_git_unsupported"));
    }
    flow::check_merged(&owner, &repository, &landed, &parent, &tree)?;
    if matches!(target, Target::Checkout) {
        check_clean(&owner, &repository, &tip, &tree)?;
    }
    if tree == owner.tree_of(&tip)? {
        return Err(unavailable());
    }
    let subject = owner.run(&["show", "-s", "--format=%s", &landed])?;
    let subject = String::from_utf8_lossy(&subject).trim().to_owned();
    let identity = owner_identity(&repository, home)?;
    let identity = (identity.0.as_str(), identity.1.as_str());
    let commit = owner.commit_tree(&tree, &[&tip], &format!("Undo: {subject}\n"), identity)?;
    verify_objects(&repository, &[&commit], &[&tip, &landed])?;
    record.intent = Some(Intent::Undo {
        reference: reference.clone(),
        old: tip.clone(),
        new: commit.clone(),
    });
    store::save(db, &record)?;
    if stopped(Step::Recorded) {
        return Err(failure("changes_git_failed"));
    }
    if matches!(target, Target::Checkout)
        && owner
            .exec(&["read-tree", "-u", "-m", &tip, &commit], None, &[], 4096)?
            .code
            != 0
    {
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
    let message = format!("CrazyAgent: undo {}", id.as_str());
    let moved = owner.exec(
        &["update-ref", "-m", &message, &reference, &commit, &tip],
        None,
        &env,
        256,
    )?;
    if moved.code != 0 {
        if matches!(target, Target::Checkout)
            && owner.test(&["diff-index", "--cached", "--quiet", &commit, "--"])?
        {
            let _ = owner.exec(&["read-tree", "-u", "-m", &commit, &tip], None, &[], 4096);
        }
        record.intent = None;
        store::save(db, &record)?;
        return Err(failure("changes_branch_moved"));
    }
    if stopped(Step::BranchMoved) {
        return Err(failure("changes_git_failed"));
    }
    undone(db, &mut record, &commit, OWNER_ACTOR)?;
    Ok(AcceptOutcome::Landed { commit, branch })
}

/// Records a finished undo: sets accepted with no commit of their own whose work the revert
/// removed, as [`reverted_work`] says, are marked undone with it, the set keeps the commit
/// and drops its intent, its merge rows show Undone and one Undone row names those sets. The
/// reads every set shares run first, so a failed one stops the step for a retry. Inputs: the
/// store, the undone set, the reverting commit and who undid it. Output: Ok once saved.
fn undone(db: &Connection, record: &mut Record, commit: &str, by: &str) -> Result<(), EngineError> {
    let landed = record
        .landed
        .clone()
        .ok_or_else(|| failure("changes_undo_unavailable"))?;
    let owner = shared(&record.repository)?;
    let above = commits_from(&owner, &landed, commit)?;
    let changed = diff::changed_paths(&owner, &format!("{landed}^1"), &landed)?;
    let before = owner
        .commit_of(&format!("{commit}^1"))?
        .ok_or_else(|| failure("changes_git_failed"))?;
    let mut closed = Vec::new();
    for mut other in store::closed_without_commit(db, &record.project, &record.id)? {
        let Some(at) = other.landed.clone().filter(|at| above.contains(at)) else {
            continue;
        };
        let reverted =
            reverted_work(&owner, [&before, commit], &changed, &other).unwrap_or_else(|error| {
                eprintln!(
                    "landing_undo_closed_set_unread cause={}",
                    crate::protocol::plain_code(&error.0)
                );
                at == landed
            });
        if !reverted {
            continue;
        }
        other.undone = Some(commit.to_owned());
        store::save(db, &other)?;
        closed.push(other.agent.label);
    }
    record.undone = Some(commit.to_owned());
    record.intent = None;
    record.updated_ms = flow::now_ms()?;
    store::save(db, record)?;
    crate::activity::mark_undone(db, &record.project, record.id.as_str());
    super::note_undo(db, record, &closed, by);
    Ok(())
}

/// Lists the commits from a landed one up to the commit reverting it on that line, the
/// landed one included, with one git run. Inputs: the owner repository and both commits.
/// Output: the commits.
fn commits_from(owner: &Repo, landed: &str, commit: &str) -> Result<BTreeSet<String>, EngineError> {
    let range = format!("{landed}..{commit}");
    let listed = owner.run(&["rev-list", "--ancestry-path", &range])?;
    let text = std::str::from_utf8(&listed).map_err(|_| failure("changes_git_failed"))?;
    let mut commits: BTreeSet<String> = text.lines().map(str::to_owned).collect();
    commits.insert(landed.to_owned());
    Ok(commits)
}

/// Tells whether a revert removed a set's work: the reverted commit changed one of the set's
/// files, the branch held the set's work just before the revert and does not at the
/// reverting commit. Inputs: the owner repository, the commits before and after the revert,
/// the paths it changed and the set. Output: the answer, or a read failure of this set.
fn reverted_work(
    owner: &Repo,
    [before, after]: [&str; 2],
    changed: &[String],
    set: &Record,
) -> Result<bool, EngineError> {
    let own = diff::changed_paths(owner, &set.base, &set.head)?;
    if !own.iter().any(|path| changed.contains(path)) {
        return Ok(false);
    }
    Ok(
        super::merges::holds_set_work(owner, before, &set.base, &set.head)?
            && !super::merges::holds_set_work(owner, after, &set.base, &set.head)?,
    )
}

/// Moves an owner ref an Accept or an undo left in flight: by compare and swap when the
/// ref still holds `old` and the owner's index already holds `new`. Inputs: the set, the
/// intent's ref, old and new commits and the reflog message. Output: whether the ref now
/// holds `new`.
fn moved_on(
    record: &Record,
    reference: &str,
    old: &str,
    new: &str,
    message: &str,
) -> Result<bool, EngineError> {
    let owner = Repo::open(
        &record.repository,
        &record.repository.join(".git"),
        Some(&record.repository),
    )?;
    let tip = owner.commit_of(reference)?;
    let written = tip.as_deref() == Some(old)
        && owner.test(&["diff-index", "--cached", "--quiet", new, "--"])?;
    let moved = written
        && owner
            .exec(
                &["update-ref", "-m", message, reference, new, old],
                None,
                &[],
                256,
            )?
            .code
            == 0;
    Ok(moved || tip.as_deref() == Some(new))
}

/// Merges the tip the set lands on into a change set's agent worktree with conflict
/// markers as an engine merge commit named after that branch, from the merge base of the
/// tip and its head, so the agent can resolve them in its next turn: its lead's branch for
/// a lead's worker while Lands is Lead, read before the hold, else the owner's branch.
/// Inputs: the store and the change set. Output: Ok once the worktree holds the merge, or
/// `changes_workspace_invalid` for a lead's worker whose lead's worktree is gone.
pub(super) fn update(db: &Connection, id: &ChangeSetId) -> Result<(), EngineError> {
    store::initialize(db)?;
    let loaded = store::load(db, id)?;
    let repository = loaded.repository.clone();
    let lead = match super::subagent::lead_of(db, &loaded.project, &loaded.agent, &repository)? {
        Some((_, place)) => Some((
            super::subagent::read_lead_tip(db, &place)?,
            super::subagent::lead_branch(db, &place),
        )),
        None => None,
    };
    let _held = flow::hold_unmoved(db, &repository)?;
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
    let (tip, message) = match lead {
        Some((tip, branch)) => (
            tip,
            match branch {
                Some(branch) => {
                    format!("Merge the lead's branch {branch} into this agent's work\n")
                }
                None => String::from("Merge the lead's branch into this agent's work\n"),
            },
        ),
        None => {
            idle(&repository)?;
            let (_, tip) = land_on(db, &owner, &record.project)?
                .ok_or_else(|| failure("changes_detached_head"))?;
            (
                tip,
                String::from("Merge the owner's branch into this agent's work\n"),
            )
        }
    };
    verify_objects(&repository, &[&record.head, &tip], &[&record.base])?;
    let from = measured_from(db, &owner, &record, &tip)?;
    let paths = merge_paths(&owner, &from, &tip, &record.head)?;
    if uses_configured_commands(&owner, &paths, &[&from, &tip, &record.head])? {
        return Err(failure("changes_git_unsupported"));
    }
    let (tree, conflicts) = merge(&owner, &from, &tip, &record.head)?;
    let merged = owner.commit_tree(&tree, &[&record.head, &tip], &message, ENGINE)?;
    verify_objects(&repository, &[&merged], &[&record.head, &tip, &record.base])?;
    let before = record.clone();
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
        from: Some(before.head.clone()),
        target: merged.clone(),
        earlier: Some(store::Earlier::of(&before)),
    });
    store::save(db, &record)?;
    if !flow::reset_agent(&agent, &before.head, &merged)? {
        store::save(db, &before)?;
        flow::set_engine_ref(&owner, &repository, &pending_ref(id), Some(&before.head))?;
        return Err(failure("changes_agent_busy"));
    }
    record.intent = None;
    store::save(db, &record)
}

/// The paths a change set's work conflicts on with the tip it lands on (OS-8): git's own
/// three-way merge of its head onto that tip from where the work is measured, with no
/// merge driver, run in the owner repository only. None while the tip cannot be read or
/// already lies in the head's history. A path it changes that carries an owner-configured
/// filter or merge driver, which that merge would run, is refused before it runs, as
/// `update` refuses it. Inputs: the store and the change set. Output: the conflicting
/// paths, empty for a clean merge, `changes_git_unsupported`, or the plain git code.
pub(super) fn conflicts_with_tip(
    db: &Connection,
    record: &Record,
) -> Result<Vec<String>, EngineError> {
    let owner = Repo::open(&record.repository, &record.repository.join(".git"), None)?;
    let Some(tip) = landing_tip(
        db,
        &owner,
        &record.repository,
        &record.project,
        &record.agent,
    ) else {
        return Ok(Vec::new());
    };
    let from = measured_from(db, &owner, record, &tip)?;
    if from == tip {
        return Ok(Vec::new());
    }
    let paths = merge_paths(&owner, &from, &tip, &record.head)?;
    if uses_configured_commands(&owner, &paths, &[&from, &tip, &record.head])? {
        return Err(failure("changes_git_unsupported"));
    }
    let (_, conflicts) = merge(&owner, &from, &tip, &record.head)?;
    Ok(conflicts)
}

/// Tells whether an agent's change set conflicts with the tip it lands on, so its turn
/// end can send it back to its agent before the owner sees it (OS-8). Only a Pending set
/// of an agent that takes its own resolve task is checked: a master's set keeps the
/// owner's Accept and Update, a lead's worker while its lead merges it, or a native
/// teammate, keeps its lead's merge, which tells the lead the files, and an agent's own
/// clone waits for the owner, since its git takes no merge of the engine's commit. With
/// `after`, a clash in known lockfiles only starts a regeneration off this thread instead
/// ([`super::lockfiles::regenerate_later`]), and the set waits for it, not its agent.
/// Inputs: the store, the change set, what runs after a regeneration that landed nothing
/// and the owner's settings files. Output: the answer, or the plain code of a check that
/// could not run.
pub(super) fn conflicts_with_main(
    db: &Connection,
    id: &ChangeSetId,
    after: Option<super::AfterRegeneration>,
    settings: Option<&(PathBuf, PathBuf)>,
) -> Result<bool, EngineError> {
    let record = store::load(db, id)?;
    if record.state != ChangeState::Pending
        || record.intent.is_some()
        || record.agent.kind == AgentKind::Master
        || flow::in_clone(&record)
    {
        return Ok(false);
    }
    let lead = super::subagent::lead_of(db, &record.project, &record.agent, &record.repository);
    if !matches!(lead, Ok(None))
        || super::subagent::is_teammate(db, &record.project, &record.agent)?
    {
        return Ok(false);
    }
    let conflicts = conflicts_with_tip(db, &record)?;
    if conflicts.is_empty() {
        return Ok(false);
    }
    match after {
        Some(after) => Ok(!super::lockfiles::regenerate_later(
            db, &record, &conflicts, after, settings,
        )?),
        None => Ok(true),
    }
}

/// Finishes or undoes owner-checkout and worktree work an engine stop interrupted, one
/// change set at a time: a change set that cannot be settled now is logged by its plain
/// code and stays in flight for the next run, so it never stops the others. Input: the
/// store. Output: Ok once every change set it could settle holds no intent.
pub(super) fn reconcile(db: &Connection) -> Result<(), EngineError> {
    store::initialize(db)?;
    for found in store::in_flight(db)? {
        let _held = hold(&found.repository);
        let settled = store::load(db, &found.id).and_then(|mut record| {
            let accept = matches!(record.intent, Some(Intent::Land { .. }));
            settle(db, &mut record).map(|()| accept)
        });
        match settled {
            Err(error) => eprintln!(
                "landing_reconcile_failed cause={}",
                crate::protocol::plain_code(&error.0)
            ),
            Ok(true) => list_accept(db, &found.id),
            Ok(false) => {}
        }
    }
    Ok(())
}

/// Lists an Accept an engine stop interrupted in the action ledger once it is settled, so
/// the recovery banner can name it until the owner dismisses it: a pending notice holding
/// the project, the change set and whether it landed (dismissed Done) or was dropped
/// (dismissed Failed). A notice an earlier run left for the change set is ended with the
/// outcome it showed and replaced, in one transaction; a ledger that cannot be written
/// logs its plain code. Inputs: the store and the change set.
fn list_accept(db: &Connection, id: &ChangeSetId) {
    use crate::ledger::{ActionKind, Outcome};
    let unavailable = |_: rusqlite::Error| failure("ledger_unavailable");
    let listed = store::load(db, id).and_then(|record| {
        let root = db
            .path()
            .map(Path::new)
            .and_then(Path::parent)
            .ok_or_else(|| failure("ledger_unavailable"))?;
        let ledger = crate::ledger::open(root)?;
        let payload = serde_json::json!({
            "project": record.project.as_str(),
            "change_set": id.as_str(),
            "landed": record.state == ChangeState::Accepted,
        });
        let held = ledger.unchecked_transaction().map_err(unavailable)?;
        for earlier in crate::ledger::pending(&held, ActionKind::LandingAccept)? {
            if earlier.key == id.as_str() {
                let shown = match serde_json::from_str::<serde_json::Value>(&earlier.payload)
                    .ok()
                    .and_then(|notice| notice.get("landed")?.as_bool())
                {
                    Some(true) => Outcome::Done,
                    Some(false) => Outcome::Failed,
                    None => Outcome::Unknown,
                };
                crate::ledger::finish(&held, earlier.id, shown)?;
            }
        }
        crate::ledger::begin(
            &held,
            ActionKind::LandingAccept,
            id.as_str(),
            &payload.to_string(),
        )?;
        held.commit().map_err(unavailable)
    });
    if let Err(error) = listed {
        eprintln!(
            "landing_ledger_failed cause={}",
            crate::protocol::plain_code(&error.0)
        );
    }
}

/// Loads a change set for an owner action, first settling an intent a failed or stopped
/// Accept, update or restore left on it, so the owner's next click repairs the change set
/// instead of finding it stale until the next upkeep. Nothing is settled or loaded for an
/// action while the project's folder is being moved into the projects home
/// (`move_project_in_progress`), since a step would write a repository that is on its way
/// out. The caller holds [`hold`] on its repository. Inputs: the store and the change set.
/// Output: the record, holding no intent.
pub(super) fn settled(db: &Connection, id: &ChangeSetId) -> Result<Record, EngineError> {
    let mut record = store::load(db, id)?;
    if crate::projects_home::project_move_holds(db, &record.repository)
        .map_err(|_| failure("changes_unavailable"))?
    {
        return Err(failure("move_project_in_progress"));
    }
    if record.intent.is_some() {
        settle(db, &mut record)?;
        record = store::load(db, id)?;
    }
    assert!(record.intent.is_none());
    Ok(record)
}

/// Finishes or undoes the one Accept or worktree reset a change set holds as its intent:
/// an Accept whose branch moved is finished, one whose files were written but whose
/// branch did not move is moved by compare and swap, either listed as the master's merge
/// with its covered sets closed when the master landed its own set, one that wrote nothing is dropped,
/// an undo is finished or dropped the same way, and an agent worktree reset counts as done
/// when HEAD already holds its target, is run again only while HEAD still holds the
/// commit it started from and the worktree holds nothing else, and is dropped otherwise,
/// so a commit the agent made after it stopped is never moved. A closed set's next set
/// opens above the target once the agent's branch holds it and not the set's head, and a
/// restore that reached its target does what the live restore does last. The caller holds
/// [`hold`] on its repository. Inputs: the store and the loaded record. Output: Ok once the
/// stored record holds no intent.
fn settle(db: &Connection, record: &mut Record) -> Result<(), EngineError> {
    match record.intent.clone() {
        Some(Intent::Land {
            reference,
            old,
            new,
            master,
        }) => {
            let landed = moved_on(record, &reference, &old, &new, "CrazyAgent: accept")?;
            if landed && master {
                finish(db, record, &new, By::Master)?;
                master_merged(
                    db,
                    record,
                    &new,
                    reference.trim_start_matches("refs/heads/"),
                );
            } else if landed {
                finish(db, record, &new, By::Named(ENGINE_ACTOR))?;
            } else {
                record.intent = None;
                store::save(db, record)?;
            }
        }
        Some(Intent::Undo {
            reference,
            old,
            new,
        }) => {
            if moved_on(record, &reference, &old, &new, "CrazyAgent: undo")? {
                undone(db, record, &new, ENGINE_ACTOR)?;
            } else {
                record.intent = None;
                store::save(db, record)?;
            }
        }
        Some(Intent::Reset {
            from,
            target,
            earlier,
        }) => {
            let agent = match flow::agent(db, record) {
                Err(refused) if refused.0 == "changes_workspace_invalid" => None,
                opened => Some(opened?),
            };
            let mut reset = false;
            if let Some(agent) = &agent
                && let Ok(current) = agent.head()
            {
                if current == target {
                    reset = true;
                } else if may_finish_reset(agent, record, &current, from.as_deref(), &target)? {
                    reset = flow::reset_settling(agent, &current, &target)?;
                }
            }
            if !record.is_open() {
                let reached = reset
                    || match &agent {
                        Some(agent) => agent.built_on(&target)? && !agent.built_on(&record.head)?,
                        None => false,
                    };
                let start = if reached { &target } else { &record.head };
                flow::set_base(
                    db,
                    &record.repository,
                    &record.worktree,
                    &record.git_dir,
                    start,
                )?;
            } else if let Some(earlier) = earlier {
                let restoring = earlier.was_discarded();
                let reached = reset
                    || match &agent {
                        Some(agent) => agent.built_on(&target)?,
                        None => false,
                    };
                if !reached {
                    put_back(record, earlier)?;
                } else if restoring {
                    flow::finish_restore(db, record)?;
                }
            }
            record.intent = None;
            store::save(db, record)?;
        }
        None => {}
    }
    assert!(record.intent.is_none());
    Ok(())
}

/// Tells whether a stored reset may still move the agent's HEAD: only from the commit it
/// recorded, with the worktree holding one of the two; or, for a step stored before resets
/// recorded that, when the files already hold `target` and HEAD holds the set's head or a
/// commit behind `target`, so no commit of the agent's leaves its branch. Inputs: the
/// agent, the set, HEAD, the recorded start and the target. Output: the answer.
fn may_finish_reset(
    agent: &flow::Agent,
    record: &Record,
    current: &str,
    from: Option<&str>,
    target: &str,
) -> Result<bool, EngineError> {
    if let Some(from) = from {
        return Ok(from == current
            && (flow::holds_only(&agent.repo, target)?
                || flow::holds_only(&agent.repo, current)?));
    }
    Ok(agent.repo.commit_of(target)?.is_some()
        && flow::holds_only(&agent.repo, target)?
        && (current == record.head
            || agent
                .repo
                .test(&["merge-base", "--is-ancestor", current, target])?))
}

/// Puts an open change set back as it was before a reset that did not happen, with its
/// pending ref, as the step's own refusal does. The caller saves the record. Inputs: the
/// set and what it held. Output: Ok once the ref is back.
fn put_back(record: &mut Record, earlier: store::Earlier) -> Result<(), EngineError> {
    earlier.put_back(record);
    let pending = record.is_open().then(|| record.head.clone());
    flow::set_engine_ref(
        &shared(&record.repository)?,
        &record.repository,
        &pending_ref(&record.id),
        pending.as_deref(),
    )
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
/// counted or landed again, whatever the set started from. The stored base instead when
/// the engine's own Accept of an earlier set with that head landed on the tip as a squash
/// commit git cannot see as a merge base, and that base lies between the merge base and
/// the head; also for a head that shares no history with the tip. A landed commit the
/// repository no longer holds, as after the owner resets below it and gc prunes it, counts
/// as not landed. Inputs: the store, the repository, the change set and the tip. Output:
/// the commit.
pub(super) fn measured_from(
    db: &Connection,
    owner: &Repo,
    record: &Record,
    tip: &str,
) -> Result<String, EngineError> {
    measured_base(
        db,
        owner,
        &record.project,
        &record.repository,
        &record.base,
        &record.head,
        tip,
    )
}

/// [`measured_from`] for a set's project, repository, start and head, also before the set
/// is stored, as the turn end's change checks read it (LAND-17-L1). Inputs: the store, the
/// repository opened for objects, the project, the owner checkout, the start, the head and
/// the tip. Output: the commit.
pub(super) fn measured_base(
    db: &Connection,
    owner: &Repo,
    project: &ProjectId,
    repository: &Path,
    base: &str,
    head: &str,
    tip: &str,
) -> Result<String, EngineError> {
    let output = owner.exec(&["merge-base", tip, head], None, &[], 256)?;
    let joined = match output.code {
        0 => super::git::object_id(&output.stdout)?,
        1 => return Ok(base.to_owned()),
        _ => return Err(failure("changes_git_failed")),
    };
    if joined == base {
        return Ok(joined);
    }
    let Some(landed) = store::landed_head(db, project, repository, base)? else {
        return Ok(joined);
    };
    if owner.commit_of(&landed)?.as_deref() != Some(landed.as_str()) {
        return Ok(joined);
    }
    let ancestor =
        |older: &str, newer: &str| owner.test(&["merge-base", "--is-ancestor", older, newer]);
    if ancestor(&landed, tip)? && ancestor(&joined, base)? && ancestor(base, head)? {
        return Ok(base.to_owned());
    }
    Ok(joined)
}

/// The tip a change set is measured against, the branch it lands on (DEC-107): its lead's
/// branch for a lead's worker while the owner's Lands setting is Lead, else the project's
/// Land on branch (DEC-111 Q1). A lead tip that cannot be read, also for a lead whose worktree is
/// gone, is logged by its plain code and gives None, never the owner's branch, which that
/// worker does not land on. Inputs: the store, the owner repository opened for object
/// work, its root and the change set's project and agent. Output: the tip, None while it
/// cannot be read.
pub(super) fn landing_tip(
    db: &Connection,
    owner: &Repo,
    repository: &Path,
    project: &ProjectId,
    agent: &super::ChangeAgent,
) -> Option<String> {
    match super::subagent::lead_tip(db, project, agent, repository) {
        Ok(Some(tip)) => Some(tip),
        Ok(None) => land_on_tip(db, owner, repository, project),
        Err(error) => {
            eprintln!(
                "landing_lead_tip_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
            None
        }
    }
}

/// The most paths a change set may change and still be judged taken by its branch.
const TAKEN_PATHS_MAX: usize = 4096;
/// The most commits of a branch read to judge a change set taken by it.
const TAKEN_COMMITS_MAX: usize = 2048;

/// What judging one open change set against the owner's branch tip found.
enum Judged {
    Landed,
    Taken(Shown),
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
/// clean merge adds. A set whose work starts at the tip, so that merge is a fast-forward,
/// shows its whole change without a merge, the same range a stored change shows. An empty
/// change, a conflicting merge or a configured filter or merge driver leaves the whole
/// change shown, measured as [`measured_from`] says (DEC-107). Inputs: the store, the
/// owner repository, the change set and the tip. Output: the judgement.
fn judge(db: &Connection, owner: &Repo, record: &Record, tip: &str) -> Result<Judged, EngineError> {
    let whole = |from: &str| -> Result<Shown, EngineError> {
        let stats = diff::stats(owner, from, &record.head)?;
        Ok(Shown {
            range: (from.to_owned(), record.head.clone()),
            counts: (stats.files, stats.added, stats.removed),
        })
    };
    if owner.tree_of(&record.base)? == owner.tree_of(&record.head)? {
        return whole(&record.base).map(Judged::Open);
    }
    if owner.test(&["merge-base", "--is-ancestor", &record.head, tip])? {
        return Ok(Judged::Landed);
    }
    let from = measured_from(db, owner, record, tip)?;
    if from == tip {
        if owner.tree_of(tip)? == owner.tree_of(&record.head)? {
            return Ok(Judged::Landed);
        }
        return whole(&from).map(Judged::Open);
    }
    let paths = merge_paths(owner, &from, tip, &record.head)?;
    if uses_configured_commands(owner, &paths, &[&from, tip, &record.head])? {
        return whole(&from).map(Judged::Open);
    }
    let (tree, conflicts) = merge(owner, &from, tip, &record.head)?;
    if !conflicts.is_empty() {
        let shown = whole(&from)?;
        return if !record.restored && taken_by(owner, &from, tip, &record.head)? {
            Ok(Judged::Taken(shown))
        } else {
            Ok(Judged::Open(shown))
        };
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
/// the store, the owner repository, the change set, the tip and the heads found. Output:
/// the judgement.
fn judged(
    db: &Connection,
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
    let found = judge(db, owner, record, tip)?;
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
/// still there, and the project's activity says it was closed outside the app. Inputs: the
/// store and the open change set. Output: Ok once closed.
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
    flow::release_agent_claims(db, record);
    if std::fs::symlink_metadata(record.repository.join(".git")).is_ok_and(|found| found.is_dir()) {
        flow::set_engine_ref(
            &shared(&record.repository)?,
            &record.repository,
            &pending_ref(&record.id),
            None,
        )?;
    }
    super::note_change(db, record, ActivityKind::Discarded, OWNER_ACTOR);
    Ok(())
}

/// Says whether the tip's own line of the newest `TAKEN_COMMITS_MAX` commits since `from`
/// took every path the change set changes at the set's own version, so its merge conflicts
/// only because the tip moved on from work it already took, as after a port (HB-S4). A
/// change of more than `TAKEN_PATHS_MAX` paths, or a line too long to read, stays open.
/// Inputs: the repository, the measured start, the tip and the head. Output: the answer.
fn taken_by(owner: &Repo, from: &str, tip: &str, head: &str) -> Result<bool, EngineError> {
    assert!(super::git::is_object_id(from));
    assert!(super::git::is_object_id(tip));
    assert_ne!(from, tip);
    let wanted = diff::changed_moves(owner, from, head)?;
    if wanted.is_empty() || wanted.len() > TAKEN_PATHS_MAX {
        return Ok(false);
    }
    let line = diff::moves_on_line(owner, from, tip, TAKEN_COMMITS_MAX)?;
    Ok(line.is_some_and(|line| line_took(&wanted, &line)))
}

/// Says whether a line of commits took every wanted move. Reading newest first, the first
/// commit to set a wanted path to the set's version takes it and the first to set it back
/// to the version the set started from is a revert, so any path left undecided or reverted
/// keeps the set open. Inputs: the wanted moves and the line, newest first. Output: the answer.
fn line_took(wanted: &[diff::PathMove], line: &[diff::PathMove]) -> bool {
    assert!(!wanted.is_empty());
    let mut waiting: BTreeMap<&[u8], &diff::PathMove> = wanted
        .iter()
        .map(|one| (one.path.as_slice(), one))
        .collect();
    assert_eq!(waiting.len(), wanted.len());
    for step in line {
        let Some(want) = waiting.get(step.path.as_slice()) else {
            continue;
        };
        assert_ne!(want.old, want.new);
        if step.new == want.old {
            return false;
        }
        if step.new == want.new {
            waiting.remove(step.path.as_slice());
        }
    }
    waiting.is_empty()
}

/// Closes a change set the branch it lands on already took (HB-S4) as a discard the owner
/// can undo within the restore window, alone, since its workers' sets are not on that
/// branch, and says so in the project's activity, even when a later step of the close
/// failed, but not when the store refused the close itself. The caller holds [`hold`] on
/// its repository. Inputs: the store and the open change set. Output: Ok once closed.
fn close_taken(db: &Connection, record: &mut Record) -> Result<(), EngineError> {
    assert!(record.is_open() && record.intent.is_none());
    assert!(!record.restored);
    let closed = flow::discard_held(db, record, flow::now_ms()?);
    if !record.is_open() {
        assert_eq!(record.state, ChangeState::Discarded);
        super::note_taken(db, record);
    }
    closed.map(drop)
}

/// Keeps one open change set's counts on what the tip does not yet hold. Inputs: the store,
/// the change set and how it was judged. Output: the two trees its diff shows.
fn show(
    db: &Connection,
    record: &mut Record,
    shown: Shown,
) -> Result<Option<(String, String)>, EngineError> {
    assert!(record.is_open());
    if (record.files, record.added, record.removed) != shown.counts {
        (record.files, record.added, record.removed) = shown.counts;
        store::save(db, record)?;
    }
    Ok(Some(shown.range))
}

/// Notices what the owner did outside the app to one open change set. The caller holds
/// [`hold`] on its repository. A gone agent copy closes it as removed outside; against the
/// tip it lands on (DEC-107), its lead's branch for a lead's worker while Lands is Lead,
/// else the owner's branch tip, when one is readable, a pending or conflicting set the tip
/// holds closes as landed outside like an Accept onto that tip, unless a worker below it
/// still holds an open set, which no close the owner did not press may discard, and
/// otherwise its counts become what is not yet on the tip. Inputs: the store, the change
/// set, the owner repository with its tip, None while the owner's HEAD is detached, busy
/// or unreadable, what the set is judged against and the heads its batch check found.
/// Output: the two trees its diff shows against that tip, None to show its stored change.
fn observe_one(
    db: &Connection,
    id: &ChangeSetId,
    owner: Option<&(Repo, String)>,
    against: &Against,
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
    if !matches!(record.state, ChangeState::Pending | ChangeState::Conflict) {
        return Ok(None);
    }
    let opened;
    let (owner, tip) = match (against, owner) {
        (Against::Owner, Some((owner, tip))) | (Against::Lead(Some(tip)), Some((owner, _))) => {
            (owner, tip.as_str())
        }
        (Against::Lead(Some(tip)), None) => {
            opened = shared(&record.repository)?;
            (&opened, tip.as_str())
        }
        (Against::Owner, None) | (Against::Lead(None), _) => return Ok(None),
    };
    match judged(db, owner, &record, tip, present)? {
        Judged::Removed => {
            close_removed(db, &mut record)?;
            Ok(None)
        }
        Judged::Taken(shown) if super::subagent::has_open_worker(db, &record)? => {
            show(db, &mut record, shown)
        }
        Judged::Taken(_) => {
            close_taken(db, &mut record)?;
            Ok(None)
        }
        Judged::Landed if super::subagent::has_open_worker(db, &record)? => Ok(None),
        Judged::Landed
            if matches!(against, Against::Lead(_))
                && !super::merges::holds_changes_since(owner, tip, &record.base, &record.head)? =>
        {
            Ok(None)
        }
        Judged::Landed => {
            record.outside = true;
            let by = match against {
                Against::Owner => By::Owner(None),
                Against::Lead(_) => By::Named(ENGINE_ACTOR),
            };
            finish(db, &mut record, tip, by)?;
            assert!(!record.is_open());
            Ok(None)
        }
        Judged::Open(shown) => show(db, &mut record, shown),
    }
}

/// Opens the owner repository for object and ref work and reads the tip of a project's
/// Land on branch (DEC-111 Q1). Inputs: the store, the owner checkout root and the
/// project. Output: the repository and tip, None while an operation is unfinished, no
/// such branch exists or it cannot be read; an unreadable one is logged by its plain code.
fn owner_tip(db: &Connection, repository: &Path, project: &ProjectId) -> Option<(Repo, String)> {
    match shared(repository) {
        Ok(owner) => land_on_tip(db, &owner, repository, project).map(|tip| (owner, tip)),
        Err(error) => {
            eprintln!(
                "landing_observe_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
            None
        }
    }
}

/// What one open change set is judged against in an observe pass (DEC-107): the owner's
/// branch tip, or the tip of its lead's branch while Lands is Lead, None when that lead or
/// tip could not be read.
enum Against {
    Owner,
    Lead(Option<String>),
}

/// Finds what each open change set of one repository is judged against, before its hold
/// is taken, reading each lead's branch once for all of that lead's workers, so a lead's
/// own clone is fetched outside the hold and once per pass. A lead or tip that cannot be
/// read is logged by its plain code, and its workers' sets are left as they are. Inputs:
/// the store and the open change sets. Output: what each set is judged against, in order.
fn against(db: &Connection, records: &[Record]) -> Vec<Against> {
    let unread = |error: EngineError| {
        eprintln!(
            "landing_lead_tip_failed cause={}",
            crate::protocol::plain_code(&error.0)
        );
    };
    let mut leads: BTreeMap<(String, i64), Option<String>> = BTreeMap::new();
    let mut found = Vec::with_capacity(records.len());
    for record in records {
        let lead = super::subagent::lead_of(db, &record.project, &record.agent, &record.repository);
        found.push(match lead {
            Ok(None) => Against::Owner,
            Ok(Some((lead, place))) => Against::Lead(
                leads
                    .entry((record.project.as_str().to_owned(), lead))
                    .or_insert_with(|| {
                        super::subagent::read_lead_tip(db, &place)
                            .map_err(unread)
                            .ok()
                    })
                    .clone(),
            ),
            Err(error) => {
                unread(error);
                Against::Lead(None)
            }
        });
    }
    found
}

/// Notices what the owner did outside the app to every open change set of one project, or
/// of every project, one repository at a time under its hold, reading each owner tip once,
/// each lead's tip once before the hold, and checking every open head with one git run,
/// after landing lead workers' sets their lead's landed set holds (DEC-107).
/// A change set that cannot be judged now is logged by its plain code and left as it was.
/// Inputs: the store and the project, None for every project. Output: Ok once each was
/// judged or left.
pub(super) fn observe(db: &Connection, project: Option<&ProjectId>) -> Result<(), EngineError> {
    store::initialize(db)?;
    let mut by_repository: BTreeMap<(PathBuf, ProjectId), Vec<Record>> = BTreeMap::new();
    for record in store::open_of(db, project)? {
        by_repository
            .entry((record.repository.clone(), record.project.clone()))
            .or_default()
            .push(record);
    }
    for ((repository, project), records) in by_repository {
        let tips = against(db, &records);
        let ids: Vec<ChangeSetId> = records.iter().map(|record| record.id.clone()).collect();
        let heads: Vec<&str> = records.iter().map(|record| record.head.as_str()).collect();
        let Ok(_held) = flow::hold_unmoved(db, &repository) else {
            continue;
        };
        if let Err(error) = super::subagent::close_orphaned(db, &ids) {
            eprintln!(
                "landing_settle_failed cause={}",
                crate::protocol::plain_code(&error.0)
            );
        }
        let owner = owner_tip(db, &repository, &project);
        let present = match &owner {
            Some((repo, _)) => repo.commits_among(&heads).unwrap_or_else(|error| {
                eprintln!(
                    "landing_observe_failed cause={}",
                    crate::protocol::plain_code(&error.0)
                );
                BTreeSet::new()
            }),
            None => BTreeSet::new(),
        };
        for (id, against) in ids.iter().zip(&tips) {
            if let Err(error) = observe_one(db, id, owner.as_ref(), against, &present) {
                eprintln!(
                    "landing_observe_failed cause={}",
                    crate::protocol::plain_code(&error.0)
                );
            }
        }
    }
    Ok(())
}

/// Notices what the owner did outside the app to one change set before its diff is read,
/// its lead's tip read before the hold as [`observe`] reads it. Inputs: the store and the
/// change set. Output: the two trees to show against the tip it lands on, its lead's branch
/// for a lead's worker while Lands is Lead, else the owner's branch, None to show its
/// stored change, also once it closed.
pub(super) fn observe_for_diff(
    db: &Connection,
    id: &ChangeSetId,
) -> Result<Option<(String, String)>, EngineError> {
    let record = store::load(db, id)?;
    let tips = against(db, std::slice::from_ref(&record));
    let _held = flow::hold_unmoved(db, &record.repository)?;
    let owner = owner_tip(db, &record.repository, &record.project);
    observe_one(db, id, owner.as_ref(), &tips[0], &BTreeSet::new())
}
