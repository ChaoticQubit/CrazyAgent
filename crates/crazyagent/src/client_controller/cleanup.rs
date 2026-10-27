use super::*;
use crate::protocol::ProjectId;
use crate::team::cleanup::{FinishedAgent, Removal};

impl OwnerController {
    fn cleanup_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads a project's finished agents whose worktree and agent folder can be removed,
    /// newest first. Input: the project. Output: the agents with their size on disk, or a
    /// `CLEANUP_CODES` refusal.
    pub async fn finished_agents(
        &self,
        project: ProjectId,
    ) -> Result<Vec<FinishedAgent>, EngineError> {
        self.cleanup_client()?.finished_agents(project).await
    }

    /// Removes the worktrees and agent folders of the finished agents the owner confirmed;
    /// the engine checks each one again first. Inputs: the project and the agents.
    /// Output: the agents removed with the code of the first one kept, or the refusal when
    /// it removed none.
    pub async fn remove_finished_agents(
        &self,
        project: ProjectId,
        agents: Vec<i64>,
    ) -> Result<Removal, EngineError> {
        self.cleanup_client()?
            .remove_finished_agents(project, agents)
            .await
    }
}
