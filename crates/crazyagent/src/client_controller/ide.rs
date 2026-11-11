use super::*;
use crate::ide::bridge::reveal::RevealTarget;
use crate::protocol::ProjectId;

impl OwnerController {
    /// Shows a file or folder in the project's open desktop IDE and waits until the IDE
    /// shows it. Inputs: the project and the target. Output: the reveal's id, or an
    /// `IDE_CODES` refusal such as `ide_not_running`.
    pub async fn reveal_in_ide(
        &self,
        project: ProjectId,
        target: RevealTarget,
    ) -> Result<RequestId, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))?
            .ide_reveal(project, target)
            .await
    }
}
