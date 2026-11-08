use super::{AGENT_COLUMNS, AgentRole, TeamAgent, agent_row};
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

pub const RECLAIMABLE_MAX: usize = 32;
const CANDIDATES_MAX: usize = 2 * RECLAIMABLE_MAX;
const DISK_ENTRIES_MAX: usize = 200_000;
const LOSSES_MAX: usize = 256;

static RUNNING: Mutex<()> = Mutex::new(());

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

/// What one removal did: the agents whose worktree, folder and profile are gone, in the
/// order asked, and the plain code of the agent it kept when it stopped before the end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Removal {
    pub agents: Vec<i64>,
    pub kept: Option<String>,
}

impl Removal {
    fn refused(code: &str) -> Self {
        Self {
            agents: Vec::new(),
            kept: Some(code.into()),
        }
    }
}

/// One agent whose work a Delete everything would lose: its id and label, and the plain
/// code of what it holds, such as `cleanup_worktree_dirty` for uncommitted edits or
/// `cleanup_worktree_unreadable` when its edits could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentLoss {
    pub id: i64,
    pub label: String,
    pub code: String,
}

/// What removing a project would do, read before anything stops: its stored name, the
/// folders Delete everything would move to the Trash, whether it may (every folder is a
/// real folder strictly inside the projects home), why not when a check refuses it, its
/// running agents' labels, its open change sets and each agent whose work would be lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemovalPreview {
    pub name: String,
    pub folders: Vec<String>,
    pub can_delete: bool,
    pub blocked: Option<String>,
    pub running: Vec<String>,
    pub open_change_sets: u32,
    pub losses: Vec<AgentLoss>,
}

/// Reads what each agent of a project with a worktree would lose if its worktree went:
/// uncommitted edits, commits or refs not landed, or an open change set, judged as a
/// removal judges it, so git runs only with the engine's hardened settings in a linked
/// worktree and never in an agent's own clone. Inputs: the store, the project and the
/// time now. Output: the agents with a loss, by id.
pub fn removal_loss(
    db: &Connection,
    project: &ProjectId,
    now_ms: i64,
) -> Result<Vec<AgentLoss>, EngineError> {
    let _running = running();
    let now = landing_time(now_ms)?;
    let agents: Vec<TeamAgent> = {
        let mut statement = db
            .prepare(&format!(
                "SELECT {AGENT_COLUMNS} FROM team_agents WHERE project=?1
                 AND id IN (SELECT agent FROM team_worktrees WHERE project=?1)
                 ORDER BY id LIMIT ?2"
            ))
            .map_err(unavailable)?;
        statement
            .query_map(params![project.as_str(), LOSSES_MAX as i64], agent_row)
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    let mut losses = Vec::new();
    for agent in agents {
        let Some(place) = place(db, Record::Live, project, agent.id)? else {
            continue;
        };
        if missing(&place.worktree) {
            continue;
        }
        let code = match crate::landing::retire_blocker(
            db,
            &place.repository,
            &place.worktree,
            &place.git_dir,
            now,
        ) {
            Ok(None) => continue,
            Ok(Some(code)) => code,
            Err(_) => "cleanup_worktree_unreadable",
        };
        losses.push(AgentLoss {
            id: agent.id,
            label: agent.label,
            code: code.into(),
        });
    }
    assert!(losses.len() <= LOSSES_MAX);
    Ok(losses)
}

/// Reads what removing a project would do, before anything stops: its stored name and
/// folders, whether Delete everything may move them (every folder a real folder strictly
/// inside the projects home and nothing holding the delete back) and why not, its running
/// agents, its open change sets and each agent's loss. Inputs: the store, the project and the time now. Output: the preview, or
/// `invalid_project` for a project the engine does not know.
pub fn removal_preview(
    db: &mut Connection,
    project: &ProjectId,
    now_ms: i64,
) -> Result<RemovalPreview, EngineError> {
    let checked = crate::storage::delete_check(db, project)?
        .ok_or_else(|| EngineError("invalid_project".into()))?;
    let (can_delete, shown, blocked) = match checked.planned {
        Ok((_, moved)) if !moved.is_empty() => (true, moved, None),
        Ok(_) => (false, checked.folders, None),
        Err(refusal) => (false, checked.folders, Some(refusal.reason)),
    };
    let running: Vec<String> = {
        let mut statement = db
            .prepare(&format!(
                "SELECT {AGENT_COLUMNS} FROM team_agents WHERE project=?1
                 AND status NOT IN ('stopped','failed') ORDER BY id LIMIT ?2"
            ))
            .map_err(unavailable)?;
        statement
            .query_map(params![project.as_str(), LOSSES_MAX as i64], agent_row)
            .map_err(unavailable)?
            .map(|agent| agent.map(|agent| agent.label))
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    let open: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM landing_change_sets WHERE project=?1
             AND state IN ('pending','conflict','updating')",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    Ok(RemovalPreview {
        name: checked.name,
        folders: shown
            .iter()
            .map(|folder| folder.to_string_lossy().into_owned())
            .collect(),
        can_delete,
        blocked,
        running,
        open_change_sets: u32::try_from(open).unwrap_or(u32::MAX),
        losses: removal_loss(db, project, now_ms)?,
    })
}

/// Says what Delete everything would lose, one plain line each, in the words the desktop
/// dialog uses. Input: the preview. Output: the running agents, then the open change sets,
/// then each agent's unlanded work.
pub fn loss_lines(preview: &RemovalPreview) -> Vec<String> {
    let mut lines: Vec<String> = preview
        .running
        .iter()
        .map(|label| format!("{label} is running and will be stopped"))
        .collect();
    match preview.open_change_sets {
        0 => {}
        1 => lines.push("1 change set not accepted or discarded".into()),
        count => lines.push(format!("{count} change sets not accepted or discarded")),
    }
    lines.extend(preview.losses.iter().map(|loss| {
        let what = match loss.code.as_str() {
            "cleanup_worktree_dirty" => "has uncommitted edits",
            "cleanup_worktree_unreadable" => "may have uncommitted edits",
            "cleanup_commits_unlanded" | "cleanup_refs_unlanded" => "has commits that never landed",
            "cleanup_changes_open" => "has a change set not accepted",
            _ => "has work that never landed",
        };
        format!("{} {what}", loss.label)
    }));
    lines
}

/// Where one agent's worktree lives: the owner's checkout root, the worktree and its git
/// folder, as the runtime recorded them when it added the worktree.
struct Place {
    repository: PathBuf,
    worktree: PathBuf,
    git_dir: PathBuf,
}

/// One agent a removal may take: where its worktree lives, whether the owner had already
/// confirmed it before, so its record waits in `team_retiring`, and whether git already
/// removed its worktree record there and left files the engine could not delete.
struct Target {
    id: i64,
    place: Place,
    retiring: bool,
    leftover: bool,
}

impl Target {
    /// Says whether git already removed this confirmed agent's worktree record, so only
    /// files git left, if any, its folder and its profile are left to delete (RVD-C-N1).
    fn worktree_removed(&self) -> bool {
        self.retiring
            && (self.leftover || (missing(&self.place.worktree) && missing(&self.place.git_dir)))
    }
}

#[derive(Clone, Copy)]
enum Record {
    Live,
    Retiring,
}

fn unavailable(_: impl std::fmt::Display) -> EngineError {
    EngineError("cleanup_unavailable".into())
}

fn running() -> MutexGuard<'static, ()> {
    RUNNING.lock().unwrap_or_else(PoisonError::into_inner)
}

fn missing(path: &Path) -> bool {
    matches!(std::fs::symlink_metadata(path), Err(problem) if problem.kind() == std::io::ErrorKind::NotFound)
}

fn agent_folder(team: &Path, id: i64) -> PathBuf {
    team.join("agents").join(format!("agent-{id}"))
}

fn profile(team: &Path, id: i64) -> PathBuf {
    team.join("profiles").join(format!("agent-{id}.sb"))
}

/// Creates the table that holds the worktree records of agents the owner confirmed for
/// removal until their files are gone; the runtime reads only `team_worktrees`, so no
/// revive or resolve task starts on a record waiting here, while the IDE's git ceiling
/// counts both. Input: the store.
pub(crate) fn initialize(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS team_retiring (
            agent INTEGER PRIMARY KEY,
            project TEXT NOT NULL,
            repository TEXT NOT NULL,
            worktree TEXT NOT NULL,
            git_dir TEXT NOT NULL,
            since_ms INTEGER NOT NULL,
            leftover INTEGER NOT NULL DEFAULT 0
        );",
    )
    .map_err(unavailable)?;
    let leftover: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('team_retiring') WHERE name='leftover')",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !leftover {
        db.execute_batch(
            "ALTER TABLE team_retiring ADD COLUMN leftover INTEGER NOT NULL DEFAULT 0;",
        )
        .map_err(unavailable)?;
    }
    Ok(())
}

/// Says whether a confirmed agent's waiting record is marked as one whose worktree record
/// git already removed, leaving files the engine could not delete. Inputs: the store, the
/// project and the agent. Output: the answer, false when no record waits.
fn left_behind(db: &Connection, project: &ProjectId, id: i64) -> Result<bool, EngineError> {
    db.query_row(
        "SELECT leftover FROM team_retiring WHERE agent=?1 AND project=?2",
        params![id, project.as_str()],
        |row| row.get(0),
    )
    .optional()
    .map(|found| found.unwrap_or(false))
    .map_err(unavailable)
}

fn place(
    db: &Connection,
    record: Record,
    project: &ProjectId,
    id: i64,
) -> Result<Option<Place>, EngineError> {
    let sql = match record {
        Record::Live => {
            "SELECT repository,worktree,git_dir FROM team_worktrees WHERE agent=?1 AND project=?2"
        }
        Record::Retiring => {
            "SELECT repository,worktree,git_dir FROM team_retiring WHERE agent=?1 AND project=?2"
        }
    };
    db.query_row(sql, params![id, project.as_str()], |row| {
        Ok(Place {
            repository: PathBuf::from(row.get::<_, String>(0)?),
            worktree: PathBuf::from(row.get::<_, String>(1)?),
            git_dir: PathBuf::from(row.get::<_, String>(2)?),
        })
    })
    .optional()
    .map_err(unavailable)
}

/// Says why a team agent's own row keeps it: not this project's, still running, the master
/// the runtime starts again in this worktree (picked by `runtime::last_master`, so aliases
/// of its harness count), or with an unfinished task pinned to it. Inputs: the store, the
/// project and the agent. Output: None when it ended and nothing waits for it.
fn agent_blocker(
    db: &Connection,
    project: &ProjectId,
    id: i64,
) -> Result<Option<&'static str>, EngineError> {
    let row: Option<(String, String, String)> = db
        .query_row(
            "SELECT status,role,harness FROM team_agents WHERE id=?1 AND project=?2",
            params![id, project.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(unavailable)?;
    let Some((status, role, harness)) = row else {
        return Ok(Some("cleanup_agent_not_found"));
    };
    if !matches!(status.as_str(), "stopped" | "failed") {
        return Ok(Some("cleanup_agent_running"));
    }
    if role == "master" {
        let revived = super::runtime::last_master(db, project, &harness).map_err(unavailable)?;
        if revived == Some(id) {
            return Ok(Some("cleanup_agent_running"));
        }
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

/// Checks one agent as a removal would take it: its row, then its worktree record,
/// waiting or live, then, unless git already removed a confirmed worktree, the worktree
/// itself. Inputs: the store, the project, the agent and the time now. Output: the
/// target, or the plain code that keeps it.
fn check(
    db: &Connection,
    project: &ProjectId,
    id: i64,
    now: u64,
) -> Result<Result<Target, &'static str>, EngineError> {
    if let Some(code) = agent_blocker(db, project, id)? {
        return Ok(Err(code));
    }
    let target = match place(db, Record::Retiring, project, id)? {
        Some(place) => Target {
            id,
            place,
            retiring: true,
            leftover: left_behind(db, project, id)?,
        },
        None => match place(db, Record::Live, project, id)? {
            Some(place) => Target {
                id,
                place,
                retiring: false,
                leftover: false,
            },
            None => return Ok(Err("cleanup_agent_not_found")),
        },
    };
    if target.worktree_removed() {
        return Ok(Ok(target));
    }
    let place = &target.place;
    match crate::landing::retire_blocker(
        db,
        &place.repository,
        &place.worktree,
        &place.git_dir,
        now,
    )? {
        Some(code) => Ok(Err(code)),
        None => Ok(Ok(target)),
    }
}

/// Hands back the waiting records of confirmed agents whose worktree is still there but no
/// longer qualifies, such as one an engine stop interrupted and the owner edited since, so
/// they are ordinary ended agents again. Runs only while no removal runs.
/// Inputs: the store, the project and the time now.
fn settle(db: &Connection, project: &ProjectId, now: u64) -> Result<(), EngineError> {
    let waiting: Vec<i64> = {
        let mut statement = db
            .prepare("SELECT agent FROM team_retiring WHERE project=?1 ORDER BY agent LIMIT ?2")
            .map_err(unavailable)?;
        statement
            .query_map(params![project.as_str(), CANDIDATES_MAX as i64], |row| {
                row.get(0)
            })
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    let mut back = Vec::new();
    for id in waiting {
        let Some(place) = place(db, Record::Retiring, project, id)? else {
            continue;
        };
        if missing(&place.worktree) && missing(&place.git_dir) {
            continue;
        }
        if check(db, project, id, now)?.is_err() {
            back.push(id);
        }
    }
    unclaim(db, project, &back)
}

/// Lists a project's finished agents the owner may remove, newest first: ended, not the
/// master the runtime starts again, with no unfinished task pinned to it, no open or
/// restorable change set and a worktree that holds exactly the commit its next change set
/// would open above; or confirmed before and stopped part way, such as by a folder that
/// could not be deleted. It reads at most `CANDIDATES_MAX` ended agents and returns at
/// most `RECLAIMABLE_MAX`. Inputs: the store, the team state folder, the project and the
/// time now. Output: the agents with their size on disk.
pub fn finished(
    db: &Connection,
    team: &Path,
    project: &ProjectId,
    now_ms: i64,
) -> Result<Vec<FinishedAgent>, EngineError> {
    let _running = running();
    initialize(db)?;
    let now = landing_time(now_ms)?;
    settle(db, project, now)?;
    let candidates: Vec<TeamAgent> = {
        let mut statement = db
            .prepare(&format!(
                "SELECT {AGENT_COLUMNS} FROM team_agents WHERE project=?1
                 AND status IN ('stopped','failed')
                 AND (id IN (SELECT agent FROM team_worktrees WHERE project=?1)
                      OR id IN (SELECT agent FROM team_retiring WHERE project=?1))
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
        let Ok(target) = check(db, project, agent.id, now)? else {
            continue;
        };
        let (worktree, all_worktree) = if missing(&target.place.worktree) {
            (0, true)
        } else {
            disk_bytes(&target.place.worktree, &mut budget)
        };
        let (folder, all_folder) = disk_bytes(&agent_folder(team, agent.id), &mut budget);
        let label = if agent.role == AgentRole::Sub
            && super::events::name_reaches_running(db, project, agent.id)?
        {
            AgentRole::Sub.agent_label(agent.id)
        } else {
            agent.label
        };
        found.push(FinishedAgent {
            id: agent.id,
            role: agent.role,
            label,
            harness: agent.harness,
            stopped_ms: agent.stopped_ms,
            bytes: worktree.saturating_add(folder),
            counted_all: all_worktree && all_folder,
        });
    }
    assert!(found.len() <= RECLAIMABLE_MAX);
    Ok(found)
}

/// Removes the owner's chosen finished agents in the order asked: every agent is checked
/// and claimed in one store transaction, which moves its worktree record to
/// `team_retiring` so no task or revive picks it up meanwhile, then every worktree is
/// checked again, then each is removed with git, never forcing, followed by its agent
/// folder and sandbox profile, and only then its record. A stop hands back the record of
/// every claimed agent whose worktree is still there. The agents' rows stay as history.
/// Inputs: the store, the team state folder, the project, the agents and the time now.
/// Output: the agents removed and the plain code of the first one kept.
pub fn remove(
    db: &Connection,
    team: &Path,
    project: &ProjectId,
    agents: &[i64],
    now_ms: i64,
) -> Result<Removal, EngineError> {
    let unique: BTreeSet<i64> = agents.iter().copied().collect();
    if agents.is_empty()
        || agents.len() > RECLAIMABLE_MAX
        || unique.len() != agents.len()
        || agents.iter().any(|id| *id <= 0)
    {
        return Ok(Removal::refused("cleanup_request_invalid"));
    }
    let _running = running();
    initialize(db)?;
    let now = landing_time(now_ms)?;
    let claimed = match claim(db, project, agents, now_ms)? {
        Ok(claimed) => claimed,
        Err(code) => return Ok(Removal::refused(code)),
    };
    let mut removed = Vec::new();
    let kept = match retire_claimed(db, team, project, &claimed, now, &mut removed) {
        Ok(None) => None,
        Ok(Some(code)) => Some(code.to_owned()),
        Err(problem) => Some(problem.0),
    };
    if kept.is_some() {
        let back: Vec<i64> = claimed[removed.len()..]
            .iter()
            .filter(|target| !(missing(&target.place.worktree) && missing(&target.place.git_dir)))
            .map(|target| target.id)
            .collect();
        unclaim(db, project, &back)?;
    }
    assert!(removed.len() <= claimed.len());
    assert_eq!(kept.is_none(), removed.len() == claimed.len());
    Ok(Removal {
        agents: removed,
        kept,
    })
}

/// Checks every claimed worktree again, then per agent checks its row once more, removes
/// its worktree, deleting what git left when it removed its record only part way, then its
/// agent folder and sandbox profile and last its waiting record, in claim order. A
/// worktree whose leftover cannot be deleted marks its waiting record, so a later removal
/// deletes the leftover without asking git again (RVD-C-N1). Inputs:
/// the store, the team state folder, the project, the claims, the time now and the list
/// each removed agent joins. Output: None once all are removed, or the plain code of the
/// first one kept.
fn retire_claimed(
    db: &Connection,
    team: &Path,
    project: &ProjectId,
    claimed: &[Target],
    now: u64,
    removed: &mut Vec<i64>,
) -> Result<Option<&'static str>, EngineError> {
    for target in claimed.iter().filter(|target| !target.worktree_removed()) {
        let place = &target.place;
        if let Some(code) = crate::landing::retire_blocker(
            db,
            &place.repository,
            &place.worktree,
            &place.git_dir,
            now,
        )? {
            return Ok(Some(code));
        }
    }
    for target in claimed {
        let place = &target.place;
        if target.worktree_removed() {
            if !missing(&place.worktree) && remove_tree(&place.worktree).is_err() {
                return Ok(Some("cleanup_worktree_leftover"));
            }
        } else {
            if let Some(code) = agent_blocker(db, project, target.id)? {
                return Ok(Some(code));
            }
            match crate::landing::retire_worktree(
                db,
                &place.repository,
                &place.worktree,
                &place.git_dir,
                now,
            )? {
                Ok(()) => {}
                Err("cleanup_worktree_leftover") => {
                    let marked = db
                        .execute(
                            "UPDATE team_retiring SET leftover=1 WHERE agent=?1 AND project=?2",
                            params![target.id, project.as_str()],
                        )
                        .map_err(unavailable)?;
                    assert_eq!(marked, 1);
                    if remove_tree(&place.worktree).is_err() {
                        return Ok(Some("cleanup_worktree_leftover"));
                    }
                }
                Err(code) => return Ok(Some(code)),
            }
        }
        if remove_agent_files(team, target.id).is_err() {
            return Ok(Some("cleanup_folder_failed"));
        }
        db.execute(
            "DELETE FROM team_agent_conversations WHERE agent=?1 AND project=?2",
            params![target.id, project.as_str()],
        )
        .map_err(unavailable)?;
        db.execute(
            "DELETE FROM team_retiring WHERE agent=?1 AND project=?2",
            params![target.id, project.as_str()],
        )
        .map_err(unavailable)?;
        removed.push(target.id);
    }
    Ok(None)
}

/// Checks and claims every chosen agent in one immediate transaction: its row must still
/// show it ended with no unfinished task, and its worktree record moves to
/// `team_retiring`, where no revive or resolve task finds it; a record already waiting
/// there stays. Inputs: the store, the project, the agents and the time now. Output: each
/// agent with its worktree place, or the first plain refusal, which leaves every record
/// as it was.
fn claim(
    db: &Connection,
    project: &ProjectId,
    agents: &[i64],
    now_ms: i64,
) -> Result<Result<Vec<Target>, &'static str>, EngineError> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    let mut claimed = Vec::new();
    for id in agents {
        if let Some(code) = agent_blocker(&tx, project, *id)? {
            return Ok(Err(code));
        }
        if let Some(place) = place(&tx, Record::Retiring, project, *id)? {
            claimed.push(Target {
                id: *id,
                place,
                retiring: true,
                leftover: left_behind(&tx, project, *id)?,
            });
            continue;
        }
        let Some(place) = place(&tx, Record::Live, project, *id)? else {
            return Ok(Err("cleanup_agent_not_found"));
        };
        tx.execute(
            "INSERT INTO team_retiring(agent,project,repository,worktree,git_dir,since_ms)
             SELECT agent,project,repository,worktree,git_dir,?3 FROM team_worktrees
             WHERE agent=?1 AND project=?2",
            params![id, project.as_str(), now_ms],
        )
        .map_err(unavailable)?;
        tx.execute(
            "DELETE FROM team_worktrees WHERE agent=?1 AND project=?2",
            params![id, project.as_str()],
        )
        .map_err(unavailable)?;
        claimed.push(Target {
            id: *id,
            place,
            retiring: false,
            leftover: false,
        });
    }
    tx.commit().map_err(unavailable)?;
    assert_eq!(claimed.len(), agents.len());
    Ok(Ok(claimed))
}

/// Hands back claims whose worktree stays: their records move from `team_retiring` back
/// to `team_worktrees`, except a record marked as left behind by git, which stays waiting
/// for the removal that deletes its leftover. Inputs: the store, the project and the
/// agents. Output: Ok once stored.
fn unclaim(db: &Connection, project: &ProjectId, agents: &[i64]) -> Result<(), EngineError> {
    if agents.is_empty() {
        return Ok(());
    }
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate).map_err(unavailable)?;
    for id in agents {
        tx.execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir)
             SELECT agent,project,repository,worktree,git_dir FROM team_retiring
             WHERE agent=?1 AND project=?2 AND leftover=0",
            params![id, project.as_str()],
        )
        .map_err(unavailable)?;
        tx.execute(
            "DELETE FROM team_retiring WHERE agent=?1 AND project=?2 AND leftover=0",
            params![id, project.as_str()],
        )
        .map_err(unavailable)?;
    }
    tx.commit().map_err(unavailable)
}

/// Deletes a removed agent's folder and sandbox profile from the team state, with a native
/// lead's teammates' profile, teammates' setup profile and the records of its teammates'
/// socket name and server pid (rv-audit TTDL F2, OP-9b, PLC c PC-L1), making its read-only
/// folders writable first, such as a module cache a toolchain left. Inputs: the team state
/// folder and the agent. Output: Ok once all are gone.
fn remove_agent_files(team: &Path, id: i64) -> Result<(), EngineError> {
    remove_tree(&agent_folder(team, id))?;
    for file in [
        profile(team, id),
        super::runtime::agent_teammates_profile(team, id),
        super::runtime::agent_teammates_setup_profile(team, id),
        super::runtime::agent_teammates_socket_name(team, id),
        super::runtime::agent_teammates_server_pid(team, id),
    ] {
        gone(std::fs::remove_file(file))?;
    }
    Ok(())
}

fn gone(result: std::io::Result<()>) -> Result<(), EngineError> {
    match result {
        Err(problem) if problem.kind() != std::io::ErrorKind::NotFound => {
            Err(EngineError("cleanup_folder_failed".into()))
        }
        _ => Ok(()),
    }
}

/// Deletes a folder and everything in it without following a link, making its read-only
/// folders writable first when a plain delete stops on one. Input: the folder. Output: Ok
/// once nothing has its name.
fn remove_tree(folder: &Path) -> Result<(), EngineError> {
    if std::fs::remove_dir_all(folder).is_err() {
        writable(folder);
        gone(std::fs::remove_dir_all(folder))?;
    }
    Ok(())
}

/// Lets the owner delete inside every real folder under `folder`, visiting at most
/// `DISK_ENTRIES_MAX` folders. Every folder is opened relative to its parent's open
/// handle without following a link, and changed and listed through its own handle, so a
/// link swapped in meanwhile is never followed. Input: the folder.
#[cfg(unix)]
fn writable(folder: &Path) {
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
    use std::ffi::CString;
    use std::rc::Rc;
    let flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let Ok(top) = rustix::fs::open(folder, flags, Mode::empty()) else {
        return;
    };
    let mut pending: Vec<(Rc<std::os::fd::OwnedFd>, Option<CString>)> = vec![(Rc::new(top), None)];
    let mut budget = DISK_ENTRIES_MAX;
    while let Some((parent, name)) = pending.pop() {
        if budget == 0 {
            return;
        }
        budget -= 1;
        let opened = match &name {
            None => Ok(Rc::clone(&parent)),
            Some(name) => rustix::fs::openat(&*parent, name.as_c_str(), flags, Mode::empty())
                .or_else(|_| {
                    rustix::fs::chmodat(
                        &*parent,
                        name.as_c_str(),
                        Mode::RWXU,
                        AtFlags::SYMLINK_NOFOLLOW,
                    )?;
                    rustix::fs::openat(&*parent, name.as_c_str(), flags, Mode::empty())
                })
                .map(Rc::new),
        };
        let Ok(handle) = opened else {
            continue;
        };
        let _ = rustix::fs::fchmod(&*handle, Mode::RWXU);
        let Ok(entries) = Dir::read_from(&*handle) else {
            continue;
        };
        for entry in entries.flatten() {
            if pending.len() >= budget {
                break;
            }
            let child = entry.file_name();
            if matches!(child.to_bytes(), b"." | b"..")
                || !matches!(entry.file_type(), FileType::Directory | FileType::Unknown)
            {
                continue;
            }
            pending.push((Rc::clone(&handle), Some(child.to_owned())));
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
