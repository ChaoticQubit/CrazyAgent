use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{IDE_TAB_CODES, IdeTabServer, IdeTabState, OwnerClient};
use crazyagent::protocol::ProjectId;
use serde::Serialize;
use std::sync::Arc;

/// What the project page shows for the IDE: where the engine's IDE stands and the platform
/// this desktop runs on, which the engine shares, so an unavailable IDE names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdeView {
    pub state: IdeTabState,
    pub platform: &'static str,
}

pub(super) fn ide_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    failure(
        IDE_TAB_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("ide_tab_outcome_unknown"),
    )
}

/// Checks a project id the window sent. Input: the id. Output: the project, or
/// `ide_project_invalid`.
pub fn ide_project(project_id: &str) -> Result<ProjectId, BridgeError> {
    ProjectId::new(project_id).map_err(|_| failure("ide_project_invalid"))
}

impl NativeBridge {
    fn ide_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(ide_error)
    }

    /// Reads what the project page shows for the IDE, run without the bridge lock: the build
    /// state, after starting the fetch again when `retry`, and this platform. Input: whether
    /// to retry. Output: the view.
    pub fn ide_state(&self, retry: bool) -> Result<Unlocked<IdeView>, BridgeError> {
        let client = self.ide_client()?;
        Ok(Box::pin(async move {
            let state = client.ide_tab_state(retry).await.map_err(ide_error)?;
            Ok(IdeView {
                state,
                platform: std::env::consts::OS,
            })
        }))
    }

    /// Opens a project's IDE server, run without the bridge lock. Input: the project.
    /// Output: where its IDE is reached.
    pub fn ide_open(&self, project: ProjectId) -> Result<Unlocked<IdeTabServer>, BridgeError> {
        let client = self.ide_client()?;
        Ok(Box::pin(async move {
            client.ide_tab_open(project).await.map_err(ide_error)
        }))
    }

    /// Keeps a project's IDE server from stopping for idleness, run without the bridge lock.
    /// Input: the project. Output: whether its server still runs.
    pub fn ide_touch(&self, project: ProjectId) -> Result<Unlocked<bool>, BridgeError> {
        let client = self.ide_client()?;
        Ok(Box::pin(async move {
            client.ide_tab_touch(project).await.map_err(ide_error)
        }))
    }

    /// Stops a project's IDE server after its project page closed, run without the bridge
    /// lock. Input: the project. Output: whether a server was running.
    pub fn ide_stop(&self, project: ProjectId) -> Result<Unlocked<bool>, BridgeError> {
        let client = self.ide_client()?;
        Ok(Box::pin(async move {
            client.ide_tab_stop(project).await.map_err(ide_error)
        }))
    }
}
