use super::flow::{check_places, clone_branch, open_agent};
use super::git::{Repo, failure, is_object_id};
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

/// Refuses a change set whose own commits merge in work from another branch (DEC-107
/// rule 2), which the agent git wrapper alone cannot stop, since an agent can run git by
/// its full path. A merge passes when every parent after its first is already in the
/// owner's current branch, as the engine's own update merges are, or in the commit the
/// agent's worktree starts from, or in the head of the agent's open change set, which the
/// engine recorded at an earlier turn end or made itself as the update merge it hands the
/// agent, or, for a lead, in the tip of one of its own workers, or, for the master at Full
/// access, in the head of any open change set of the project (DEC-117 Q2). The caller
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
