use super::*;
use crate::engine::owner::RecoveryBanner;
use crate::ledger::ActionKind;
use crate::protocol::ProjectId;

impl OwnerController {
    fn recovery_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads one project's interrupted work for its recovery banner. Input: the project.
    /// Output: its runs and notices, or a `RECOVERY_CODES` refusal.
    pub async fn recovery_banner(&self, project: ProjectId) -> Result<RecoveryBanner, EngineError> {
        self.recovery_client()?.recovery_banner(project).await
    }

    /// Starts an agent's failed resume again. Input: the team agent. Output: Ok once it
    /// waits to resume, or a `RECOVERY_CODES` refusal.
    pub async fn recovery_retry(&self, agent: i64) -> Result<(), EngineError> {
        self.recovery_client()?.recovery_retry(agent).await
    }

    /// Dismisses one recovery notice the owner was shown. Inputs: its kind and number.
    /// Output: Ok once dismissed, or a `RECOVERY_CODES` refusal.
    pub async fn recovery_dismiss(&self, kind: ActionKind, id: i64) -> Result<(), EngineError> {
        self.recovery_client()?.recovery_dismiss(kind, id).await
    }
}
