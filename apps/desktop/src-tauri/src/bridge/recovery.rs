use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{OwnerClient, RECOVERY_CODES, RecoveryBanner};
use crazyagent::ledger::ActionKind;
use crazyagent::protocol::ProjectId;
use std::sync::Arc;

pub(super) fn recovery_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        RECOVERY_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("recovery_outcome_unknown"),
        &error.0,
    )
}

impl NativeBridge {
    fn recovery_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(recovery_error)
    }

    /// Reads one project's interrupted work for its recovery banner, run without the bridge
    /// lock. Input: the project id. Output: its runs and the notices waiting for the owner.
    pub fn recovery_banner(
        &self,
        project_id: String,
    ) -> Result<Unlocked<RecoveryBanner>, BridgeError> {
        let project =
            ProjectId::new(project_id).map_err(|_| failure("recovery_request_invalid"))?;
        let client = self.recovery_client()?;
        Ok(Box::pin(async move {
            client
                .recovery_banner(project)
                .await
                .map_err(recovery_error)
        }))
    }

    /// Starts an agent's failed resume again, run without the bridge lock. Input: the team
    /// agent. Output: done once it waits to resume on the team runtime's next tick.
    pub fn retry_recovery(&self, agent: i64) -> Result<Unlocked<()>, BridgeError> {
        if agent <= 0 {
            return Err(failure("recovery_request_invalid"));
        }
        let client = self.recovery_client()?;
        Ok(Box::pin(async move {
            client.recovery_retry(agent).await.map_err(recovery_error)
        }))
    }

    /// Dismisses one recovery notice the owner was shown, run without the bridge lock.
    /// Inputs: the notice's kind and number from the banner. Output: done once dismissed.
    pub fn dismiss_recovery(
        &self,
        action_kind: ActionKind,
        id: i64,
    ) -> Result<Unlocked<()>, BridgeError> {
        if id <= 0 {
            return Err(failure("recovery_request_invalid"));
        }
        let client = self.recovery_client()?;
        Ok(Box::pin(async move {
            client
                .recovery_dismiss(action_kind, id)
                .await
                .map_err(recovery_error)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Recovery code the engine answers reaches the window as itself, so the owner
    /// reads its sentence; anything else is an unknown outcome with the engine code behind.
    #[test]
    fn engine_recovery_codes_pass_through_unchanged() {
        for code in RECOVERY_CODES {
            assert_eq!(recovery_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            recovery_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = recovery_error(EngineError("recovery_state_torn".into()));
        assert_eq!(unlisted.code, "recovery_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("recovery_state_torn"));
    }
}
