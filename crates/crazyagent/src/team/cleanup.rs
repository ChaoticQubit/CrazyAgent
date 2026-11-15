use super::{AGENT_COLUMNS, AgentRole, TeamAgent, agent_row};
use crate::cleanup_settings::CleanupSettings;
use crate::engine::EngineError;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

pub const RECLAIMABLE_MAX: usize = 32;
pub const CANDIDATES_MAX: usize = 2 * RECLAIMABLE_MAX;
const DISK_ENTRIES_MAX: usize = 200_000;
const LOSSES_MAX: usize = 256;

static RUNNING: Mutex<()> = Mutex::new(());

/// One finished team agent that still holds a worktree: its id, role, label and harness,
/// when it stopped, the bytes its worktree holds on disk, with `counted_all` false when the
/// count stopped at its bound and the size is a floor, and the plain code of the check that
/// keeps it, None when the owner may remove its worktree.
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
    #[serde(default)]
    pub kept: Option<String>,
}

/// What one removal did: the agents whose worktree is gone, in the
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

/// Says whether an agent's worktree record waits in `team_retiring` for its files to go,
/// a record marked as left behind included, since a later removal deletes what is at its
/// path, which a fresh start would reuse; a task sent to it then waits instead of failing
/// or starting (F10). Inputs: the store, the project and the agent. Output: the answer,
/// false before the table is made.
pub(crate) fn retiring(db: &Connection, project: &ProjectId, id: i64) -> Result<bool, EngineError> {
    let table: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='team_retiring')",
            [],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !table {
        return Ok(false);
    }
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM team_retiring WHERE agent=?1 AND project=?2)",
        params![id, project.as_str()],
        |row| row.get(0),
    )
    .map_err(unavailable)
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

/// Says whether a worktree record names a place cleanup may delete (DEC-123 rv-secreview
/// L1): its repository is this project's own folder as the runtime records it, canonical,
/// and the worktree is a named folder directly inside that folder's `.crazyagent/worktrees`,
/// or `worktrees` on the old layout. Inputs: the store, the project and the place. Output:
/// the answer, false for a project with no stored setup or a folder that no longer resolves.
fn placed(db: &Connection, project: &ProjectId, place: &Place) -> Result<bool, EngineError> {
    let Some(stored) = crate::project_setup::load_setup(db, project).map_err(unavailable)? else {
        return Ok(false);
    };
    let Ok(folder) = stored.project_folder().canonicalize() else {
        return Ok(false);
    };
    assert!(folder.is_absolute());
    let state = folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
    let parent = place.worktree.parent();
    let inside = place.worktree.file_name().is_some()
        && [state.join("worktrees"), folder.join("worktrees")]
            .iter()
            .any(|worktrees| parent == Some(worktrees.as_path()));
    assert!(!inside || place.worktree.starts_with(&folder));
    Ok(inside && place.repository == folder)
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
    if !placed(db, project, &target.place)? {
        return Ok(Err("cleanup_worktree_invalid"));
    }
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

/// Lists a project's finished agents that still hold a worktree, newest first: each one the
/// owner may remove, with the size of its worktree on disk, and each one a check keeps, with
/// the plain code a removal would answer. An agent may go when it ended, is not the master the
/// runtime starts again, has no unfinished task pinned to it, no open or restorable change set
/// and a worktree that holds exactly the commit its next change set would open above, or when
/// it was confirmed before and stopped part way, such as by a leftover git could not delete.
/// It reads at most `CANDIDATES_MAX` ended agents and returns at most `RECLAIMABLE_MAX` of
/// each kind. Inputs: the store, the project and the time now. Output: the agents.
pub fn finished(
    db: &Connection,
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
    let (mut removable, mut kept) = (0, 0);
    let mut budget = DISK_ENTRIES_MAX;
    for agent in candidates {
        let (bytes, counted_all, code) = match check(db, project, agent.id, now)? {
            Ok(_) if removable == RECLAIMABLE_MAX => continue,
            Ok(target) => {
                removable += 1;
                let (bytes, all) = if missing(&target.place.worktree) {
                    (0, true)
                } else {
                    disk_bytes(&target.place.worktree, &mut budget)
                };
                (bytes, all, None)
            }
            Err(_) if kept == RECLAIMABLE_MAX => continue,
            Err(code) => {
                kept += 1;
                (0, true, Some(code.to_owned()))
            }
        };
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
            bytes,
            counted_all,
            kept: code,
        });
    }
    assert!(removable <= RECLAIMABLE_MAX && kept <= RECLAIMABLE_MAX);
    assert_eq!(found.len(), removable + kept);
    Ok(found)
}

/// What one pass of the engine's own cleanup did: the agents whose worktree it removed, in
/// id order, the id the next pass starts after, 0 once this pass read the last finished agent
/// so agents a check keeps never hold the others back, one line for each agent the engine
/// could not check or remove, and one for each agent its check let through but its removal
/// then kept, with the plain code that kept it, at most `CANDIDATES_MAX` lines in all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Swept {
    pub removed: Vec<i64>,
    pub after: i64,
    pub failures: Vec<String>,
    pub kept: Vec<String>,
}

/// Removes, on the engine's own schedule, the worktrees of finished agents in every project
/// the owner's setting lets go: past its wait since the agent finished, an agent with no
/// finish time counting as past it, each one checked and removed as Clean up does, one at a
/// time, so one a check keeps never stops the others. Nothing goes when only the owner's
/// Clean up removes. A pass reads at most `CANDIDATES_MAX` agents with an id above `after`
/// and removes at most `RECLAIMABLE_MAX`. Inputs: the store, the engine root, the setting,
/// where the last pass stopped and the time now. Output: what this pass did.
pub fn sweep(
    db: &Connection,
    runtime: &Path,
    settings: &CleanupSettings,
    after: i64,
    now_ms: i64,
) -> Result<Swept, EngineError> {
    assert!(after >= 0);
    let Some(wait_ms) = settings.wait_ms() else {
        return Ok(Swept {
            removed: Vec::new(),
            after: 0,
            failures: Vec::new(),
            kept: Vec::new(),
        });
    };
    let now = landing_time(now_ms)?;
    let candidates: Vec<(i64, String)> = {
        let _running = running();
        initialize(db)?;
        let mut statement = db
            .prepare(
                "SELECT id,project FROM team_agents AS agent WHERE id>?1
                 AND status IN ('stopped','failed')
                 AND (stopped_ms IS NULL OR stopped_ms<=?2)
                 AND (id IN (SELECT agent FROM team_worktrees WHERE project=agent.project)
                      OR id IN (SELECT agent FROM team_retiring WHERE project=agent.project))
                 ORDER BY id LIMIT ?3",
            )
            .map_err(unavailable)?;
        statement
            .query_map(
                params![after, now_ms.saturating_sub(wait_ms), CANDIDATES_MAX as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(unavailable)?
            .collect::<Result<_, _>>()
            .map_err(unavailable)?
    };
    let mut swept = Swept {
        removed: Vec::new(),
        after: 0,
        failures: Vec::new(),
        kept: Vec::new(),
    };
    for (id, project) in &candidates {
        if swept.removed.len() == RECLAIMABLE_MAX {
            break;
        }
        assert!(*id > swept.after);
        swept.after = *id;
        if let Err(problem) = sweep_one(db, runtime, project, *id, now, now_ms, &mut swept) {
            swept.failures.push(format!(
                "agent={id} cause={}",
                crate::protocol::plain_code(&problem.0)
            ));
        }
    }
    if candidates.len() < CANDIDATES_MAX
        && candidates.last().map(|last| last.0) == Some(swept.after)
    {
        swept.after = 0;
    }
    assert!(swept.removed.len() <= RECLAIMABLE_MAX);
    assert!(candidates.len() <= CANDIDATES_MAX);
    assert!(swept.removed.len() + swept.kept.len() + swept.failures.len() <= candidates.len());
    Ok(swept)
}

/// Checks one agent as Clean up would and removes its worktree when nothing keeps it. Inputs:
/// the store, its project, the agent, the time now in both forms and the pass it adds to:
/// the agent once removed, or the code its removal kept it with. Output: Ok whether it went
/// or stayed, or the problem that stopped the check.
fn sweep_one(
    db: &Connection,
    runtime: &Path,
    project: &str,
    id: i64,
    now: u64,
    now_ms: i64,
    swept: &mut Swept,
) -> Result<(), EngineError> {
    let project = ProjectId::new(project).map_err(unavailable)?;
    let free = {
        let _running = running();
        check(db, &project, id, now)?.is_ok()
    };
    if !free {
        return Ok(());
    }
    let removal = remove(db, runtime, &project, &[id], now_ms)?;
    match removal.kept {
        None => {
            assert_eq!(removal.agents, [id]);
            swept.removed.push(id);
        }
        Some(code) => {
            assert!(removal.agents.is_empty());
            swept.kept.push(format!(
                "agent={id} kept={}",
                crate::protocol::plain_code(&code)
            ));
        }
    }
    Ok(())
}

/// Removes the worktrees of the owner's chosen finished agents in the order asked: every
/// agent is checked and claimed in one store transaction, which moves its worktree record to
/// `team_retiring` so no task or revive picks it up meanwhile, then every worktree is
/// checked again, then each is removed with git, never forcing, and only then its record.
/// A stop hands back the record of every claimed agent whose worktree is still there. The
/// agent's folder with its conversations, its sandbox profile, its rows, messages and logs
/// stay as history; on macOS the tool caches One per agent gave it go with the worktree
/// (T8). Inputs: the store, the engine root, the project, the agents and the time now.
/// Output: the agents removed and the plain code of the first one kept.
pub fn remove(
    db: &Connection,
    runtime: &Path,
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
    let kept = match retire_claimed(db, runtime, project, &claimed, now, &mut removed) {
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

/// Checks every claimed worktree again, then per agent checks its row once more and removes
/// its worktree, deleting what git left when it removed its record only part way, pinned
/// entries included (T8-R2-N4), and last its waiting record, in claim order. A worktree
/// whose leftover cannot be deleted marks its
/// waiting record, so a later removal deletes the leftover without asking git again
/// (RVD-C-N1). Inputs: the store, the project, the claims, the time now and the list each
/// removed agent joins. Output: None once all are removed, or the plain code of the first
/// one kept.
fn retire_claimed(
    db: &Connection,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] runtime: &Path,
    project: &ProjectId,
    claimed: &[Target],
    now: u64,
    removed: &mut Vec<i64>,
) -> Result<Option<&'static str>, EngineError> {
    for target in claimed {
        if !placed(db, project, &target.place)? {
            return Ok(Some("cleanup_worktree_invalid"));
        }
    }
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
            if !missing(&place.worktree) && remove_agent_tree(&place.worktree).is_err() {
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
                    if remove_agent_tree(&place.worktree).is_err() {
                        return Ok(Some("cleanup_worktree_leftover"));
                    }
                }
                Err(code) => return Ok(Some(code)),
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(project_root) = place.repository.to_str()
            && crate::sandbox::tool_cache::remove_agent(
                runtime,
                project_root,
                &target.id.to_string(),
            )
            .is_err()
        {
            return Ok(Some("cleanup_folder_failed"));
        }
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
            if !placed(&tx, project, &place)? {
                return Ok(Err("cleanup_worktree_invalid"));
            }
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
        if !placed(&tx, project, &place)? {
            return Ok(Err("cleanup_worktree_invalid"));
        }
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

/// Deletes the agent folder and sandbox profile of a deleted project's agent, with a native
/// lead's teammates' profile, teammates' setup profile and the records of its teammates'
/// socket name and server pid (rv-audit TTDL F2, OP-9b, PLC c PC-L1), making its read-only
/// folders writable first, such as a module cache a toolchain left. The folder goes last, so
/// a retry after a failure still finds what a delete's record matches. Inputs: the team state
/// folder and the agent. Output: Ok once all are gone.
pub(crate) fn remove_agent_files(team: &Path, id: i64) -> Result<(), EngineError> {
    for file in [
        profile(team, id),
        super::runtime::agent_teammates_profile(team, id),
        super::runtime::agent_teammates_setup_profile(team, id),
        super::runtime::agent_teammates_socket_name(team, id),
        super::runtime::agent_teammates_server_pid(team, id),
    ] {
        gone(std::fs::remove_file(file))?;
    }
    remove_tree(&agent_folder(team, id))
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
pub(crate) fn remove_tree(folder: &Path) -> Result<(), EngineError> {
    if std::fs::remove_dir_all(folder).is_err() {
        writable(folder, Flags::Kept, Acls::Kept);
        gone(std::fs::remove_dir_all(folder))?;
    }
    Ok(())
}

/// Deletes a folder an agent wrote as [`remove_tree`] does and, when a plain delete stops,
/// also clears the immutable and append-only flags of this user's entries on the way and
/// the extended ACLs in it ([`remove_pinned_tree`]), so an agent's `chflags uchg`,
/// `chmod 0500` or deny entry in its own tool cache or worktree keeps neither Clean up nor
/// a project's delete from removing it (T8 r1 L3, T8-R2-N4, RV-DS241-137 sec r1 F2). ACLs
/// go only through entries the walk reached from the tree's own handles, never a path
/// (RV-DS241-181). Input: the folder. Output: Ok once nothing has its name.
pub(crate) fn remove_agent_tree(folder: &Path) -> Result<(), EngineError> {
    remove_pinned_tree(folder, Flags::Cleared)
}

/// The deepest folder below an entry of an agent home that a launch's reset removes. Past
/// it the removal stops and the launch is refused, so the walk holds at most this many
/// folder handles at once.
#[cfg(unix)]
const HOME_DEPTH_MAX: usize = 64;

/// The most names one listing of an agent home folder holds ([`home_names`]): a folder
/// holding more is emptied a listing at a time, so what a removal keeps in memory stays
/// bounded whatever the folder holds (RV-DS241-137 code r1 F1 ds137-reset-walk).
#[cfg(unix)]
const HOME_NAMES_CHUNK: usize = 4096;

/// How many listings' worth of entries past those a folder held when its emptying began
/// [`empty_home_folder`] removes before it refuses, as only a process still adding entries
/// keeps a folder from listing empty by then.
#[cfg(unix)]
const HOME_ADDED_CHUNKS_MAX: usize = 8;

/// Removes the entry `name` of an open agent home folder with all it holds, each step
/// relative to an open folder handle and none following a link, so a folder a process an
/// agent left swaps for a link meanwhile goes as that link, what the link names stays, and
/// a folder moved away is emptied where it went (RV-DS241-137, sec F1 ds137-lock-linux). A
/// step that stops on a flag, mode or ACL an agent set clears it and tries once more
/// ([`open_home_folder`], [`unlink_home_entry`]), so a reset never stops on one (code r1
/// F2, r2 F3), and no count of entries stops it (code r1 F1 ds137-reset-walk). Inputs: the
/// parent handle and the name, one plain name. Output: Ok once nothing has the name, or
/// `cleanup_folder_failed`, also past [`HOME_DEPTH_MAX`] folders deep or for a folder that
/// keeps gaining entries while it is emptied ([`empty_home_folder`]).
#[cfg(unix)]
pub(crate) fn remove_home_entry<P: rustix::path::Arg>(
    parent: impl std::os::fd::AsFd,
    name: P,
) -> Result<(), EngineError> {
    let name = name.into_c_str().map_err(|_| folder_failed())?;
    remove_home_below(parent.as_fd(), &name, 0)
}

/// Removes one entry as [`remove_home_entry`] does, `depth` folders below the entry it was
/// given. Inputs: the parent handle, the name and the depth. Output: as
/// [`remove_home_entry`].
#[cfg(unix)]
fn remove_home_below(
    parent: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::CStr,
    depth: usize,
) -> Result<(), EngineError> {
    use rustix::fs::{AtFlags, FileType};
    assert!(plain_name(name));
    if depth > HOME_DEPTH_MAX {
        return Err(folder_failed());
    }
    let found = match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(_) => return Err(folder_failed()),
        Ok(found) => found,
    };
    if FileType::from_raw_mode(found.st_mode) == FileType::Directory
        && let Some(folder) = open_home_folder(parent, name).map_err(|_| folder_failed())?
    {
        empty_home_folder(&folder, depth)?;
    }
    unlink_home_entry(parent, name)
}

/// Opens the folder `name` of an open agent home folder without following a link, as a
/// folder this user can list: one that opens but will not list, as reopening it as `.` the
/// way [`rustix::fs::Dir::read_from`] does needs search on it, counts as one that stopped
/// (code r1 F2 ds137-reset-walk). An open that stops on an entry that is still a folder
/// clears its immutable and append-only flags ([`unpin_home_entry`]), gives this user rwx on
/// it ([`open_up`]) and clears its ACL by its path ([`release_home_acls`]), each through
/// the parent by name, then opens it once more. Inputs: the parent handle and the name.
/// Output: the folder, None once the name is gone or names no folder, or the error.
#[cfg(unix)]
pub(crate) fn open_home_folder(
    parent: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::CStr,
) -> rustix::io::Result<Option<std::os::fd::OwnedFd>> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags};
    let flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let listing = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let open = || -> rustix::io::Result<std::os::fd::OwnedFd> {
        let opened = rustix::fs::openat(parent, name, flags, Mode::empty())?;
        rustix::fs::openat(&opened, c".", listing, Mode::empty())?;
        Ok(opened)
    };
    let folder = || match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(problem) => Err(problem),
        Ok(found) => Ok(FileType::from_raw_mode(found.st_mode) == FileType::Directory),
    };
    if let Ok(opened) = open() {
        return Ok(Some(opened));
    }
    if !folder()? {
        return Ok(None);
    }
    unpin_home_entry(parent, name);
    let _ = open_up(parent, name);
    release_home_acls(parent, &[name]);
    let reopened = open();
    if reopened.is_err() && !folder()? {
        return Ok(None);
    }
    reopened.map(Some)
}

/// Removes all an open agent home folder holds as [`remove_home_below`] does, once its own
/// flags are cleared and this user has rwx on it: it counts what the folder holds, then
/// lists at most [`HOME_NAMES_CHUNK`] names at a time from a fresh listing and removes them
/// until a listing comes back empty, so no folder's size stops it (code r1 F1
/// ds137-reset-walk). Removing more than the count plus [`HOME_ADDED_CHUNKS_MAX`] listings'
/// worth means a process is still adding entries, and the removal stops. Inputs: the folder
/// and its depth. Output: Ok once a listing finds it empty, or `cleanup_folder_failed`.
#[cfg(unix)]
fn empty_home_folder(folder: &std::os::fd::OwnedFd, depth: usize) -> Result<(), EngineError> {
    use std::os::fd::AsFd;
    unpin_home_entry(folder, c".");
    let _ = rustix::fs::fchmod(folder, rustix::fs::Mode::RWXU);
    let added = HOME_NAMES_CHUNK * HOME_ADDED_CHUNKS_MAX;
    let allowed = home_count(folder.as_fd())?.saturating_add(added);
    let mut removed: usize = 0;
    loop {
        let names = home_names(folder.as_fd())?;
        if names.is_empty() {
            return Ok(());
        }
        removed = removed.saturating_add(names.len());
        if removed > allowed {
            return Err(folder_failed());
        }
        for name in &names {
            remove_home_below(folder.as_fd(), name, depth + 1)?;
        }
    }
}

/// Lists an open agent home folder from its start, clearing its ACL by its path and listing
/// once more when a listing stops. Input: the folder. Output: the listing, or
/// `cleanup_folder_failed` for a folder that will not list.
#[cfg(unix)]
fn home_listing(folder: std::os::fd::BorrowedFd<'_>) -> Result<rustix::fs::Dir, EngineError> {
    rustix::fs::Dir::read_from(folder)
        .or_else(|_| {
            release_home_acls(folder, &[c"."]);
            rustix::fs::Dir::read_from(folder)
        })
        .map_err(|_| folder_failed())
}

/// Counts the names an open agent home folder holds but `.` and `..`, keeping none of them
/// ([`home_listing`]). Input: the folder. Output: the count, or `cleanup_folder_failed`.
#[cfg(unix)]
fn home_count(folder: std::os::fd::BorrowedFd<'_>) -> Result<usize, EngineError> {
    let mut count: usize = 0;
    for entry in home_listing(folder)? {
        let entry = entry.map_err(|_| folder_failed())?;
        let dots = matches!(entry.file_name().to_bytes(), b"." | b"..");
        count = count.saturating_add(usize::from(!dots));
    }
    Ok(count)
}

/// Lists the first [`HOME_NAMES_CHUNK`] names an open agent home folder holds but `.` and
/// `..` ([`home_listing`]). Input: the folder. Output: the names, none for an empty folder,
/// or `cleanup_folder_failed`.
#[cfg(unix)]
fn home_names(folder: std::os::fd::BorrowedFd<'_>) -> Result<Vec<std::ffi::CString>, EngineError> {
    let mut names = Vec::new();
    for entry in home_listing(folder)? {
        let entry = entry.map_err(|_| folder_failed())?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        names.push(name.to_owned());
        if names.len() == HOME_NAMES_CHUNK {
            break;
        }
    }
    assert!(names.len() <= HOME_NAMES_CHUNK);
    Ok(names)
}

/// Unlinks the entry `name` of an open agent home folder, a folder as an empty folder, never
/// following a link. An unlink that stops clears the immutable and append-only flags of the
/// folder and the entry ([`unpin_home_entry`]) and both their ACLs by path
/// ([`release_home_acls`]), then unlinks once more. Inputs: the parent handle and the name.
/// Output: Ok once nothing has the name, or `cleanup_folder_failed`.
#[cfg(unix)]
fn unlink_home_entry(
    parent: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::CStr,
) -> Result<(), EngineError> {
    use rustix::fs::{AtFlags, FileType};
    let unlink = || -> rustix::io::Result<()> {
        let found = match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            found => found?,
        };
        let how = if FileType::from_raw_mode(found.st_mode) == FileType::Directory {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        match rustix::fs::unlinkat(parent, name, how) {
            Err(rustix::io::Errno::NOENT) => Ok(()),
            done => done,
        }
    };
    if unlink().is_ok() {
        return Ok(());
    }
    unpin_home_entry(parent, c".");
    unpin_home_entry(parent, name);
    release_home_acls(parent, &[c".", name]);
    unlink().map_err(|_| folder_failed())
}

/// Clears the ACLs of entries of an open agent home folder for a step that stopped on them,
/// each through its inode as read from the folder's handle without following a link
/// ([`AclClears`]), `.` naming the folder itself, so no path an agent can change is
/// resolved (RV-DS241-181). The folder's own clear runs before the names after it are read,
/// so a folder whose ACL denies searching it no longer hides them (ds181 r2, rv-code4 N1).
/// Inputs: the folder and the names.
#[cfg(target_os = "macos")]
fn release_home_acls(folder: std::os::fd::BorrowedFd<'_>, names: &[&std::ffi::CStr]) {
    use rustix::fs::AtFlags;
    let mut clears = AclClears::new();
    for name in names {
        let found = match name.to_bytes() {
            b"." => rustix::fs::fstat(folder),
            _ => rustix::fs::statat(folder, *name, AtFlags::SYMLINK_NOFOLLOW),
        };
        if let Ok(found) = found {
            clears.add(&found);
        }
        if name.to_bytes() == b"." {
            clears.flush();
        }
    }
    clears.flush();
}

/// Leaves ACLs alone where no deny entry an agent may set keeps an entry from going. Inputs:
/// unused.
#[cfg(all(unix, not(target_os = "macos")))]
fn release_home_acls(_: std::os::fd::BorrowedFd<'_>, _: &[&std::ffi::CStr]) {}

/// Whether a name is one plain entry name: not empty, `.` or `..`, and holding no `/`.
/// Input: the name. Output: the answer.
#[cfg(unix)]
fn plain_name(name: &std::ffi::CStr) -> bool {
    let bytes = name.to_bytes();
    !bytes.is_empty() && !bytes.contains(&b'/') && !matches!(bytes, b"." | b"..")
}

/// The code a removal of an agent home entry that left it in place reports.
#[cfg(unix)]
fn folder_failed() -> EngineError {
    EngineError("cleanup_folder_failed".into())
}

/// Deletes a folder. When a plain delete stops, it is walked clearing flags as `flags`
/// says and the ACLs of folders that will not open or list, so a delete a mode change lets
/// through starts no child (RV-DS241-182); when it still stays, it is walked once more
/// clearing every ACL in it too ([`writable`]). Inputs: the folder and what to do with the
/// flags. Output: Ok once nothing has its name.
fn remove_pinned_tree(folder: &Path, flags: Flags) -> Result<(), EngineError> {
    assert!(flags != Flags::Kept);
    if std::fs::remove_dir_all(folder).is_err() {
        writable(folder, flags, Acls::Stuck);
        if std::fs::remove_dir_all(folder).is_err() {
            writable(folder, flags, Acls::All);
            gone(std::fs::remove_dir_all(folder))?;
        }
    }
    Ok(())
}

/// What a walk that opens up a folder for removal does with the immutable and append-only
/// flags an agent may set.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flags {
    /// Leaves them.
    Kept,
    /// Clears them through each entry's own handle ([`unpin`]).
    Cleared,
}

/// What a walk that opens up a folder for removal does with the extended ACLs an agent may
/// set, which only macOS clears.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Acls {
    /// Leaves them.
    Kept,
    /// Clears those of folders that will not open or list, then walks those folders.
    Stuck,
    /// Also clears those of every folder walked and of every entry in them.
    All,
}

/// The most entries one `/bin/chmod` of an ACL clear is given.
#[cfg(target_os = "macos")]
const ACL_BATCH: usize = 512;

/// The most folders a walk that opens up a folder for removal holds waiting for an ACL clear,
/// each with its parent's open handle. At this many the clear runs and they are walked again
/// at once, so the walk holds at most this many handles beyond those of the folders it is
/// inside (ds181 r2, rv-code4 F2).
#[cfg(unix)]
const STUCK_MAX: usize = 32;

/// Extended ACLs a walk clears in batches, each entry named `/.vol/<device>/<inode>`, which
/// the system looks up by those numbers alone, so `/bin/chmod -N` resolves no path an agent
/// can change and follows no link in one (RV-DS241-181). A folder or a file with one link
/// goes through that name, a link with one link through `-h`, so its target never changes.
/// A file with a second link, which may be the owner's file an agent linked in, is never
/// taken (V231-ACL). At most [`DISK_ENTRIES_MAX`] entries are taken, and one child gets at
/// most [`ACL_BATCH`] of them (RV-DS241-182). An entry is taken once per walk: one waiting
/// for the next clear is answered as taken again, so every folder that needs that clear is
/// tried after it, and one already cleared is refused, so a folder the clear did not help is
/// not tried again (ds181 r2, rv-code4 F2). Holds the entries waiting by name and by
/// device and inode, those already cleared, and how many more it takes.
#[cfg(target_os = "macos")]
struct AclClears {
    named: Vec<PathBuf>,
    links: Vec<PathBuf>,
    waiting: std::collections::HashSet<(i64, u64)>,
    cleared: std::collections::HashSet<(i64, u64)>,
    left: usize,
}

#[cfg(target_os = "macos")]
impl AclClears {
    /// An empty batch that takes up to [`DISK_ENTRIES_MAX`] entries. Output: the batch.
    fn new() -> Self {
        Self {
            named: Vec::new(),
            links: Vec::new(),
            waiting: std::collections::HashSet::new(),
            cleared: std::collections::HashSet::new(),
            left: DISK_ENTRIES_MAX,
        }
    }

    /// Takes the entry `found` describes, clearing the batch once [`ACL_BATCH`] wait. Input:
    /// the entry's status, read without following a link. Output: whether it was taken now or
    /// waits from earlier; false for an entry already cleared in this walk or one never taken.
    fn add(&mut self, found: &rustix::fs::Stat) -> bool {
        use rustix::fs::FileType;
        let kind = FileType::from_raw_mode(found.st_mode);
        let key = (i64::from(found.st_dev), found.st_ino);
        if self.waiting.contains(&key) {
            return true;
        }
        if self.cleared.contains(&key)
            || self.left == 0
            || (kind != FileType::Directory && found.st_nlink != 1)
        {
            return false;
        }
        self.left -= 1;
        self.waiting.insert(key);
        let entry = PathBuf::from(format!("/.vol/{}/{}", found.st_dev, found.st_ino));
        let waiting = if kind == FileType::Symlink {
            &mut self.links
        } else {
            &mut self.named
        };
        waiting.push(entry);
        if waiting.len() >= ACL_BATCH {
            self.flush();
        }
        true
    }

    /// Clears the ACLs of the entries waiting, in at most two children, and empties the
    /// batch. A child that fails or does not start leaves those ACLs, and the step that needs
    /// an entry then reports it.
    fn flush(&mut self) {
        use std::process::{Command, Stdio};
        assert_eq!(self.waiting.len(), self.named.len() + self.links.len());
        assert!(self.named.len() <= ACL_BATCH && self.links.len() <= ACL_BATCH);
        self.cleared.extend(self.waiting.drain());
        let batches = [
            (&["-f", "-N"][..], &mut self.named),
            (&["-f", "-h", "-N"][..], &mut self.links),
        ];
        for (flags, waiting) in batches {
            if waiting.is_empty() {
                continue;
            }
            let _ = crate::spawn_gate::status(
                Command::new("/bin/chmod")
                    .args(flags)
                    .args(waiting.iter())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
            waiting.clear();
        }
    }
}

/// Clears no ACL where no deny entry an agent may set keeps an entry from going.
#[cfg(all(unix, not(target_os = "macos")))]
struct AclClears;

#[cfg(all(unix, not(target_os = "macos")))]
impl AclClears {
    /// The batch. Output: it.
    fn new() -> Self {
        Self
    }

    /// Takes nothing. Input: unused. Output: false.
    fn add(&mut self, _found: &rustix::fs::Stat) -> bool {
        false
    }

    /// Does nothing.
    fn flush(&mut self) {}
}

/// Clears the flags of one entry as `flags` says. Inputs: what to do, the entry's open
/// parent and its name there.
#[cfg(unix)]
fn clear_flags<P: rustix::path::Arg + Copy>(flags: Flags, parent: impl std::os::fd::AsFd, name: P) {
    match flags {
        Flags::Kept => {}
        Flags::Cleared => unpin(parent, name),
    }
}

/// Clears the extended ACL of each entry without following a link in its last part, through
/// `/bin/chmod -f -h -N`, as no call this crate makes without `unsafe` sets an ACL. A deny
/// entry an agent set binds the owner too, so it would keep an entry from going or being
/// written (RV-DS241-137 sec r1 F2). Only a folder or an entry with one link is cleared, so
/// a file with a second link, which may be the owner's file an agent linked in, never
/// changes (V231-ACL, sec r2 F1). Input: the entries. An entry that is gone or skipped, a
/// pinned one or a spawn that fails leaves things as they were, and the step that needs the
/// entry then reports it.
#[cfg(target_os = "macos")]
pub(crate) fn clear_acls<'a>(entries: impl IntoIterator<Item = &'a Path>) {
    use std::os::unix::fs::MetadataExt;
    let cleared = |entry: &&Path| {
        std::fs::symlink_metadata(entry).is_ok_and(|found| found.is_dir() || found.nlink() == 1)
    };
    let cleared: Vec<&Path> = entries.into_iter().filter(cleared).collect();
    chmod_acls(&["-f", "-h", "-N", "--"], &cleared);
}

/// Leaves ACLs alone where no deny entry an agent may set keeps an entry from going. Input:
/// unused.
#[cfg(not(target_os = "macos"))]
pub(crate) fn clear_acls<'a>(_entries: impl IntoIterator<Item = &'a Path>) {}

/// Clears the extended ACL of each entry itself for a step that stopped on it, as a launch
/// context's release clears what a tree holds: a folder and a one-link file by their paths,
/// so a folder whose ACL denies listing or searching and a file whose ACL denies reading
/// lose it too, where [`clear_acls`] opens them and fails, and a one-link link on the link
/// itself. A file with a second link, which may be the owner's file an agent linked in,
/// never changes (V231-ACL, RV-DS241-137 code r2 F3). A name a process swaps for a link
/// between the check and the clear is followed, as in that release. Input: the entries.
/// What stays is reported by the step that needs the entry.
#[cfg(target_os = "macos")]
pub(crate) fn release_acls<'a>(entries: impl IntoIterator<Item = &'a Path>) {
    use std::os::unix::fs::MetadataExt;
    let mut by_path = Vec::new();
    let mut links = Vec::new();
    for entry in entries {
        let Ok(found) = std::fs::symlink_metadata(entry) else {
            continue;
        };
        if found.is_symlink() {
            if found.nlink() == 1 {
                links.push(entry);
            }
        } else if found.is_dir() || found.nlink() == 1 {
            by_path.push(entry);
        }
    }
    chmod_acls(&["-f", "-N", "--"], &by_path);
    chmod_acls(&["-f", "-h", "-N", "--"], &links);
}

/// Leaves ACLs alone where no deny entry an agent may set keeps an entry from going. Input:
/// unused.
#[cfg(not(target_os = "macos"))]
pub(crate) fn release_acls<'a>(_entries: impl IntoIterator<Item = &'a Path>) {}

/// Runs `/bin/chmod` with `flags`, which end in `--`, on the entries, its output dropped,
/// and nothing when there are none. Inputs: the flags and the entries. A spawn that fails
/// changes nothing.
#[cfg(target_os = "macos")]
fn chmod_acls(flags: &[&str], entries: &[&Path]) {
    use std::process::Stdio;
    assert_eq!(flags.last(), Some(&"--"));
    if entries.is_empty() {
        return;
    }
    let _ = crate::spawn_gate::status(
        std::process::Command::new("/bin/chmod")
            .args(flags)
            .args(entries)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
}

/// Lets the owner delete inside every real folder under `folder`, visiting at most
/// `DISK_ENTRIES_MAX` folders. Every folder is opened relative to its parent's open
/// handle without following a link, and changed and listed through its own handle, so a
/// link swapped in meanwhile is never followed. Unless `pins` keeps them, the flags
/// [`unpin`] clears go first from the folder, each folder below it and each entry in them.
/// ACLs go as `acls` says, each through the inode of an entry read from a handle the walk
/// holds ([`AclClears`]): a folder that will not open or list has its own ACL cleared, or
/// its parent's when even its status cannot be read there ([`stuck_clear`]), and is walked
/// again once that clear has run, with at most [`STUCK_MAX`] such folders waiting at once;
/// [`Acls::All`] also clears every folder walked and every entry in them. Inputs: the
/// folder, what to do with those flags and what to do with ACLs.
#[cfg(unix)]
fn writable(folder: &Path, pins: Flags, acls: Acls) {
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
    use std::ffi::CString;
    use std::rc::Rc;
    let pinned = pins != Flags::Kept;
    let flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let mut clears = AclClears::new();
    if pinned {
        clear_flags(pins, rustix::fs::CWD, folder);
        let _ = open_up(rustix::fs::CWD, folder);
    }
    let open = || rustix::fs::open(folder, flags, Mode::empty());
    let opened = open().or_else(|problem| {
        if acls == Acls::Kept {
            return Err(problem);
        }
        let found = rustix::fs::statat(rustix::fs::CWD, folder, AtFlags::SYMLINK_NOFOLLOW)?;
        clears.add(&found);
        clears.flush();
        open()
    });
    let Ok(top) = opened else {
        return;
    };
    let mut pending: Vec<(Rc<std::os::fd::OwnedFd>, Option<CString>)> = vec![(Rc::new(top), None)];
    let mut stuck = Vec::new();
    let mut budget = DISK_ENTRIES_MAX;
    loop {
        let Some((parent, name)) = pending.pop() else {
            if stuck.is_empty() {
                break;
            }
            clears.flush();
            pending.append(&mut stuck);
            continue;
        };
        if budget == 0 {
            break;
        }
        budget -= 1;
        let opened = match &name {
            None => Ok(Rc::clone(&parent)),
            Some(name) => rustix::fs::openat(&*parent, name.as_c_str(), flags, Mode::empty())
                .or_else(|_| {
                    clear_flags(pins, &*parent, name.as_c_str());
                    open_up(&*parent, name.as_c_str())?;
                    rustix::fs::openat(&*parent, name.as_c_str(), flags, Mode::empty())
                })
                .map(Rc::new),
        };
        let listed = opened.and_then(|handle| {
            clear_flags(pins, &*handle, c".");
            let _ = rustix::fs::fchmod(&*handle, Mode::RWXU);
            let entries = Dir::read_from(&*handle)?;
            Ok((handle, entries))
        });
        let Ok((handle, entries)) = listed else {
            if acls != Acls::Kept && stuck_clear(&mut clears, &parent, name.as_deref()) {
                stuck.push((parent, name));
                #[cfg(test)]
                STUCK_PEAK.set(STUCK_PEAK.get().max(stuck.len()));
                if stuck.len() == STUCK_MAX {
                    clears.flush();
                    pending.append(&mut stuck);
                }
            }
            continue;
        };
        if acls == Acls::All
            && let Ok(found) = rustix::fs::fstat(&*handle)
        {
            clears.add(&found);
        }
        for entry in entries.flatten() {
            if pending.len() >= budget {
                break;
            }
            let child = entry.file_name();
            if matches!(child.to_bytes(), b"." | b"..") {
                continue;
            }
            if !matches!(entry.file_type(), FileType::Directory | FileType::Unknown) {
                clear_flags(pins, &*handle, child);
                if acls == Acls::All
                    && let Ok(found) =
                        rustix::fs::statat(&*handle, child, AtFlags::SYMLINK_NOFOLLOW)
                {
                    clears.add(&found);
                }
                continue;
            }
            pending.push((Rc::clone(&handle), Some(child.to_owned())));
        }
    }
    clears.flush();
}

#[cfg(not(unix))]
fn writable(_: &Path, _: Flags, _: Acls) {}

/// Queues the ACL clear after which a folder a walk could not open or list may open: its
/// own, or its parent's when even its status cannot be read there, which is a parent whose
/// ACL denies searching it (ds181 r2, rv-code4 F1). Inputs: the batch, the parent's handle,
/// and the folder's name there, none for the walk's top folder. Output: whether a clear the
/// next try can see now waits.
#[cfg(unix)]
fn stuck_clear(
    clears: &mut AclClears,
    parent: &std::os::fd::OwnedFd,
    name: Option<&std::ffi::CStr>,
) -> bool {
    use rustix::fs::{AtFlags, FileType};
    let found = match name {
        None => rustix::fs::fstat(parent),
        Some(name) => rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW),
    };
    match found {
        Ok(found) => {
            FileType::from_raw_mode(found.st_mode) == FileType::Directory && clears.add(&found)
        }
        Err(rustix::io::Errno::ACCESS) => {
            rustix::fs::fstat(parent).is_ok_and(|above| clears.add(&above))
        }
        Err(_) => false,
    }
}

/// Gives this user rwx on the folder `name` under `parent` without following a link. Linux
/// `fchmodat` cannot refuse a link, so there the folder is pinned by an `O_PATH` handle that
/// refuses a link and changed through `/proc/self/fd/<n>` of that handle; elsewhere
/// `chmodat` refuses the link itself. Inputs: the parent handle and the name. Output: Ok,
/// or the error that left the folder as it was.
#[cfg(target_os = "linux")]
fn open_up<P: rustix::path::Arg>(
    parent: impl std::os::fd::AsFd,
    name: P,
) -> rustix::io::Result<()> {
    use rustix::fs::{Mode, OFlags};
    use std::os::fd::AsRawFd;
    let pinned = rustix::fs::openat(
        parent,
        name,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    rustix::fs::chmod(
        format!("/proc/self/fd/{}", pinned.as_raw_fd()).as_str(),
        Mode::RWXU,
    )
}

/// Gives this user rwx on the folder `name` under `parent` without following a link, through
/// `chmodat` with `AT_SYMLINK_NOFOLLOW`. Inputs: the parent handle and the name. Output: Ok,
/// or the error that left the folder as it was.
#[cfg(all(unix, not(target_os = "linux")))]
fn open_up<P: rustix::path::Arg>(
    parent: impl std::os::fd::AsFd,
    name: P,
) -> rustix::io::Result<()> {
    rustix::fs::chmodat(
        parent,
        name,
        rustix::fs::Mode::RWXU,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
}

#[cfg(all(test, unix))]
thread_local! {
    /// The most folders one walk that opens up a folder for removal on this thread held
    /// waiting for an ACL clear at once, each with its parent's open handle, so a row can
    /// bound the handles that walk keeps open.
    pub(super) static STUCK_PEAK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    /// A step a cleanup test runs in [`unpin`] once a pinned entry is open and before its
    /// flags change, standing in for a process an agent left that swaps a folder meanwhile.
    /// Runs once; None runs nothing.
    pub(super) static AFTER_UNPIN_OPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };

    /// A step a cleanup test runs in [`unpin_home_entry`] once it has named an entry that
    /// will not open for `/usr/bin/chflags` and before that runs, standing in for a process
    /// an agent left that swaps a folder meanwhile. Runs once; None runs nothing.
    pub(super) static BEFORE_HOME_ENTRY_CHFLAGS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Clears the immutable and append-only flags, which an agent may set on what it owns, from
/// one entry of this user that is not a link, keeping every other flag (T8 r1 L3). The
/// entry is opened from its parent's handle without following a link, and its flags are set
/// through `/dev/fd/<n>` of that open entry, so no path an agent can change is resolved and
/// a folder or link it swaps in meanwhile is never followed (T8 r2 L1). An entry that will
/// not open, such as one at mode 000, keeps its flags; the removal then stops on it and its
/// caller reports what is left. A file with a second link keeps its flags too, since it may
/// be the owner's file an agent linked in (V231-ACL, RV-DS241-137 code r2 F2). A launch
/// clears its Shared tool cache folders the same way (T8-R2-N1). Inputs: the entry's open
/// parent and its name there.
#[cfg(target_os = "macos")]
pub(crate) fn unpin<P: rustix::path::Arg>(parent: impl std::os::fd::AsFd, name: P) {
    use nix::sys::stat::FileFlag;
    use rustix::fs::{FileType, Mode, OFlags};
    use std::os::fd::AsRawFd;
    let pinning = FileFlag::UF_IMMUTABLE | FileFlag::UF_APPEND;
    let open = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let Ok(entry) = rustix::fs::openat(parent, name, open, Mode::empty()) else {
        return;
    };
    let Ok(found) = rustix::fs::fstat(&entry) else {
        return;
    };
    assert_ne!(
        FileType::from_raw_mode(found.st_mode),
        FileType::Symlink,
        "an entry opened without following a link is never one"
    );
    let file = FileType::from_raw_mode(found.st_mode) != FileType::Directory;
    let flags = FileFlag::from_bits_retain(found.st_flags);
    if !flags.intersects(pinning)
        || (file && found.st_nlink != 1)
        || found.st_uid != rustix::process::geteuid().as_raw()
    {
        return;
    }
    #[cfg(test)]
    if let Some(step) = AFTER_UNPIN_OPEN.with(|step| step.borrow_mut().take()) {
        step();
    }
    let handle = PathBuf::from(format!("/dev/fd/{}", entry.as_raw_fd()));
    let _ = nix::unistd::chflags(&handle, flags.difference(pinning));
}

/// Clears the immutable and append-only flags of one entry of an agent home as [`unpin`]
/// does and, when the entry will not open, such as one at mode 000, which changing flags
/// does not need to read, by its path (RV-DS241-137 code r1 F2): its flags are read from its
/// parent's handle without following a link, and only when one of them is set is
/// `/usr/bin/chflags -h` run on its path, which changes a link itself rather than what it
/// names. That path is resolved again, so a folder above it that a process swaps meanwhile
/// is followed, which is why only an agent home's removal uses it: there a confined agent
/// can set no flag (the profile denies it) and a Full one has the owner's rights already,
/// while a Shared tool cache keeps [`unpin`] alone (T8-R2-F1). A file with a second link
/// keeps its flags, as with [`unpin`]. Inputs: the entry's parent and its name there,
/// absolute when the parent is the working folder.
#[cfg(target_os = "macos")]
pub(crate) fn unpin_home_entry<P: rustix::path::Arg + Copy>(
    parent: impl std::os::fd::AsFd,
    name: P,
) {
    use nix::sys::stat::FileFlag;
    use rustix::fs::{AtFlags, FileType};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let parent = parent.as_fd();
    let pinning = FileFlag::UF_IMMUTABLE | FileFlag::UF_APPEND;
    let pinned = || {
        rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).is_ok_and(|found| {
            let file = FileType::from_raw_mode(found.st_mode) != FileType::Directory;
            let flags = FileFlag::from_bits_retain(found.st_flags);
            flags.intersects(pinning)
                && !(file && found.st_nlink != 1)
                && found.st_uid == rustix::process::geteuid().as_raw()
        })
    };
    if !pinned() {
        return;
    }
    unpin(parent, name);
    if !pinned() {
        return;
    }
    let Ok(name) = name.as_cow_c_str() else {
        return;
    };
    let name = Path::new(OsStr::from_bytes(name.to_bytes()));
    let path = if name.is_absolute() {
        name.to_path_buf()
    } else {
        let Ok(folder) = rustix::fs::getpath(parent) else {
            return;
        };
        Path::new(OsStr::from_bytes(folder.to_bytes())).join(name)
    };
    #[cfg(test)]
    if let Some(step) = BEFORE_HOME_ENTRY_CHFLAGS.with(|step| step.borrow_mut().take()) {
        step();
    }
    let _ = crate::spawn_gate::status(
        std::process::Command::new("/usr/bin/chflags")
            .args(["-h", "nouchg,nouappnd", "--"])
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    );
}

/// Leaves flags alone where no flag an agent may set keeps an entry from going. Inputs:
/// unused.
#[cfg(all(unix, not(target_os = "macos")))]
fn unpin<P: rustix::path::Arg>(_: impl std::os::fd::AsFd, _: P) {}

/// Leaves flags alone where no flag an agent may set keeps an entry of its home from going.
/// Inputs: unused.
#[cfg(all(unix, not(target_os = "macos")))]
fn unpin_home_entry<P: rustix::path::Arg + Copy>(_: impl std::os::fd::AsFd, _: P) {}

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
