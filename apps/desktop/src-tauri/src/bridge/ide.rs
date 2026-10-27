use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{IDE_TAB_CODES, IdeTabServer, IdeTabState, OwnerClient};
use crazyagent::protocol::ProjectId;
use serde::Serialize;
use std::sync::Arc;

/// The switch that shows the IDE tab until IDE-15 makes it the default: `1` in the desktop
/// app's environment turns it on.
pub const IDE_TAB_FLAG: &str = "CRAZYAGENT_IDE_TAB";

/// What the IDE tab shows: nothing while the `ide_tab` flag is off, else where the engine's
/// IDE stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "tab", rename_all = "snake_case")]
pub enum IdeView {
    Off,
    On { state: IdeTabState },
}

fn ide_tab_on() -> bool {
    std::env::var_os(IDE_TAB_FLAG).is_some_and(|value| value == "1")
}

fn ide_error(error: EngineError) -> BridgeError {
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

    /// Reads what the IDE tab shows, run without the bridge lock: Off while the flag is off,
    /// without asking the engine, else the build state, after starting the fetch again when
    /// `retry`. Input: whether to retry. Output: the view.
    pub fn ide_state(&self, retry: bool) -> Result<Unlocked<IdeView>, BridgeError> {
        if !ide_tab_on() {
            return Ok(Box::pin(async { Ok(IdeView::Off) }));
        }
        let client = self.ide_client()?;
        Ok(Box::pin(async move {
            let state = client.ide_tab_state(retry).await.map_err(ide_error)?;
            Ok(IdeView::On { state })
        }))
    }

    /// Opens a project's IDE server, run without the bridge lock. Input: the project.
    /// Output: where its IDE is reached; `ide_tab_off` while the flag is off.
    pub fn ide_open(&self, project: ProjectId) -> Result<Unlocked<IdeTabServer>, BridgeError> {
        if !ide_tab_on() {
            return Err(failure("ide_tab_off"));
        }
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
}
