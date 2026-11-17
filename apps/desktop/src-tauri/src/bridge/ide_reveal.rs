use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{IDE_CODES, OwnerClient};
use crazyagent::ide::bridge::reveal::{RevealFocus, RevealTarget};
use crazyagent::protocol::ProjectId;
use std::sync::Arc;

pub(super) fn ide_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        IDE_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("ide_outcome_unknown"),
        &error.0,
    )
}

impl NativeBridge {
    pub(super) fn reveal_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(ide_error)
    }

    /// Takes, once, the project whose IDE tab a TUI "open in IDE" asked to bring forward,
    /// run without the bridge lock. Output: the project and the reveal, or None when no
    /// reveal is waiting.
    pub fn ide_focus(&self) -> Result<Unlocked<Option<RevealFocus>>, BridgeError> {
        let client = self.reveal_client()?;
        Ok(Box::pin(async move {
            client.ide_focus().await.map_err(ide_error)
        }))
    }

    /// Shows one file of a project in that project's open IDE, as the TUI's open in IDE does,
    /// run without the bridge lock. The path goes to the engine as given: the engine alone
    /// checks that it names a file inside the project's folder. Inputs: the project id and
    /// the path relative to the project. Output: done once the IDE showed it, or the refusal.
    pub fn ide_reveal(
        &self,
        project_id: String,
        path: String,
    ) -> Result<Unlocked<()>, BridgeError> {
        let project = ProjectId::new(project_id).map_err(|_| failure("ide_request_invalid"))?;
        let client = self.reveal_client()?;
        Ok(Box::pin(async move {
            let request = client
                .ide_reveal(project, RevealTarget::Project { path })
                .await
                .map_err(ide_error)?;
            assert!(request.as_str().starts_with("reveal-"));
            Ok(())
        }))
    }
}
