use crate::engine::EngineError;
use crate::engine::owner::{OwnerRuntimeHandle, failure};
use crate::project_setup::StoredSetup;
use crate::protocol::ProjectId;
use crate::team::{AgentRole, AgentStatus, TeamAgent};
use crate::terminal::Host;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const BUSY_WAIT: Duration = Duration::from_secs(5);

/// One team agent as the IDE sidebar lists it: its id, name, role, harness, status with its
/// words, the absolute path of its worktree when it has one, and whether the owner can
/// message it now (a running sub-agent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdeAgent {
    pub id: i64,
    pub label: String,
    pub role: AgentRole,
    pub harness: String,
    pub status: AgentStatus,
    pub status_label: String,
    pub worktree: Option<String>,
    pub message: bool,
}

/// Answers `ide.agents`: the project's team agents exactly as the team runtime reports them,
/// newest last, with each worktree resolved inside the project folder.
/// Takes the engine connection and the project setup. Returns the agents.
pub fn agents(connection: &Connection, stored: &StoredSetup) -> Result<Vec<IdeAgent>, EngineError> {
    let folder = stored.project_folder();
    Ok(crate::team::status(connection, stored)?
        .agents
        .into_iter()
        .map(|agent| agent_view(&folder, agent))
        .collect())
}

/// Answers `ide.agent_message`: types the owner's text into one of the project's running
/// sub-agents as one submission, as the team view's send does, and notes it for the
/// master's next turn and as the owner's in the agent's Messages timeline. Takes the
/// connection, the terminal host, the project, the agent id, the text and the time. Returns
/// the agent id, or the team's plain refusal code for blank or long text, an unknown, stopped
/// or other project's agent, a master, or an unreachable terminal.
pub fn message(
    connection: &Connection,
    host: &dyn Host,
    project: &ProjectId,
    agent: i64,
    text: &str,
    now_ms: i64,
) -> Result<Result<i64, &'static str>, EngineError> {
    let sent = crate::team::events::send_to_id(connection, host, project, agent, text)?;
    if sent.is_ok() {
        crate::team::direct::record(connection, agent, text, now_ms)?;
    }
    Ok(sent.map(|()| agent))
}

/// Answers `ide.agents` from the engine database, read off the async runtime. Takes the
/// owner runtime and the project. Returns the agents as JSON, or `project_setup_missing`
/// or `ide_agents_unavailable`.
pub(crate) async fn listed(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
) -> Result<serde_json::Value, EngineError> {
    let database = database(runtime)?;
    let project = project.clone();
    let shown = tokio::task::spawn_blocking(move || {
        let mut connection = open(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let read = connection
            .transaction()
            .map_err(|_| failure("ide_agents_unavailable"))?;
        let stored = crate::project_setup::load_setup(&read, &project)?
            .ok_or_else(|| failure("project_setup_missing"))?;
        agents(&read, &stored)
    })
    .await
    .map_err(|_| failure("ide_agents_unavailable"))??;
    serde_json::to_value(shown).map_err(|_| failure("ide_reply_invalid"))
}

/// Answers `ide.agent_message` on the engine's terminal host, off the async runtime.
/// Takes the owner runtime, the project, the agent id and the text. Returns `{agent}`, or
/// the team's refusal code.
pub(crate) async fn sent(
    runtime: &OwnerRuntimeHandle,
    project: &ProjectId,
    agent: i64,
    text: String,
) -> Result<serde_json::Value, EngineError> {
    let database = database(runtime)?;
    let host = runtime.terminal_off_worker().await?;
    let project = project.clone();
    let sent = tokio::task::spawn_blocking(move || {
        let connection = open(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        message(
            &connection,
            host.as_ref(),
            &project,
            agent,
            &text,
            crate::team::now_ms(),
        )
    })
    .await
    .map_err(|_| failure("ide_agents_unavailable"))??;
    let agent = sent.map_err(failure)?;
    Ok(serde_json::json!({ "agent": agent }))
}

pub(super) fn database(runtime: &OwnerRuntimeHandle) -> Result<PathBuf, EngineError> {
    Ok(runtime.ide()?.root.join("engine.db"))
}

pub(super) fn open(database: &Path, access: OpenFlags) -> Result<Connection, EngineError> {
    let connection = Connection::open_with_flags(
        database,
        access | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| failure("ide_agents_unavailable"))?;
    connection
        .busy_timeout(BUSY_WAIT)
        .map_err(|_| failure("ide_agents_unavailable"))?;
    Ok(connection)
}

fn agent_view(folder: &Path, agent: TeamAgent) -> IdeAgent {
    let worktree = agent
        .worktree
        .as_deref()
        .map(Path::new)
        .filter(|relative| {
            relative.components().next().is_some()
                && relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
        })
        .and_then(|relative| folder.join(relative).to_str().map(str::to_owned));
    IdeAgent {
        id: agent.id,
        message: agent.role == AgentRole::Sub && agent.status.live(),
        label: agent.label,
        role: agent.role,
        harness: agent.harness,
        status: agent.status,
        status_label: agent.status_label,
        worktree,
    }
}

#[cfg(all(test, unix))]
mod tests;
