use super::flow::{check_places, shared, tip};
use super::git::failure;
use super::store;
use crate::agent_git::Layout;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

/// The most sub-agents of one lead the engine reads in one call, past the team's
/// running-agent limit, so a full team is never cut short.
const SUB_AGENTS_MAX: usize = 64;
const _: () = assert!(SUB_AGENTS_MAX >= crate::terminal::PANES_PER_PROJECT_MAX);

fn unavailable(_: impl std::fmt::Display) -> EngineError {
    failure("changes_unavailable")
}

/// Where one team agent works, as the engine recorded it when it made the worktree: the
/// owner's checkout root, the worktree and its git metadata folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Place {
    pub(super) repository: PathBuf,
    pub(super) worktree: PathBuf,
    pub(super) git_dir: PathBuf,
}

/// Reads where one team agent of a project works. Inputs: the engine database, the project
/// and the agent. Output: the place, None for an agent with no worktree recorded now.
pub(super) fn place(
    db: &Connection,
    project: &ProjectId,
    agent: i64,
) -> Result<Option<Place>, EngineError> {
    db.query_row(
        "SELECT repository,worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
        params![agent, project.as_str()],
        |row| {
            Ok(Place {
                repository: PathBuf::from(row.get::<_, String>(0)?),
                worktree: PathBuf::from(row.get::<_, String>(1)?),
                git_dir: PathBuf::from(row.get::<_, String>(2)?),
            })
        },
    )
    .optional()
    .map_err(unavailable)
}

/// Reads the sub-agents one lead started that still have a worktree, oldest first, each
/// with its id and place. Inputs: the engine database, the project and the lead. Output:
/// at most `SUB_AGENTS_MAX` of them, or `changes_too_large` for more.
pub(super) fn sub_agents(
    db: &Connection,
    project: &ProjectId,
    lead: i64,
) -> Result<Vec<(i64, Place)>, EngineError> {
    let mut statement = db
        .prepare(
            "SELECT a.id,w.repository,w.worktree,w.git_dir FROM team_agents a
             JOIN team_worktrees w ON w.agent=a.id AND w.project=a.project
             WHERE a.project=?1 AND a.parent=?2 AND a.role='sub' ORDER BY a.id LIMIT ?3",
        )
        .map_err(unavailable)?;
    let limit = i64::try_from(SUB_AGENTS_MAX + 1).map_err(unavailable)?;
    let found: Vec<(i64, Place)> = statement
        .query_map(params![project.as_str(), lead, limit], |row| {
            Ok((
                row.get(0)?,
                Place {
                    repository: PathBuf::from(row.get::<_, String>(1)?),
                    worktree: PathBuf::from(row.get::<_, String>(2)?),
                    git_dir: PathBuf::from(row.get::<_, String>(3)?),
                },
            ))
        })
        .map_err(unavailable)?
        .collect::<Result<_, _>>()
        .map_err(unavailable)?;
    if found.len() > SUB_AGENTS_MAX {
        return Err(failure("changes_too_large"));
    }
    Ok(found)
}

/// Lists the branches of a lead's sub-agents whose work is not yet in the lead's task
/// branch (DEC-104 K5b). For a lead in its own clone, a sub-agent's branch counts as
/// merged once its tip, read through the engine's checked fetch, is an ancestor of the
/// lead's tip. For a lead in a linked worktree, whose workers land as squash commits, it
/// counts once the sub-agent has no open change set. No git runs in any agent's clone.
/// Inputs: the engine database, the project and the lead. Output: the unmerged branches in
/// sub-agent order, or `changes_workspace_invalid` for a lead with no worktree or branch.
pub(super) fn unmerged(
    db: &Connection,
    project: &ProjectId,
    lead: i64,
) -> Result<Vec<String>, EngineError> {
    store::initialize(db)?;
    let lead_place =
        place(db, project, lead)?.ok_or_else(|| failure("changes_workspace_invalid"))?;
    let subs = sub_agents(db, project, lead)?;
    if subs.is_empty() {
        return Ok(Vec::new());
    }
    let layout = check_places(
        &lead_place.repository,
        &lead_place.worktree,
        &lead_place.git_dir,
    )?;
    let lead_tip = match layout {
        Layout::Clone => Some(tip(
            db,
            &lead_place.repository,
            &lead_place.worktree,
            &lead_place.git_dir,
        )?),
        Layout::Linked => None,
    };
    let repository = shared(&lead_place.repository)?;
    let mut unmerged = Vec::new();
    for (_, sub) in &subs {
        if sub.repository != lead_place.repository {
            return Err(failure("changes_workspace_invalid"));
        }
        let branch = super::agent_branch(db, &sub.worktree, &sub.git_dir)?
            .ok_or_else(|| failure("changes_workspace_invalid"))?;
        let landed = match &lead_tip {
            Some(lead_tip) => {
                let sub_tip = tip(db, &sub.repository, &sub.worktree, &sub.git_dir)?;
                repository.test(&["merge-base", "--is-ancestor", &sub_tip, lead_tip])?
            }
            None => store::open_for(db, path_text(&sub.worktree)?)?.is_none(),
        };
        if !landed {
            unmerged.push(branch);
        }
    }
    assert!(unmerged.len() <= subs.len());
    Ok(unmerged)
}

/// The text of a path the engine recorded, which is always UTF-8. Input: the path.
/// Output: the text, or `changes_workspace_invalid`.
pub(super) fn path_text(path: &Path) -> Result<&str, EngineError> {
    path.to_str()
        .ok_or_else(|| failure("changes_workspace_invalid"))
}
