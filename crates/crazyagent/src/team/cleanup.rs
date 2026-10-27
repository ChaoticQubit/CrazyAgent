use super::{AGENT_COLUMNS, AgentRole, TeamAgent, agent_row};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const RECLAIMABLE_MAX: usize = 32;
const CANDIDATES_MAX: usize = 2 * RECLAIMABLE_MAX;
const DISK_ENTRIES_MAX: usize = 200_000;

/// One finished team agent whose worktree and agent folder the owner may remove: its id,
/// role, label and harness, when it stopped, and the bytes both hold on disk, with
/// `counted_all` false when the count stopped at its bound and the size is a floor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishedAgent {
    pub id: i64,
    pub role: AgentRole,
    pub label: String,
    pub harness: String,
    pub stopped_ms: Option<i64>,
    pub bytes: u64,
    pub counted_all: bool,
}

/// Where one agent's worktree lives: the owner's checkout root, the worktree and its git
/// folder, as the runtime recorded them when it added the worktree.
struct Place {
    repository: PathBuf,
    worktree: PathBuf,
    git_dir: PathBuf,
}

fn unavailable(_: impl std::fmt::Display) -> EngineError {
    EngineError("cleanup_unavailable".into())
}

fn agent_folder(team: &Path, id: i64) -> PathBuf {
    team.join("agents").join(format!("agent-{id}"))
}

fn profile(team: &Path, id: i64) -> PathBuf {
    team.join("profiles").join(format!("agent-{id}.sb"))
}

fn place(db: &Connection, project: &ProjectId, id: i64) -> Result<Option<Place>, EngineError> {
    db.query_row(
        "SELECT repository,worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2",
        params![id, project.as_str()],
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

/// Says why a team agent's own row keeps it: not this project's, still running, or with
/// an unfinished task pinned to it. Inputs: the store, the project and the agent.
/// Output: None when it ended and nothing waits for it.
fn agent_blocker(
    db: &Connection,
    project: &ProjectId,
    id: i64,
) -> Result<Option<&'static str>, EngineError> {
    let status: Option<String> = db
        .query_row(
            "SELECT status FROM team_agents WHERE id=?1 AND project=?2",
            params![id, project.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?;
    match status.as_deref() {
        None => return Ok(Some("cleanup_agent_not_found")),
        Some("stopped" | "failed") => {}
        Some(_) => return Ok(Some("cleanup_agent_running")),
    }
    let tasks: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM team_tasks WHERE agent=?1 AND project=?2
             AND status IN ('queued','needs_pool','running')",
            params![id, project.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    Ok((tasks > 0).then_some("cleanup_agent_has_task"))
}

fn landing_time(now_ms: i64) -> Result<u64, EngineError> {
    u64::try_from(now_ms).map_err(unavailable)
}

/// Lists a project's finished agents the owner may remove, newest first: ended, with no
/// unfinished task pinned to it, no open or restorable change set and a worktree that
/// holds exactly the commit its next change set would open above. It reads at most
/// `CANDIDATES_MAX` ended agents and returns at most `RECLAIMABLE_MAX`.
/// Inputs: the store, the team state folder, the project and the time now.
/// Output: the agents with their size on disk.
pub fn finished(
    db: &Connection,
    team: &Path,
    project: &ProjectId,
    now_ms: i64,
) -> Result<Vec<FinishedAgent>, EngineError> {
    let now = landing_time(now_ms)?;
    let candidates: Vec<TeamAgent> = {
        let mut statement = db
            .prepare(&format!(
                "SELECT {AGENT_COLUMNS} FROM team_agents WHERE project=?1
                 AND status IN ('stopped','failed')
                 AND id IN (SELECT agent FROM team_worktrees WHERE project=?1)
                 ORDER BY id DESC LIMIT ?2"
            ))
            .map_err(unavailable)?;
        statement
            .query_map(params![project.as_str(), CANDIDATES_MAX as i64], agent_row)
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    let mut found = Vec::new();
    let mut budget = DISK_ENTRIES_MAX;
    for agent in candidates {
        if found.len() == RECLAIMABLE_MAX {
            break;
        }
        if agent_blocker(db, project, agent.id)?.is_some() {
            continue;
        }
        let Some(place) = place(db, project, agent.id)? else {
            continue;
        };
        if crate::landing::retire_blocker(
            db,
            &place.repository,
            &place.worktree,
            &place.git_dir,
            now,
        )?
        .is_some()
        {
            continue;
        }
        let (worktree, all_worktree) = disk_bytes(&place.worktree, &mut budget);
        let (folder, all_folder) = disk_bytes(&agent_folder(team, agent.id), &mut budget);
        found.push(FinishedAgent {
            id: agent.id,
            role: agent.role,
            label: agent.label,
            harness: agent.harness,
            stopped_ms: agent.stopped_ms,
            bytes: worktree.saturating_add(folder),
            counted_all: all_worktree && all_folder,
        });
    }
    assert!(found.len() <= RECLAIMABLE_MAX);
    Ok(found)
}

/// Removes the owner's chosen finished agents, all or none as far as the checks go: every
/// agent is checked and claimed in one store transaction, so no task or revive picks it up
/// meanwhile, then every worktree is checked again, then each is removed with git, never
/// forcing, followed by its agent folder and sandbox profile. A refusal hands back every
/// claim not yet removed. The agents' rows stay as history.
/// Inputs: the store, the team state folder, the project, the agents and the time now.
/// Output: the agents removed, or the plain code of the first one kept.
pub fn remove(
    db: &Connection,
    team: &Path,
    project: &ProjectId,
    agents: &[i64],
    now_ms: i64,
) -> Result<Result<Vec<i64>, &'static str>, EngineError> {
    let unique: BTreeSet<i64> = agents.iter().copied().collect();
    if agents.is_empty()
        || agents.len() > RECLAIMABLE_MAX
        || unique.len() != agents.len()
        || agents.iter().any(|id| *id <= 0)
    {
        return Ok(Err("cleanup_request_invalid"));
    }
    let now = landing_time(now_ms)?;
    let claimed = match claim(db, project, agents)? {
        Ok(claimed) => claimed,
        Err(code) => return Ok(Err(code)),
    };
    let mut removed = Vec::new();
    let outcome = retire_claimed(db, team, &claimed, now, &mut removed);
    if !matches!(outcome, Ok(Ok(()))) {
        unclaim(db, project, &claimed[removed.len()..])?;
    }
    assert!(removed.len() <= claimed.len());
    outcome.map(|done| done.map(|()| removed))
}

/// Checks every claimed worktree again, then removes each worktree with its agent folder
/// and sandbox profile, in claim order. Inputs: the store, the team state folder, the
/// claims, the time now and the list each removed agent joins. Output: Ok once all are
/// removed, or the plain code of the first one kept.
fn retire_claimed(
    db: &Connection,
    team: &Path,
    claimed: &[(i64, Place)],
    now: u64,
    removed: &mut Vec<i64>,
) -> Result<Result<(), &'static str>, EngineError> {
    for (_, place) in claimed {
        if let Some(code) = crate::landing::retire_blocker(
            db,
            &place.repository,
            &place.worktree,
            &place.git_dir,
            now,
        )? {
            return Ok(Err(code));
        }
    }
    for (id, place) in claimed {
        if let Err(code) = crate::landing::retire_worktree(
            db,
            &place.repository,
            &place.worktree,
            &place.git_dir,
            now,
        )? {
            return Ok(Err(code));
        }
        removed.push(*id);
        remove_agent_files(team, *id)?;
    }
    Ok(Ok(()))
}

/// Checks and claims every chosen agent in one immediate transaction: its row must still
/// show it ended with no unfinished task, and its worktree record is deleted, which no
/// revive or resolve task can start without. Inputs: the store, the project and the
/// agents. Output: each agent with its worktree place, or the first plain refusal, which
/// leaves every record as it was.
fn claim(
    db: &Connection,
    project: &ProjectId,
    agents: &[i64],
) -> Result<Result<Vec<(i64, Place)>, &'static str>, EngineError> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    let mut claimed = Vec::new();
    for id in agents {
        if let Some(code) = agent_blocker(&tx, project, *id)? {
            return Ok(Err(code));
        }
        let Some(place) = place(&tx, project, *id)? else {
            return Ok(Err("cleanup_agent_not_found"));
        };
        tx.execute(
            "DELETE FROM team_worktrees WHERE agent=?1 AND project=?2",
            params![id, project.as_str()],
        )
        .map_err(unavailable)?;
        claimed.push((*id, place));
    }
    tx.commit().map_err(unavailable)?;
    assert_eq!(claimed.len(), agents.len());
    Ok(Ok(claimed))
}

/// Hands back claims whose worktree stays: their worktree records are stored again.
/// Inputs: the store, the project and the claims. Output: Ok once stored.
fn unclaim(
    db: &Connection,
    project: &ProjectId,
    claims: &[(i64, Place)],
) -> Result<(), EngineError> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    for (id, place) in claims {
        let text = |path: &Path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or_else(|| unavailable(""))
        };
        tx.execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
            params![
                id,
                project.as_str(),
                text(&place.repository)?,
                text(&place.worktree)?,
                text(&place.git_dir)?
            ],
        )
        .map_err(unavailable)?;
    }
    tx.commit().map_err(unavailable)
}

/// Deletes a removed agent's folder and sandbox profile from the team state, making its
/// read-only folders writable first, such as a module cache a toolchain left. Inputs: the
/// team state folder and the agent. Output: Ok once both are gone.
fn remove_agent_files(team: &Path, id: i64) -> Result<(), EngineError> {
    let gone = |result: std::io::Result<()>| match result {
        Err(problem) if problem.kind() != std::io::ErrorKind::NotFound => {
            Err(EngineError("cleanup_folder_failed".into()))
        }
        _ => Ok(()),
    };
    let folder = agent_folder(team, id);
    if std::fs::remove_dir_all(&folder).is_err() {
        writable(&folder);
        gone(std::fs::remove_dir_all(&folder))?;
    }
    gone(std::fs::remove_file(profile(team, id)))
}

/// Lets the owner delete inside every real folder under `folder`, never following a link,
/// visiting at most `DISK_ENTRIES_MAX` entries. Input: the folder.
#[cfg(unix)]
fn writable(folder: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut pending = vec![folder.to_path_buf()];
    let mut budget = DISK_ENTRIES_MAX;
    while let Some(next) = pending.pop() {
        let Ok(metadata) = std::fs::symlink_metadata(&next) else {
            continue;
        };
        if !metadata.is_dir() || budget == 0 {
            continue;
        }
        budget -= 1;
        let _ = std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o700));
        if let Ok(entries) = std::fs::read_dir(&next) {
            pending.extend(entries.flatten().take(budget).map(|entry| entry.path()));
        }
    }
}

#[cfg(not(unix))]
fn writable(_: &Path) {}

/// Counts what a folder holds on disk, never following a link, visiting at most `budget`
/// entries. Inputs: the folder and the entries left. Output: the bytes and whether every
/// entry was counted.
fn disk_bytes(folder: &Path, budget: &mut usize) -> (u64, bool) {
    let mut bytes = 0_u64;
    let mut pending = vec![folder.to_path_buf()];
    while let Some(next) = pending.pop() {
        if *budget == 0 {
            return (bytes, false);
        }
        *budget -= 1;
        let Ok(metadata) = std::fs::symlink_metadata(&next) else {
            continue;
        };
        bytes = bytes.saturating_add(allocated(&metadata));
        if metadata.is_dir()
            && let Ok(entries) = std::fs::read_dir(&next)
        {
            pending.extend(entries.flatten().take(*budget).map(|entry| entry.path()));
        }
    }
    (bytes, true)
}

#[cfg(unix)]
fn allocated(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated(metadata: &std::fs::Metadata) -> u64 {
    metadata.len()
}

#[cfg(all(test, unix))]
mod tests;
