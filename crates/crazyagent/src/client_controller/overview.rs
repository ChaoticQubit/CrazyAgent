use super::*;
use crate::project_overview::{Apply, Overview, Tab};
use crate::protocol::ProjectId;
use crate::roadmap_status::Refresh;

impl OwnerController {
    fn overview_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads a project's Overview: the README, the four tabs with their text, version and
    /// proposal, the competitor state, the run state and the settings. Input: the project.
    /// Output: the Overview, or an `OVERVIEW_CODES` refusal.
    pub async fn overview_read(&self, project: ProjectId) -> Result<Overview, EngineError> {
        self.overview_client()?.overview_read(project).await
    }

    /// Asks for a writing run of the project's tabs at once. Input: the project. Output:
    /// unit, or the refusal (`overview_off` when the tabs are off).
    pub async fn overview_refresh(&self, project: ProjectId) -> Result<(), EngineError> {
        self.overview_client()?.overview_refresh(project).await
    }

    /// Saves the owner's edit of a tab at the version the owner saw. Inputs: the project,
    /// the tab, the text and the version. Output: unit, or the refusal
    /// (`overview_tab_changed` at another version).
    pub async fn overview_save(
        &self,
        project: ProjectId,
        tab: Tab,
        text: String,
        version: String,
    ) -> Result<(), EngineError> {
        self.overview_client()?
            .overview_save(project, tab, text, version)
            .await
    }

    /// Writes a tab's proposal as its file at the version its diff was made from. Inputs:
    /// the project, the tab and the version. Output: unit, or the refusal.
    pub async fn overview_accept(
        &self,
        project: ProjectId,
        tab: Tab,
        version: String,
    ) -> Result<(), EngineError> {
        self.overview_client()?
            .overview_accept(project, tab, version)
            .await
    }

    /// Drops a tab's proposal; the file is untouched. Inputs: the project and the tab.
    /// Output: unit, or the refusal.
    pub async fn overview_reject(&self, project: ProjectId, tab: Tab) -> Result<(), EngineError> {
        self.overview_client()?.overview_reject(project, tab).await
    }

    /// Stores when the project's tabs are refreshed. Inputs: the project and the choice.
    /// Output: unit, or the refusal.
    pub async fn overview_set_refresh(
        &self,
        project: ProjectId,
        refresh: Refresh,
    ) -> Result<(), EngineError> {
        self.overview_client()?
            .overview_set_refresh(project, refresh)
            .await
    }

    /// Stores what happens to a refreshed tab. Inputs: the project and the choice. Output:
    /// unit, or the refusal.
    pub async fn overview_set_apply(
        &self,
        project: ProjectId,
        apply: Apply,
    ) -> Result<(), EngineError> {
        self.overview_client()?
            .overview_set_apply(project, apply)
            .await
    }
}
