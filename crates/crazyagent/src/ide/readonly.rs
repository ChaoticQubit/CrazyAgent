use super::workspace::WorkspaceSettings;
use crate::engine::EngineError;
use crate::engine::owner::failure;
use crate::protocol::ProjectId;
use rusqlite::{Connection, OptionalExtension, params};

/// The most files the owner may unlock in one agent's worktree during one agent turn.
pub const UNLOCKED_PER_AGENT_MAX: i64 = 256;

const AGENTS_GUARDED_MAX: i64 = 256;
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ide_unlocked (
    agent INTEGER NOT NULL,
    path TEXT NOT NULL,
    turns INTEGER NOT NULL,
    PRIMARY KEY (agent, path)
);";
const RUNNING: &str = "a.status IN ('starting','working')";

/// The read-only globs of a project's workspace: every file of each running agent's
/// worktree, less the files the owner unlocked during the agent's current turn.
/// Inputs: the engine database and project. Output: the settings, in agent id order.
pub fn settings(db: &Connection, project: &ProjectId) -> Result<WorkspaceSettings, EngineError> {
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    let mut running = db
        .prepare(&format!(
            "SELECT w.worktree FROM team_agents a
             JOIN team_worktrees w ON w.agent=a.id AND w.project=a.project
             WHERE a.project=?1 AND {RUNNING} ORDER BY a.id LIMIT ?2"
        ))
        .map_err(unavailable)?;
    let readonly_include: Vec<String> = running
        .query_map(params![project.as_str(), AGENTS_GUARDED_MAX], |row| {
            row.get::<_, String>(0)
        })
        .map_err(unavailable)?
        .map(|worktree| worktree.map(|worktree| format!("{}/**", literal(&worktree))))
        .collect::<Result<_, _>>()
        .map_err(unavailable)?;
    let mut unlocked = db
        .prepare(&format!(
            "SELECT w.worktree, u.path FROM ide_unlocked u
             JOIN team_agents a ON a.id=u.agent AND u.turns=a.turns
             JOIN team_worktrees w ON w.agent=a.id AND w.project=a.project
             WHERE a.project=?1 AND {RUNNING} ORDER BY a.id, u.path LIMIT ?2"
        ))
        .map_err(unavailable)?;
    let readonly_exclude: Vec<String> = unlocked
        .query_map(
            params![
                project.as_str(),
                AGENTS_GUARDED_MAX * UNLOCKED_PER_AGENT_MAX
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(unavailable)?
        .map(|pair| pair.map(|(worktree, path)| literal(&format!("{worktree}/{path}"))))
        .collect::<Result<_, _>>()
        .map_err(unavailable)?;
    assert!(readonly_include.len() as i64 <= AGENTS_GUARDED_MAX);
    Ok(WorkspaceSettings {
        readonly_include,
        readonly_exclude,
    })
}

/// Unlocks one file of a running team agent's worktree until the agent's current turn
/// ends, which moves the agent's turn count, and forgets the files unlocked in its earlier
/// turns. Inputs: the engine database, the team agent id and the path inside its worktree.
/// Output: Ok, or `ide_agent_not_running`, `ide_unlocked_full` past
/// `UNLOCKED_PER_AGENT_MAX` files, or `changes_path_invalid`.
pub fn unlock(db: &Connection, agent: i64, path: &str) -> Result<(), EngineError> {
    crate::landing::owner_edit::check_path(path)?;
    db.execute_batch(SCHEMA).map_err(unavailable)?;
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)
        .map_err(unavailable)?;
    let turns: i64 = tx
        .query_row(
            &format!("SELECT a.turns FROM team_agents a WHERE a.id=?1 AND {RUNNING}"),
            [agent],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?
        .ok_or_else(|| failure("ide_agent_not_running"))?;
    tx.execute(
        "DELETE FROM ide_unlocked WHERE agent=?1 AND turns<>?2",
        params![agent, turns],
    )
    .map_err(unavailable)?;
    let kept: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM ide_unlocked WHERE agent=?1 AND path<>?2",
            params![agent, path],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    assert!(kept <= UNLOCKED_PER_AGENT_MAX);
    if kept == UNLOCKED_PER_AGENT_MAX {
        return Err(failure("ide_unlocked_full"));
    }
    tx.execute(
        "INSERT INTO ide_unlocked(agent, path, turns) VALUES (?1,?2,?3)
         ON CONFLICT(agent, path) DO UPDATE SET turns=excluded.turns",
        params![agent, path, turns],
    )
    .map_err(unavailable)?;
    tx.commit().map_err(unavailable)
}

/// Writes a path as a glob that matches only itself: each of `*`, `?`, `[`, `{` and `}`
/// becomes a one-character class such as `[*]`. A lone `]` already matches itself.
/// Input: the path. Output: the glob.
fn literal(path: &str) -> String {
    let mut glob = String::with_capacity(path.len());
    for c in path.chars() {
        if matches!(c, '*' | '?' | '[' | '{' | '}') {
            glob.push('[');
            glob.push(c);
            glob.push(']');
        } else {
            glob.push(c);
        }
    }
    glob
}

fn unavailable<E>(_: E) -> EngineError {
    failure("ide_workspace_unavailable")
}

#[cfg(all(test, unix))]
mod tests;
