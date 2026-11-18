use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{IDE_EXTENSIONS_CODES, OwnerClient};
use crazyagent::ide::extensions::{Approved, AskView};
use crazyagent::protocol::ProjectId;
use std::sync::Arc;

pub(super) fn extensions_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        IDE_EXTENSIONS_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("ide_extensions_outcome_unknown"),
        &error.0,
    )
}

fn project(project_id: String) -> Result<ProjectId, BridgeError> {
    ProjectId::new(project_id).map_err(|_| failure("ide_extensions_request_invalid"))
}

impl NativeBridge {
    fn extensions_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(extensions_error)
    }

    /// Reads the extension installs and updates a project's IDE asked the owner to
    /// confirm, run without the bridge lock. Input: the project id. Output: each ask,
    /// oldest first, with the version approved now for an update.
    pub fn ide_extension_asks(
        &self,
        project_id: String,
    ) -> Result<Unlocked<Vec<AskView>>, BridgeError> {
        let project = project(project_id)?;
        let client = self.extensions_client()?;
        Ok(Box::pin(async move {
            client
                .ide_extension_asks(project)
                .await
                .map_err(extensions_error)
        }))
    }

    /// The owner's approval of one ask, run without the bridge lock: the engine
    /// downloads the VSIX from Open VSX, checks its sha256 and pins that version.
    /// Inputs: the project id, the extension id and the version as asked. Output: the pin.
    pub fn ide_extension_approve(
        &self,
        project_id: String,
        id: String,
        version: String,
    ) -> Result<Unlocked<Approved>, BridgeError> {
        let project = project(project_id)?;
        let client = self.extensions_client()?;
        Ok(Box::pin(async move {
            client
                .ide_extension_approve(project, id, version)
                .await
                .map_err(extensions_error)
        }))
    }

    /// The owner's Not now on one ask, run without the bridge lock. Inputs: the project
    /// id, the extension id and the version as asked. Output: done once it is dropped.
    pub fn ide_extension_decline(
        &self,
        project_id: String,
        id: String,
        version: String,
    ) -> Result<Unlocked<()>, BridgeError> {
        let project = project(project_id)?;
        let client = self.extensions_client()?;
        Ok(Box::pin(async move {
            client
                .ide_extension_decline(project, id, version)
                .await
                .map_err(extensions_error)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every extension code the engine answers reaches the window as itself; anything
    /// else is an unknown outcome with the engine code behind it and raw text withheld.
    #[test]
    fn engine_extension_codes_pass_through_unchanged() {
        for code in IDE_EXTENSIONS_CODES {
            assert_eq!(extensions_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            extensions_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = extensions_error(EngineError("ide_extensions_torn".into()));
        assert_eq!(unlisted.code, "ide_extensions_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("ide_extensions_torn"));
        let raw = extensions_error(EngineError("open /Users/mira/vsix: denied".into()));
        assert_eq!(raw.code, "ide_extensions_outcome_unknown");
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
        assert_eq!(
            project("../escape".into()).err().unwrap().code,
            "ide_extensions_request_invalid"
        );
    }
}
