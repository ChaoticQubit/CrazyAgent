use super::{Answering, is_worker, land_on_lead, state};
use crate::engine::EngineError;
use crate::landing::{LeadGit, MergeOutcome};
use crate::sandbox::{ProjectSandbox, SandboxNetwork};
use crate::team::bound_text;
use crate::team::tool::{FINISH_WORK_TOOL, INSPECT_WORK_TOOL};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

const SHOWN_MAX: usize = 64;
const FILES_SHOWN_MAX: usize = 400;
const COMMIT_SHOWN: usize = 12;
const CODE_SHOWN_MAX: usize = 120;
const OWNER_LANDS: &str = "Not merged: the owner's setting keeps your sub-agents' work for the owner's own Accept in Changes. Tell the master what is ready.";

/// Merges one of a lead's own finished sub-agents into the lead's task branch for the
/// lead's `merge` tool (DEC-104 K5b, Q32). In the lead's own clone the engine reads the
/// sub-agent's work through its checked fetch and runs the merge with the lead's own
/// rights (`landing::merge_sub_agent`); in a linked worktree the sub-agent's open change
/// set lands on the lead's branch as the owner's Accept does. Nothing is pushed. Takes the
/// database, what the answers need, the lead and the sub-agent's name. Returns the
/// sentence the lead reads and whether it is a refusal, or a SQLite failure.
pub(super) fn merge_worker(
    connection: &Connection,
    answering: &Answering<'_>,
    lead: i64,
    worker: &str,
) -> Result<(String, bool), EngineError> {
    let project = &answering.stored.project_id;
    if crate::team::nesting(connection, project)?.lands == crate::team::ChildLanding::Owner {
        return Ok((OWNER_LANDS.to_owned(), true));
    }
    let shown = bound_text(worker, SHOWN_MAX);
    let not_yours = format!(
        "Not merged: none of your own sub-agents is called {shown}. Call {INSPECT_WORK_TOOL}() for their names."
    );
    let found = match crate::team::events::resolve(connection, project, worker)? {
        Some(id) => Some(id),
        None => crate::team::names::resolve_finished(connection, project, worker)?,
    };
    let Some(id) = found else {
        return Ok((not_yours, true));
    };
    if !is_worker(connection, project, lead, id)? {
        return Ok((not_yours, true));
    }
    let label = crate::team::events::label(connection, id)?;
    let status: Option<String> = connection
        .query_row(
            "SELECT status FROM team_agents WHERE id=?1 AND project=?2",
            params![id, project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(state)?;
    if matches!(status.as_deref(), Some("starting" | "working")) {
        return Ok((
            format!(
                "Not merged: {label} is still working. Merge it once it calls {FINISH_WORK_TOOL}()."
            ),
            true,
        ));
    }
    let lead_place: Option<(String, String)> = connection
        .query_row(
            "SELECT worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
            params![lead, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(state)?;
    let Some((lead_worktree, lead_git_dir)) = lead_place else {
        return Ok((
            "Not merged: your worktree is no longer known to the engine.".to_owned(),
            true,
        ));
    };
    let (lead_worktree, lead_git_dir) = (Path::new(&lead_worktree), Path::new(&lead_git_dir));
    if lead_git_dir != crate::agent_git::git_dir(lead_worktree) {
        return Ok(land_linked(
            answering,
            id,
            &label,
            lead_worktree,
            lead_git_dir,
        ));
    }
    Ok(merge_in_clone(answering, lead, id, &label))
}

/// Lands a sub-agent's open change set on a lead's linked worktree branch, as one squash
/// commit through landing's Accept. Takes what the answers need, the sub-agent, its label
/// and the lead's worktree and git folder. Returns the sentence and whether it refuses.
fn land_linked(
    answering: &Answering<'_>,
    id: i64,
    label: &str,
    worktree: &Path,
    git_dir: &Path,
) -> (String, bool) {
    let project = &answering.stored.project_id;
    let opened = crate::landing::open(answering.landing).and_then(|db| {
        let sets = crate::landing::list(&db, project)?;
        let branch = crate::landing::agent_branch(&db, worktree, git_dir)?;
        Ok((db, sets, branch))
    });
    let (db, sets, branch) = match opened {
        Ok(opened) => opened,
        Err(problem) => {
            return (
                format!(
                    "Not merged: the change sets could not be read ({}).",
                    bound_text(&problem.0, CODE_SHOWN_MAX)
                ),
                true,
            );
        }
    };
    let Some(branch) = branch else {
        return (
            "Not merged: your worktree has no branch of its own to merge into.".to_owned(),
            true,
        );
    };
    match super::merge::waiting(sets, id) {
        Some(set) => land_on_lead(&db, &set.id, &set.head, &branch),
        None => (
            format!("Nothing to merge: {label} has no work waiting to land on your branch."),
            true,
        ),
    }
}

/// Merges a sub-agent into a lead's own clone with the lead's own rights: the lead's
/// sandbox and installed profile run every git step there. Takes what the answers need,
/// the lead, the sub-agent and its label. Returns the sentence and whether it refuses.
fn merge_in_clone(answering: &Answering<'_>, lead: i64, id: i64, label: &str) -> (String, bool) {
    let refused = |text: String| (text, true);
    let Some(state_folder) = answering.agents.parent() else {
        return refused("Not merged: the engine's agent folders could not be found.".to_owned());
    };
    let profile = state_folder
        .join("profiles")
        .join(format!("agent-{lead}.sb"));
    if !std::fs::symlink_metadata(&profile).is_ok_and(|found| found.is_file()) {
        return refused(
            "Not merged: your sandbox profile is missing, so the engine cannot run git with your rights. Ask the master to restart you."
                .to_owned(),
        );
    }
    let home = crate::team::runtime::agent_home(state_folder, lead);
    let tmp = answering.agents.join(format!("agent-{lead}")).join("tmp");
    let sandbox = answering
        .stored
        .project_folder()
        .canonicalize()
        .ok()
        .and_then(|root| {
            ProjectSandbox::new(&root, &home, &tmp, SandboxNetwork::Only(Vec::new())).ok()
        });
    let Some(sandbox) = sandbox else {
        return refused(
            "Not merged: your sandbox could not be opened to run git with your rights.".to_owned(),
        );
    };
    let git = LeadGit {
        sandbox: &sandbox,
        profile: &profile,
    };
    let project = &answering.stored.project_id;
    let merged = crate::landing::open(answering.landing)
        .and_then(|db| crate::landing::merge_sub_agent(&db, project, lead, id, &git));
    match merged {
        Ok(MergeOutcome::Merged { commit, branch }) => (
            format!(
                "Merged {label}'s branch {} into your task branch as {}. Nothing was pushed.",
                bound_text(&branch, crate::landing::BRANCH_BYTES_MAX),
                bound_text(&commit, COMMIT_SHOWN)
            ),
            false,
        ),
        Ok(MergeOutcome::AlreadyMerged { branch }) => (
            format!(
                "Nothing to merge: {label}'s branch {} is already in your task branch.",
                bound_text(&branch, crate::landing::BRANCH_BYTES_MAX)
            ),
            false,
        ),
        Ok(MergeOutcome::Conflict { branch, files }) => refused(format!(
            "Not merged: {label}'s branch {branch} conflicts with your task branch in {}. The merge was undone. Run git merge {branch} in your worktree, resolve and commit it, then call merge again; or ask {label} to merge your task branch first and finish its work again.",
            bound_text(&files.join(", "), FILES_SHOWN_MAX),
            branch = bound_text(&branch, crate::landing::BRANCH_BYTES_MAX)
        )),
        Err(problem) => refused(refusal(label, &problem.0)),
    }
}

/// The sentence a lead reads for a merge landing refused. Takes the sub-agent's label and
/// the plain code. Returns the sentence.
fn refusal(label: &str, code: &str) -> String {
    match code {
        crate::landing::WORKER_OPEN => {
            format!("Not merged: {}", crate::landing::WORKER_OPEN_TEXT)
        }
        "changes_lead_off_task_branch" => {
            "Not merged: check out your task branch in your worktree first; the merge goes into it.".to_owned()
        }
        "changes_lead_uncommitted" => {
            "Not merged: commit your own changes first, then merge again.".to_owned()
        }
        "changes_git_busy" => {
            "Not merged: finish the git merge already in progress in your worktree first.".to_owned()
        }
        "changes_merge_refused" => format!(
            "Not merged: git refused to start the merge of {label}'s branch in your worktree, often for an untracked file in the way. Move it, then merge again."
        ),
        "changes_merge_abort_failed" => {
            "Not merged: the merge conflicted and could not be undone. Run git merge --abort in your worktree, then merge again.".to_owned()
        }
        "changes_agent_branch_exists" => format!(
            "Not merged: a branch with {label}'s branch name in your worktree holds other work. Rename or delete it, then merge again."
        ),
        _ => format!(
            "Not merged: {label}'s work could not be merged ({}).",
            bound_text(code, CODE_SHOWN_MAX)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::refusal;

    #[test]
    fn a_lead_merge_refused_for_an_open_worker_says_why() {
        assert_eq!(
            refusal("Sub-agent 2", crate::landing::WORKER_OPEN),
            format!("Not merged: {}", crate::landing::WORKER_OPEN_TEXT)
        );
    }
}
