use super::*;
use crate::protocol::ProjectId;
use crate::roadmap_status::{Refresh, RoadmapStatus};

impl OwnerController {
    fn roadmap_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads a project's roadmap status: every item with its state, when it was checked and
    /// the refresh setting. Input: the project. Output: the status, or a `ROADMAP_CODES`
    /// refusal.
    pub async fn roadmap_status(&self, project: ProjectId) -> Result<RoadmapStatus, EngineError> {
        self.roadmap_client()?.roadmap_status(project).await
    }

    /// Asks for a project's roadmap status to be checked now. Input: the project. Output:
    /// unit once the check is due, or a `ROADMAP_CODES` refusal, `roadmap_status_off` under
    /// the Off setting.
    pub async fn roadmap_refresh(&self, project: ProjectId) -> Result<(), EngineError> {
        self.roadmap_client()?.roadmap_refresh(project).await
    }

    /// Saves when a project's roadmap status is checked. Inputs: the project and the choice.
    /// Output: unit once saved, or a `ROADMAP_CODES` refusal.
    pub async fn roadmap_set_refresh(
        &self,
        project: ProjectId,
        refresh: Refresh,
    ) -> Result<(), EngineError> {
        self.roadmap_client()?
            .roadmap_set_refresh(project, refresh)
            .await
    }
}
