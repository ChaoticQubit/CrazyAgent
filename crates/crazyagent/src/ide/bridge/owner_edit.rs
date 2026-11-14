use crate::engine::EngineError;
use crate::engine::owner::{OwnerRuntimeHandle, failure};
use crate::protocol::ProjectId;
use crate::terminal::{AgentId, Host};
use rusqlite::{OptionalExtension, params};
use std::path::{Path, PathBuf};

/// Answers `ide.owner_edit` on a blocking thread: records the owner's edit, unlocks the
/// file and tells the agent. Inputs: the owner runtime, project, agent and path inside its
/// worktree. Output: `{path, told}`, `told` saying whether the agent's terminal took the
/// message.
pub(super) async fn answer(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    agent: AgentId,
    path: String,
) -> Result<serde_json::Value, EngineError> {
    let root = runtime.ide()?.root;
    let host = runtime.terminal_off_worker().await.ok();
    let project = project.clone();
    let (path, told) = tokio::task::spawn_blocking(move || -> Result<_, EngineError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
            .ok_or_else(|| failure("ide_clock_unavailable"))?;
        let told = edit(&root, host.as_deref(), &project, &agent, &path, now)?;
        Ok((path, told))
    })
    .await
    .map_err(|_| failure("ide_owner_edit_unavailable"))??;
    Ok(serde_json::json!({ "path": path, "told": told }))
}

/// Lets the owner edit one file in a running team agent's worktree: records the edit for
/// the change set that will hold it, unlocks the file until the agent's turn ends, rewrites
/// the workspace and types one message naming the file on one line into a sub-agent's
/// terminal, kept in its Messages timeline as an engine notice, so a path with a line break
/// cannot add a line to that notice. A master hears nothing; the owner talks to it
/// directly. Inputs: the engine root, the terminal host when there is one, the project, the
/// agent, the path inside its worktree and the time in ms. Output: whether the agent was
/// told, or `ide_agent_unknown`, `ide_agent_not_running`, `ide_agent_files_read_only` while
/// the project's agent folders are Read only below Full access (DEC-124), since the IDE could
/// not save the file, or the store's refusal.
pub(crate) fn edit(
    root: &Path,
    host: Option<&dyn Host>,
    project: &ProjectId,
    agent: &AgentId,
    path: &str,
    now: u64,
) -> Result<bool, EngineError> {
    let id = agent
        .team_agent()
        .ok_or_else(|| failure("ide_agent_unknown"))?;
    crate::landing::owner_edit::check_path(path)?;
    let db = crate::landing::open(root)?;
    let found: Option<(String, String, PathBuf)> = db
        .query_row(
            "SELECT a.role, a.status, w.worktree FROM team_agents a
             JOIN team_worktrees w ON w.agent=a.id AND w.project=a.project
             WHERE a.id=?1 AND a.project=?2",
            params![id, project.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    PathBuf::from(row.get::<_, String>(2)?),
                ))
            },
        )
        .optional()
        .map_err(|_| failure("ide_owner_edit_unavailable"))?;
    let (role, status, worktree) = found.ok_or_else(|| failure("ide_agent_unknown"))?;
    if status != "starting" && status != "working" {
        return Err(failure("ide_agent_not_running"));
    }
    let policy = crate::sandbox::policy::load(&db, project.as_str())
        .map_err(|_| failure("ide_owner_edit_unavailable"))?;
    if policy.access != crate::sandbox::policy::AgentAccess::Full
        && policy.ide_agent_files == crate::sandbox::policy::IdeAgentFiles::ReadOnly
    {
        return Err(failure("ide_agent_files_read_only"));
    }
    crate::landing::owner_edit::record(&db, &worktree, path, now)?;
    super::super::readonly::unlock(&db, id, path)?;
    super::super::workspace::write(root, project)?;
    let Some(host) = host.filter(|_| role == "sub") else {
        return Ok(false);
    };
    let named = super::shown(path, super::NOTE_PATH_BYTES_MAX);
    assert!(!named.contains('\n'), "the note names the file on one line");
    let message = format!(
        "The owner edited {named} in your worktree while you work. Keep their edit; it is part of your change set."
    );
    let sent = crate::team::events::send_to_agent(
        &db,
        host,
        project,
        &id.to_string(),
        crate::team::events::Said::Notice,
        &message,
    )?;
    Ok(sent.is_ok())
}

#[cfg(all(test, unix))]
mod tests;
