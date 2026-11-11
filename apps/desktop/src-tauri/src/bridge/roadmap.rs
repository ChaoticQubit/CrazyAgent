use super::*;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{OwnerClient, ROADMAP_CODES};
use crazyagent::protocol::ProjectId;
use crazyagent::roadmap_status::{Refresh, RoadmapStatus};
use std::sync::Arc;

/// The bridge failure for an engine roadmap status error: a known code reaches the window
/// as itself, anything else is an unknown outcome with the engine code behind it.
/// Input: the engine error. Output: the bridge failure.
pub fn roadmap_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        ROADMAP_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("roadmap_status_outcome_unknown"),
        &error.0,
    )
}

/// Reads the refresh choice the window sent by its stored name. Input: the name, any text.
/// Output: the choice, or `roadmap_status_invalid` for a name the engine does not know.
pub fn refresh_choice(name: &str) -> Result<Refresh, BridgeError> {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .map_err(|_| failure("roadmap_status_invalid"))
}

fn project(id: String) -> Result<ProjectId, BridgeError> {
    ProjectId::new(id).map_err(|_| failure("roadmap_status_project_missing"))
}

impl NativeBridge {
    fn roadmap_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(roadmap_error)
    }

    /// Reads a project's roadmap status, run without the bridge lock. Input: the project
    /// id. Output: every item with its state, when it was checked and the refresh setting.
    pub fn roadmap_status(
        &self,
        project_id: String,
    ) -> Result<Unlocked<RoadmapStatus>, BridgeError> {
        let project = project(project_id)?;
        let client = self.roadmap_client()?;
        Ok(Box::pin(async move {
            client.roadmap_status(project).await.map_err(roadmap_error)
        }))
    }

    /// Asks the engine to check a project's roadmap status now, run without the bridge
    /// lock. Input: the project id. Output: Ok once the check is due; `roadmap_status_off`
    /// under the Off setting.
    pub fn refresh_roadmap_status(&self, project_id: String) -> Result<Unlocked<()>, BridgeError> {
        let project = project(project_id)?;
        let client = self.roadmap_client()?;
        Ok(Box::pin(async move {
            client.roadmap_refresh(project).await.map_err(roadmap_error)
        }))
    }

    /// Saves when a project's roadmap status is checked, run without the bridge lock.
    /// Inputs: the project id and the choice's name. Output: Ok once saved.
    pub fn set_roadmap_refresh(
        &self,
        project_id: String,
        refresh: String,
    ) -> Result<Unlocked<()>, BridgeError> {
        let project = project(project_id)?;
        let refresh = refresh_choice(&refresh)?;
        let client = self.roadmap_client()?;
        Ok(Box::pin(async move {
            client
                .roadmap_set_refresh(project, refresh)
                .await
                .map_err(roadmap_error)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_refresh_choice_is_read_by_its_name_and_any_other_name_is_refused() {
        for (name, choice) in [
            ("after_landing", Refresh::AfterLanding),
            ("refresh_only", Refresh::RefreshOnly),
            ("off", Refresh::Off),
        ] {
            assert_eq!(refresh_choice(name).unwrap(), choice);
        }
        let long = "off".repeat(4096);
        for name in [
            "",
            "Off",
            "sometimes",
            "after_landing ",
            "\0",
            "off\0",
            "o\0ff",
            long.as_str(),
        ] {
            assert_eq!(
                refresh_choice(name).unwrap_err().code,
                "roadmap_status_invalid",
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_known_roadmap_code_reaches_the_window_and_any_other_is_an_unknown_outcome() {
        let mapped = |code: &str| roadmap_error(EngineError(code.to_owned())).code;
        assert_eq!(mapped("roadmap_status_off"), "roadmap_status_off");
        assert_eq!(
            mapped("roadmap_status_project_missing"),
            "roadmap_status_project_missing"
        );
        assert_eq!(mapped("controller_disconnected"), "not_connected");
        assert_eq!(mapped("sqlite_busy"), "roadmap_status_outcome_unknown");
    }
}
