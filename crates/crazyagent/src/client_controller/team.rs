use super::*;
use crate::engine::owner::TerminalSession;
use crate::protocol::ProjectId;
use crate::team::{TeamAction, TeamGraph, TeamStatus};
use crate::terminal::AgentId;

impl OwnerController {
    /// Reads a project's team: harness plan, agents, tasks and the master conversation.
    /// Takes the project id.
    /// Returns the bounded team status, or the engine's plain rejection code.
    pub async fn team_status(&mut self, project_id: ProjectId) -> Result<TeamStatus, EngineError> {
        match self.read(OwnerRead::TeamStatus { project_id }).await?.data {
            OwnerData::TeamStatus { status } => Ok(*status),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Reads a project's team as a graph: the master, harness groups, agents and task edges.
    /// Takes the project id.
    /// Returns the graph, or the engine's plain rejection code.
    pub async fn team_graph(&mut self, project_id: ProjectId) -> Result<TeamGraph, EngineError> {
        match self.read(OwnerRead::TeamGraph { project_id }).await?.data {
            OwnerData::TeamGraph { graph } => Ok(*graph),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Reads what the app shows across projects: the app wake rule and every project's
    /// newest owner notifications with the unseen count for the badge.
    /// Returns the view, or the engine's plain rejection code.
    pub async fn team_events(&mut self) -> Result<crate::team::events::AppEvents, EngineError> {
        match self.read(OwnerRead::TeamEvents).await?.data {
            OwnerData::TeamEvents { events } => Ok(*events),
            _ => Err(EngineError("controller_read_response".into())),
        }
    }

    /// Opens a live view of one agent's terminal, such as a graph node's `terminal`.
    /// Takes the terminal name.
    /// Returns the session with the screen, live output and controls, or a plain code.
    pub async fn terminal_attach(
        &mut self,
        agent: AgentId,
    ) -> Result<TerminalSession, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?
            .terminal_attach(agent)
            .await
    }

    /// Sends one owner team action: a message to the master, a task handed straight
    /// to a sub-agent, a cancel or the keep-warm switch.
    /// Takes the action and the client generation.
    /// Returns the actual owner response; a refused action carries a plain code.
    pub async fn team_action(
        &mut self,
        action: TeamAction,
        generation: u64,
    ) -> Result<OwnerResponse, EngineError> {
        action.validate()?;
        if let Some(project) = action.project() {
            self.team_status(project.clone()).await?;
        }
        self.begin_mutation(OwnerMutation::Team { action }, generation)?;
        self.send_pending().await
    }
}
