use super::*;
use crazyagent::connectors::ConnectorSettings;
use crazyagent::connectors::audit::ActivityView;
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{CONNECTORS_CODES, ConnectorsSaved, OwnerClient};
use serde::Serialize;
use std::sync::Arc;

/// How a connector settings save ended, as the window reads it: saved, or stale because
/// the settings changed since the window read them, with the current version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectorSettingsSave {
    Saved,
    Stale { current: u64 },
}

impl From<ConnectorsSaved> for ConnectorSettingsSave {
    fn from(saved: ConnectorsSaved) -> Self {
        match saved {
            ConnectorsSaved::Saved(_) => ConnectorSettingsSave::Saved,
            ConnectorsSaved::Stale(current) => ConnectorSettingsSave::Stale { current },
        }
    }
}

/// The bridge failure for an engine Connectors error: a known code reaches the window as
/// itself, anything else is an unknown outcome with the engine code behind it.
/// Input: the engine error. Output: the bridge failure.
fn connector_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        CONNECTORS_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("connector_outcome_unknown"),
        &error.0,
    )
}

impl NativeBridge {
    fn connectors_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(connector_error)
    }

    /// Reads the connector activity log, run without the bridge lock. Inputs: the connector
    /// and the project to filter by, none for all. Output: the activity view.
    pub fn connector_activity(
        &self,
        connector: Option<String>,
        project: Option<String>,
    ) -> Result<Unlocked<ActivityView>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .connector_activity(connector, project)
                .await
                .map_err(connector_error)
        }))
    }

    /// Saves the connector settings with the version the window read, run without the
    /// bridge lock. Inputs: the settings and the version. Output: saved or stale.
    pub fn connector_settings_set(
        &self,
        settings: ConnectorSettings,
        version: u64,
    ) -> Result<Unlocked<ConnectorSettingsSave>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .set_connector_settings(settings, version)
                .await
                .map(ConnectorSettingsSave::from)
                .map_err(connector_error)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Connectors code the engine answers reaches the window as itself, anything else
    /// is an unknown outcome with raw engine text withheld, and a save reads as saved or
    /// stale with the current version.
    #[test]
    fn connector_codes_pass_through_and_a_save_reads_as_saved_or_stale() {
        for code in CONNECTORS_CODES {
            assert_eq!(connector_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            connector_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let raw = connector_error(EngineError("open /Users/mira/engine.db: denied".into()));
        assert_eq!(raw.code, "connector_outcome_unknown");
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
        assert_eq!(
            serde_json::to_value(ConnectorSettingsSave::from(ConnectorsSaved::Stale(4))).unwrap(),
            serde_json::json!({"kind": "stale", "current": 4})
        );
        assert_eq!(
            serde_json::to_value(ConnectorSettingsSave::Saved).unwrap(),
            serde_json::json!({"kind": "saved"})
        );
    }
}
