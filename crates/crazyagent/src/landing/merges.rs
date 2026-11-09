use super::flow::{check_places, clone_branch, open_agent};
use super::git::{OUTPUT_BYTES_MAX, Repo, failure, is_object_id};
use super::store::Record;
use super::{AgentKind, Workspace, clone, store, subagent};
use crate::agent_git::Layout;
use crate::engine::EngineError;
use crate::worktrees::TREE_BOUNDS;
use rusqlite::Connection;

/// The most merge commits one change set's own commits may hold before the engine stops
/// reading them and refuses the set as too large, well past the 130 merges one lead made
/// in the DEC-107 evidence.
const MERGES_MAX: usize = 1024;
/// The most parents one listed merge may have.
const PARENTS_MAX: usize = 16;
/// The most bytes one object id with its separator takes, for a SHA-256 repository.
const ID_FIELD_BYTES_MAX: usize = 65;
const LISTING_BYTES_MAX: usize = (MERGES_MAX + 1) * (PARENTS_MAX + 1) * ID_FIELD_BYTES_MAX;
const UNCOVERED_BYTES_MAX: usize = ID_FIELD_BYTES_MAX;
/// The most commits of its own a master set may hold and still land by itself: one fix per
/// conflicted set, well past the two sets of the HackerBlog case.
const OWN_COMMITS_MAX: usize = 32;

/// Refuses a change set whose own commits merge in work from another branch (DEC-107
/// rule 2), which the agent git wrapper alone cannot stop, since an agent can run git by
/// its full path. A merge passes when every parent after its first is already in the
/// owner's current branch, as the engine's own update merges are, or in the tip of the
/// project's Land on branch, the landed main the agent brings in with `git merge
/// crazyagent/main` (OS-8), read from the owner repository and left out when it cannot be
/// read, or in the commit the agent's worktree starts from, or in the head of the agent's open change set, which the
/// engine recorded at an earlier turn end or made itself as the update merge it hands the
/// agent, or, for a lead, in the tip of one of its own workers, or, for the master at Full
/// access, in the head of any open change set of the project (DEC-117 Q2). "Catch up with
/// main" Off is a rule of the git the app writes for an agent and is not read here: the
/// owner's branch must stay known for the update merges, so landed work merged any other
/// way passes. The caller
/// holds the repository hold, so nothing here takes it. Inputs: the store, the workspace, the owner
/// repository and the agent's head. Output: Ok, else `changes_outside_merge`,
/// `changes_too_large` or a plain git or store code.
pub(super) fn refuse_outside_merges(
    db: &Connection,
    workspace: &Workspace,
    shared: &Repo,
    head: &str,
) -> Result<(), EngineError> {
    assert!(is_object_id(head));
    let Some(owner) = shared.commit_of("HEAD")? else {
        return Ok(());
    };
    let mut known = vec![owner];
    if let Ok(Some((_, landed))) = super::owner::land_on(db, shared, &workspace.project) {
        known.push(landed);
    }
    let worktree = subagent::path_text(&workspace.worktree)?;
    if let Some(start) = store::base_of(db, worktree)?
        && shared.commit_of(&start)?.as_deref() == Some(start.as_str())
    {
        known.push(start);
    }
    if let Some(open) = store::open_for(db, worktree)?
        && shared.commit_of(&open.head)?.as_deref() == Some(open.head.as_str())
    {
        known.push(open.head);
    }
    let parents = merged_parents(shared, head, &known)?;
    if parents.is_empty() || covered(shared, &parents, &known)? {
        return Ok(());
    }
    known.extend(worker_tips(db, workspace, shared)?);
    if workspace.agent.kind == AgentKind::Master
        && crate::sandbox::policy::load(db, workspace.project.as_str())
            .map_err(|_| failure("changes_unavailable"))?
            .access
            == crate::sandbox::policy::AgentAccess::Full
    {
        for head in store::open_heads(db, &workspace.project)? {
            if shared.commit_of(&head)?.as_deref() == Some(head.as_str()) {
                known.push(head);
            }
        }
    }
    if covered(shared, &parents, &known)? {
        return Ok(());
    }
    Err(failure("changes_outside_merge"))
}

/// Tells whether a master's set holds nothing but fixes of real conflicts with the other
/// open sets its head holds, so it may land with no Accept (MASTER-LAND-SPLIT). Every own
/// commit must be such a fix, and every one of those sets must come in through one. Any
/// doubt is false and the set waits for the owner. The caller holds the repository hold
/// and has verified the objects. Inputs: the owner repository, the master's set, the sets
/// its head holds, the Land on tip and the commit its squash measures from. Output: the
/// answer.
pub(super) fn only_conflict_fixes(
    owner: &Repo,
    master: &Record,
    covered: &[Record],
    tip: &str,
    from: &str,
) -> Result<bool, EngineError> {
    assert!(!covered.is_empty());
    assert!(is_object_id(&master.head) && is_object_id(from) && is_object_id(tip));
    let Some(merges) = own_merges(owner, master, covered, tip, from)? else {
        return Ok(false);
    };
    for [commit, ours, theirs] in &merges {
        if !fixes_conflict(owner, commit, ours, theirs)? {
            return Ok(false);
        }
    }
    for set in covered {
        if !brought_in(owner, &set.head, &merges)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Lists every commit the master's squash carries, leaving out where it measures from, the
/// Land on tip and the sets its head holds, as commit, first parent and second parent.
/// Inputs: the owner repository, the master's set, the sets, the tip and the commit the
/// squash measures from. Output: the merges, or None when there are no own commits, too
/// many, or one is not a merge of exactly two parents.
fn own_merges(
    owner: &Repo,
    master: &Record,
    covered: &[Record],
    tip: &str,
    from: &str,
) -> Result<Option<Vec<[String; 3]>>, EngineError> {
    let count = format!("--max-count={}", OWN_COMMITS_MAX + 1);
    let mut arguments = vec![
        "rev-list",
        "--parents",
        &count,
        &master.head,
        "--not",
        from,
        tip,
    ];
    arguments.extend(covered.iter().map(|set| set.head.as_str()));
    let listed = owner.exec(&arguments, None, &[], LISTING_BYTES_MAX)?;
    if listed.code != 0 || listed.overflow {
        return Err(failure("changes_git_failed"));
    }
    let text = std::str::from_utf8(&listed.stdout).map_err(|_| failure("changes_git_failed"))?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() || lines.len() > OWN_COMMITS_MAX {
        return Ok(None);
    }
    let mut merges = Vec::with_capacity(lines.len());
    for line in lines {
        let ids: Vec<&str> = line.split(' ').collect();
        if !ids.iter().all(|id| is_object_id(id)) {
            return Err(failure("changes_git_failed"));
        }
        let [commit, ours, theirs] = ids[..] else {
            return Ok(None);
        };
        merges.push([commit.to_owned(), ours.to_owned(), theirs.to_owned()]);
    }
    assert!(!merges.is_empty() && merges.len() <= OWN_COMMITS_MAX);
    Ok(Some(merges))
}

/// Tells whether a merge commit is only the fix of a real conflict: git's own merge of its
/// parents really conflicts, its tree keeps every clean change of that merge, and no
/// conflicted file still holds a conflict marker. Inputs: the owner repository, the commit
/// and its two parents. Output: the answer.
fn fixes_conflict(
    owner: &Repo,
    commit: &str,
    ours: &str,
    theirs: &str,
) -> Result<bool, EngineError> {
    let Some(conflicts) = kept_clean_changes(owner, commit, ours, theirs)? else {
        return Ok(false);
    };
    if conflicts.is_empty() {
        return Ok(false);
    }
    for path in &conflicts {
        if holds_conflict_marker(owner, commit, path)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Tells whether a landed commit holds a change set's clean changes, by the test a conflict
/// fix passes: merging the set's head into it again would change nothing but conflicted
/// paths. A merge that kept none of the set's work holds its head and fails this. Inputs:
/// the owner repository, the landed commit and the set's head. Output: the answer.
pub(super) fn holds_clean_changes(
    owner: &Repo,
    landed: &str,
    head: &str,
) -> Result<bool, EngineError> {
    Ok(kept_clean_changes(owner, landed, landed, head)?.is_some())
}

/// Tells whether a commit holds a change set's clean changes as [`holds_clean_changes`]
/// does, merging from the set's own base. For a commit with the set's head in its history
/// the merge base is that head and the merge always agrees, so a merge that kept none of
/// the work shows only from the set's base. Inputs: the owner repository, the commit and
/// the set's base and head. Output: the answer.
pub(super) fn holds_changes_since(
    owner: &Repo,
    commit: &str,
    base: &str,
    head: &str,
) -> Result<bool, EngineError> {
    assert!(is_object_id(base));
    assert!(is_object_id(head));
    Ok(kept_from_base(owner, commit, base, commit, head)?.is_some())
}

/// Reads whether a file of a commit has a line a conflict marker starts. A path the commit
/// deleted has none; a path git cannot show as one file, or too large to read, counts as
/// having one, so the set waits. Inputs: the owner repository, the commit and the path as
/// git printed it. Output: the answer.
fn holds_conflict_marker(owner: &Repo, commit: &str, path: &[u8]) -> Result<bool, EngineError> {
    if path.contains(&b'\n') {
        return Ok(true);
    }
    let mut request = format!("{commit}:").into_bytes();
    request.extend_from_slice(path);
    request.push(b'\n');
    let read = owner.exec(
        &["cat-file", "--batch"],
        Some(&request),
        &[],
        OUTPUT_BYTES_MAX,
    )?;
    if read.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    if read.overflow {
        return Ok(true);
    }
    let end = read
        .stdout
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| failure("changes_git_failed"))?;
    let (header, rest) = read.stdout.split_at(end);
    if header.ends_with(b" missing") {
        return Ok(false);
    }
    if header.split(|byte| *byte == b' ').nth(1) != Some(b"blob".as_slice()) {
        return Ok(true);
    }
    Ok(rest[1..].split(|byte| *byte == b'\n').any(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        line.starts_with(b"<<<<<<< ") || line.starts_with(b">>>>>>> ") || line == b"======="
    }))
}

/// Merges two commits again and tells whether `commit`'s tree differs from the result only
/// at the paths that merge conflicts on, so it kept every clean change of both sides. The
/// sides need one merge base and no configured command. Paths are compared as git printed
/// them. Inputs: the owner repository, the commit and both sides. Output: the conflicted
/// paths, empty for a clean merge, or None when the commit did not keep that merge.
fn kept_clean_changes(
    owner: &Repo,
    commit: &str,
    ours: &str,
    theirs: &str,
) -> Result<Option<Vec<Vec<u8>>>, EngineError> {
    let bases = owner.exec(
        &["merge-base", "--all", ours, theirs],
        None,
        &[],
        ID_FIELD_BYTES_MAX * 3,
    )?;
    if bases.code == 1 || bases.overflow {
        return Ok(None);
    }
    if bases.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    let text = std::str::from_utf8(&bases.stdout).map_err(|_| failure("changes_git_failed"))?;
    let [base] = text.lines().collect::<Vec<&str>>()[..] else {
        return Ok(None);
    };
    if !is_object_id(base) {
        return Err(failure("changes_git_failed"));
    }
    kept_from_base(owner, commit, base, ours, theirs)
}

/// As [`kept_clean_changes`], from a merge base the caller names. Inputs: the owner
/// repository, the commit, the base and both sides. Output: the conflicted paths, empty for
/// a clean merge, or None when the commit did not keep that merge or a configured command
/// would run.
fn kept_from_base(
    owner: &Repo,
    commit: &str,
    base: &str,
    ours: &str,
    theirs: &str,
) -> Result<Option<Vec<Vec<u8>>>, EngineError> {
    let paths = super::owner::merge_paths(owner, base, ours, theirs)?;
    if super::owner::uses_configured_commands(owner, &paths, &[base, ours, theirs])? {
        return Ok(None);
    }
    let (merged, conflicts) = super::owner::merge_raw(owner, base, ours, theirs)?;
    let tree = owner.tree_of(commit)?;
    let changed = owner.exec(
        &[
            "diff-tree",
            "-r",
            "-z",
            "--name-only",
            "--no-renames",
            &merged,
            &tree,
        ],
        None,
        &[],
        OUTPUT_BYTES_MAX,
    )?;
    if changed.code != 0 || changed.overflow {
        return Err(failure("changes_git_failed"));
    }
    let kept = changed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .all(|path| conflicts.iter().any(|conflict| conflict == path));
    Ok(kept.then_some(conflicts))
}

/// Tells whether one of the merges brought a set's head in through its second parent, so
/// a set the master fast-forwarded onto does not count. Inputs: the owner repository, the
/// set's head and the merges. Output: the answer.
fn brought_in(owner: &Repo, head: &str, merges: &[[String; 3]]) -> Result<bool, EngineError> {
    for [_, ours, theirs] in merges {
        if owner.test(&["merge-base", "--is-ancestor", head, theirs])?
            && !owner.test(&["merge-base", "--is-ancestor", head, ours])?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Lists the parents after the first of every merge commit the head holds above the known
/// commits, once each. Inputs: the owner repository, the head and the known commits.
/// Output: the parents, or `changes_too_large` past `MERGES_MAX` merges.
fn merged_parents(shared: &Repo, head: &str, known: &[String]) -> Result<Vec<String>, EngineError> {
    assert!(!known.is_empty() && known.iter().all(|id| is_object_id(id)));
    let count = format!("--max-count={}", MERGES_MAX + 1);
    let mut arguments = vec!["rev-list", "--merges", "--parents", &count, head, "--not"];
    arguments.extend(known.iter().map(String::as_str));
    let listed = shared.exec(&arguments, None, &[], LISTING_BYTES_MAX)?;
    if listed.code != 0 {
        return Err(failure("changes_git_failed"));
    }
    if listed.overflow {
        return Err(failure("changes_too_large"));
    }
    let text = std::str::from_utf8(&listed.stdout).map_err(|_| failure("changes_git_failed"))?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > MERGES_MAX {
        return Err(failure("changes_too_large"));
    }
    let mut parents: Vec<String> = Vec::new();
    for line in lines {
        let ids: Vec<&str> = line.split(' ').collect();
        if ids.len() < 3 || ids.len() > PARENTS_MAX + 1 || !ids.iter().all(|id| is_object_id(id)) {
            return Err(failure("changes_git_failed"));
        }
        for parent in &ids[2..] {
            if !parents.iter().any(|seen| seen == parent) {
                parents.push((*parent).to_owned());
            }
        }
    }
    assert!(parents.len() <= MERGES_MAX * PARENTS_MAX);
    Ok(parents)
}

/// Tells whether every commit is an ancestor of, or one of, the known commits, with one
/// git run that reads the commits from its input. Inputs: the owner repository, the
/// commits and the known commits. Output: the answer.
fn covered(shared: &Repo, commits: &[String], known: &[String]) -> Result<bool, EngineError> {
    assert!(!commits.is_empty() && !known.is_empty());
    let mut input = String::new();
    for commit in commits {
        input.push_str(commit);
        input.push('\n');
    }
    for commit in known {
        input.push('^');
        input.push_str(commit);
        input.push('\n');
    }
    let listed = shared.exec(
        &["rev-list", "--max-count=1", "--stdin"],
        Some(input.as_bytes()),
        &[],
        UNCOVERED_BYTES_MAX,
    )?;
    if listed.code != 0 || listed.overflow {
        return Err(failure("changes_git_failed"));
    }
    Ok(listed.stdout.is_empty())
}

/// Reads the tips of a lead's own workers in the owner repository: each sub-agent it
/// started, read as the engine reads any agent's tip, and, for a lead that runs native
/// teammates in its own clone, each teammate branch there. An agent with no workers has
/// none. Inputs: the store, the workspace and the owner repository. Output: the tips.
fn worker_tips(
    db: &Connection,
    workspace: &Workspace,
    shared: &Repo,
) -> Result<Vec<String>, EngineError> {
    if workspace.agent.kind != AgentKind::Sub {
        return Ok(Vec::new());
    }
    let Some(lead) = workspace.agent.id.parse::<i64>().ok().filter(|id| *id > 0) else {
        return Ok(Vec::new());
    };
    let mut tips = Vec::new();
    for (_, sub) in subagent::sub_agents(db, &workspace.project, lead)? {
        if sub.repository != workspace.repository {
            continue;
        }
        tips.push(
            match check_places(&sub.repository, &sub.worktree, &sub.git_dir)? {
                Layout::Linked => open_agent(
                    db,
                    AgentKind::Sub,
                    &sub.repository,
                    &sub.worktree,
                    &sub.git_dir,
                )?
                .head()?,
                Layout::Clone => clone::fetch(
                    shared,
                    &sub.repository,
                    &sub.worktree,
                    &clone_branch(db, &sub.worktree)?,
                    &TREE_BOUNDS,
                )?,
            },
        );
    }
    let native = crate::team::lead_mode(db, &workspace.project, lead)?
        == crate::coord::settings::SubagentMode::Native;
    if native
        && check_places(
            &workspace.repository,
            &workspace.worktree,
            &workspace.git_dir,
        )? == Layout::Clone
    {
        let teammates = clone::fetch_teammates(
            shared,
            &workspace.repository,
            &workspace.worktree,
            &clone_branch(db, &workspace.worktree)?,
            &TREE_BOUNDS,
        )?;
        tips.extend(teammates.into_iter().map(|(_, tip)| tip));
    }
    assert!(tips.iter().all(|tip| is_object_id(tip)));
    Ok(tips)
}
