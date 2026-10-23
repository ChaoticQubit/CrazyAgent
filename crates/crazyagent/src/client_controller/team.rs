use super::*;
use crate::protocol::ProjectId;
use crate::team::{TeamAction, TeamStatus};

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
        self.team_status(action.project().clone()).await?;
        self.begin_mutation(OwnerMutation::Team { action }, generation)?;
        self.send_pending().await
    }
}
