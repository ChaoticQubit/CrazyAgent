use super::*;
use crazyagent::app_plugins::Preview;
use crazyagent::connectors::audit::ActivityView;
use crazyagent::connectors::{ConnectorSettings, ConnectorsView, Who};
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

    /// Reads the Connectors page, run without the bridge lock. Input: the project whose
    /// switches to include, if any. Output: the page.
    pub fn connectors(
        &self,
        project: Option<String>,
    ) -> Result<Unlocked<ConnectorsView>, BridgeError> {
        let _ = project;
        Err(failure("connector_outcome_unknown"))
    }

    /// Reads the trust screen of a picked or pasted connector manifest without saving it,
    /// run without the bridge lock. Input: the manifest text. Output: ready or refused.
    pub fn connector_preview(&self, yaml: String) -> Result<Unlocked<Preview>, BridgeError> {
        let _ = yaml;
        Err(failure("connector_outcome_unknown"))
    }

    /// Reads the trust screen of a listed connector that needs review, run without the
    /// bridge lock. Input: the name. Output: ready or refused.
    pub fn connector_review(&self, name: String) -> Result<Unlocked<Preview>, BridgeError> {
        let _ = name;
        Err(failure("connector_outcome_unknown"))
    }

    /// Adds a connector manifest the owner confirmed on its trust screen, run without the
    /// bridge lock. Inputs: the text, the SHA-256 the screen showed and whether it replaces
    /// the owner's connector of that name. Output: the page.
    pub fn connector_add(
        &self,
        yaml: String,
        sha256: String,
        replace: bool,
    ) -> Result<Unlocked<ConnectorsView>, BridgeError> {
        let _ = (yaml, sha256, replace);
        Err(failure("connector_outcome_unknown"))
    }

    /// Trusts a reviewed connector file, run without the bridge lock. Inputs: the name and
    /// the SHA-256 its trust screen showed. Output: the page.
    pub fn connector_trust(
        &self,
        name: String,
        sha256: String,
    ) -> Result<Unlocked<ConnectorsView>, BridgeError> {
        let _ = (name, sha256);
        Err(failure("connector_outcome_unknown"))
    }

    /// Removes one of the owner's connectors, run without the bridge lock. Input: the name.
    /// Output: the page.
    pub fn connector_remove(&self, name: String) -> Result<Unlocked<ConnectorsView>, BridgeError> {
        let _ = name;
        Err(failure("connector_outcome_unknown"))
    }

    /// Saves a key the owner typed for one of a connector's secrets; it goes to the engine
    /// in its own frame and the bridge keeps no copy. Run without the bridge lock. Inputs:
    /// the connector, the secret and the key. Output: the page.
    pub fn connector_set_key(
        &self,
        name: String,
        secret: String,
        key: String,
    ) -> Result<Unlocked<ConnectorsView>, BridgeError> {
        let _ = (name, secret, key);
        Err(failure("connector_outcome_unknown"))
    }

    /// Deletes the key saved for one of a connector's secrets, run without the bridge lock.
    /// Inputs: the connector and the secret. Output: the page.
    pub fn connector_clear_key(
        &self,
        name: String,
        secret: String,
    ) -> Result<Unlocked<ConnectorsView>, BridgeError> {
        let _ = (name, secret);
        Err(failure("connector_outcome_unknown"))
    }

    /// Turns a connector on or off for one project and says who it reaches, run without the
    /// bridge lock. Inputs: the project, the connector, the switch and who. Output: the page
    /// with that project's switches.
    pub fn connector_grant(
        &self,
        project: String,
        connector: String,
        enabled: bool,
        who: Who,
    ) -> Result<Unlocked<ConnectorsView>, BridgeError> {
        let _ = (project, connector, enabled, who);
        Err(failure("connector_outcome_unknown"))
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

    /// The code a bridge call refused with before any engine call. Input: the call's
    /// result. Output: the code.
    fn refused<T>(result: Result<Unlocked<T>, BridgeError>) -> &'static str {
        match result {
            Ok(_) => panic!("a bridge with no engine readied a Connectors call"),
            Err(error) => error.code,
        }
    }

    /// Every Connectors command needs the engine, and a typed key is checked in the bridge
    /// first: an empty one is refused before any engine call, and a typed one on a bridge
    /// with no engine is not_connected with the key in no part of the refusal.
    #[test]
    fn connector_commands_need_the_engine_and_an_empty_key_never_leaves_the_bridge() {
        let bridge = NativeBridge::new(None);
        let text = || "linear".to_string();
        assert_eq!(
            refused(bridge.connector_set_key(text(), "api_key".into(), String::new())),
            "app_plugin_key_invalid"
        );
        let typed =
            match bridge.connector_set_key(text(), "api_key".into(), "lin-e2e-synthetic".into()) {
                Ok(_) => panic!("a bridge with no engine readied a key save"),
                Err(error) => error,
            };
        assert_eq!(typed.code, "not_connected");
        assert!(!format!("{typed:?}").contains("lin-e2e"));
        assert!(!serde_json::to_string(&typed).unwrap().contains("lin-e2e"));
        assert_eq!(
            refused(bridge.connectors(Some("alpha".into()))),
            "not_connected"
        );
        assert_eq!(
            refused(bridge.connector_preview("schema: 1".into())),
            "not_connected"
        );
        assert_eq!(refused(bridge.connector_review(text())), "not_connected");
        assert_eq!(
            refused(bridge.connector_add("schema: 1".into(), "0".repeat(64), false)),
            "not_connected"
        );
        assert_eq!(
            refused(bridge.connector_trust(text(), "0".repeat(64))),
            "not_connected"
        );
        assert_eq!(refused(bridge.connector_remove(text())), "not_connected");
        assert_eq!(
            refused(bridge.connector_clear_key(text(), "api_key".into())),
            "not_connected"
        );
        assert_eq!(
            refused(bridge.connector_grant("alpha".into(), text(), true, Who::Master)),
            "not_connected"
        );
        assert_eq!(
            serde_json::from_value::<Who>(serde_json::json!("both")).unwrap(),
            Who::Both,
            "the window's Who words are the engine's"
        );
    }
}
