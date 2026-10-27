use super::*;
use crate::protocol::ProjectId;

impl OwnerController {
    fn process_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads the owner's instructions. Input: none for the app-wide text, or the project.
    /// Output: the text, empty when none is saved, or a `PROCESS_CODES` refusal.
    pub async fn process_text(&self, project: Option<ProjectId>) -> Result<String, EngineError> {
        self.process_client()?.process_text(project).await
    }

    /// Saves the owner's instructions whole. Inputs: none for the app-wide text, or the
    /// project, and the text. Output: unit, or a refusal such as `process_text_too_large`.
    pub async fn save_process_text(
        &self,
        project: Option<ProjectId>,
        text: String,
    ) -> Result<(), EngineError> {
        self.process_client()?
            .save_process_text(project, text)
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
}
