use super::*;
use crate::engine::owner::{ProcessText, SavedText};
use crate::protocol::ProjectId;

impl OwnerController {
    fn process_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads the owner's instructions. Input: none for the app-wide text, or the project.
    /// Output: the text, empty when none is saved, and its version, or a `PROCESS_CODES`
    /// refusal.
    pub async fn process_text(
        &self,
        project: Option<ProjectId>,
    ) -> Result<ProcessText, EngineError> {
        self.process_client()?.process_text(project).await
    }

    /// Saves the owner's instructions whole when no other save landed since they were read.
    /// Inputs: none for the app-wide text, or the project; the text; the version read; and,
    /// only for "Keep mine" after a stale reply, the older version the owner opened.
    /// Output: the saved version, the stale reply with the newer text, or a refusal such as
    /// `process_text_too_large`.
    pub async fn save_process_text(
        &self,
        project: Option<ProjectId>,
        text: String,
        version: u64,
        opened: Option<u64>,
    ) -> Result<SavedText, EngineError> {
        self.process_client()?
            .save_process_text(project, text, version, opened)
            .await
    }

    /// Reads the owner's own instructions for one team agent. Inputs: the project and the
    /// agent id. Output: the text, empty when none is saved, and its version.
    pub async fn agent_text(
        &self,
        project: ProjectId,
        agent: i64,
    ) -> Result<ProcessText, EngineError> {
        self.process_client()?.agent_text(project, agent).await
    }

    /// Saves the owner's own instructions for one team agent when no other save landed since
    /// they were read. Inputs: the project, the agent id, the text, the version read and,
    /// only for "Keep mine" after a stale reply, the older version the owner opened.
    /// Output: the saved version, the stale reply with the newer text, or a refusal.
    pub async fn save_agent_text(
        &self,
        project: ProjectId,
        agent: i64,
        text: String,
        version: u64,
        opened: Option<u64>,
    ) -> Result<SavedText, EngineError> {
        self.process_client()?
            .save_agent_text(project, agent, text, version, opened)
            .await
    }

    /// Reads the owner's `~/.claude/CLAUDE.md` for a preview, only on this explicit call.
    /// Output: the text, or `process_import_missing`.
    pub async fn import_global_instructions(&self) -> Result<String, EngineError> {
        self.process_client()?.import_global_instructions().await
    }

    /// Reads the CrazyAgent default process for a preview.
    pub async fn default_process(&self) -> Result<String, EngineError> {
        self.process_client()?.default_process().await
    }

    /// Reads what one team agent received at its last launch. Inputs: the project and the
    /// agent id, or none for the project's newest master. Output: the read-only view.
    pub async fn agent_instructions(
        &self,
        project: ProjectId,
        agent: Option<i64>,
    ) -> Result<crate::engine::owner::AgentInstructions, EngineError> {
        self.process_client()?
            .agent_instructions(project, agent)
            .await
    }
}
