use super::*;
use crazyagent::app_plugins::Preview;
use crazyagent::connectors::approvals::{AllowedWrites, ApprovalView};
use crazyagent::connectors::audit::ActivityView;
use crazyagent::connectors::sign_in::SignInState;
use crazyagent::connectors::{ConnectorSettings, ConnectorsView, SignInWith, Who};
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
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client.connectors(project).await.map_err(connector_error)
        }))
    }

    /// Reads the trust screen of a picked or pasted connector manifest without saving it,
    /// run without the bridge lock. Input: the manifest text. Output: ready or refused.
    pub fn connector_preview(&self, yaml: String) -> Result<Unlocked<Preview>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .connector_preview(yaml)
                .await
                .map_err(connector_error)
        }))
    }

    /// Reads the trust screen of a listed connector that needs review, run without the
    /// bridge lock. Input: the name. Output: ready or refused.
    pub fn connector_review(&self, name: String) -> Result<Unlocked<Preview>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client.connector_review(name).await.map_err(connector_error)
        }))
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
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .connector_add(yaml, sha256, replace)
                .await
                .map_err(connector_error)
        }))
    }

    /// Trusts a reviewed connector file, run without the bridge lock. Inputs: the name and
    /// the SHA-256 its trust screen showed. Output: the page.
    pub fn connector_trust(
        &self,
        name: String,
        sha256: String,
    ) -> Result<Unlocked<ConnectorsView>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .connector_trust(name, sha256)
                .await
                .map_err(connector_error)
        }))
    }

    /// Removes one of the owner's connectors, run without the bridge lock. Input: the name.
    /// Output: the page.
    pub fn connector_remove(&self, name: String) -> Result<Unlocked<ConnectorsView>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client.connector_remove(name).await.map_err(connector_error)
        }))
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
        let key = super::app_plugins::entered_key(key)?;
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .connector_set_key(name, secret, key)
                .await
                .map_err(connector_error)
        }))
    }

    /// Deletes the key saved for one of a connector's secrets, run without the bridge lock.
    /// Inputs: the connector and the secret. Output: the page.
    pub fn connector_clear_key(
        &self,
        name: String,
        secret: String,
    ) -> Result<Unlocked<ConnectorsView>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .connector_clear_key(name, secret)
                .await
                .map_err(connector_error)
        }))
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
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .connector_grant(project, connector, enabled, who)
                .await
                .map_err(connector_error)
        }))
    }

    /// Reads one connector write that waits for, or had, the owner's answer, run without the
    /// bridge lock. Input: the approval id from its notice. Output: the approval.
    pub fn connector_approval(&self, id: i64) -> Result<Unlocked<ApprovalView>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client.connector_approval(id).await.map_err(connector_error)
        }))
    }

    /// Answers one waiting connector write once, run without the bridge lock. Inputs: the
    /// approval id and whether the owner allows it. Output: the approval after the answer.
    pub fn connector_answer(
        &self,
        id: i64,
        allow: bool,
    ) -> Result<Unlocked<ApprovalView>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .answer_connector_write(id, allow)
                .await
                .map_err(connector_error)
        }))
    }

    /// Reads the connectors a project may write with without asking again, run without the
    /// bridge lock. Input: the project. Output: them by name.
    pub fn connector_writes(
        &self,
        project: String,
    ) -> Result<Unlocked<AllowedWrites>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .connector_writes(project)
                .await
                .map_err(connector_error)
        }))
    }

    /// Takes back a project's standing allow for one connector, run without the bridge
    /// lock. Inputs: the project and the connector. Output: the standing allows left.
    pub fn connector_revoke_writes(
        &self,
        project: String,
        connector: String,
    ) -> Result<Unlocked<AllowedWrites>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .revoke_connector_writes(project, connector)
                .await
                .map_err(connector_error)
        }))
    }

    /// Chooses how a connector whose file offers a key and a browser sign-in signs in, run
    /// without the bridge lock. Inputs: the connector and the way. Output: the page.
    pub fn connector_set_sign_in_with(
        &self,
        name: String,
        way: SignInWith,
    ) -> Result<Unlocked<ConnectorsView>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .connector_set_sign_in_with(name, way)
                .await
                .map_err(connector_error)
        }))
    }

    /// Starts a connector's sign-in through the browser, run without the bridge lock; the
    /// app opens the address itself and the page never gets it. Input: the connector.
    /// Output: the address the owner's browser opens.
    pub fn connector_sign_in(&self, name: String) -> Result<Unlocked<String>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .connector_sign_in(name)
                .await
                .map_err(connector_error)
        }))
    }

    /// Reads where a connector's sign-in through the browser stands, run without the bridge
    /// lock. Input: the connector. Output: its state.
    pub fn connector_sign_in_state(
        &self,
        name: String,
    ) -> Result<Unlocked<SignInState>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .connector_sign_in_state(name)
                .await
                .map_err(connector_error)
        }))
    }

    /// Cancels a connector's waiting sign-in, run without the bridge lock. Input: the
    /// connector. Output: where it stands after, still waiting while its answer is redeemed.
    pub fn cancel_connector_sign_in(
        &self,
        name: String,
    ) -> Result<Unlocked<SignInState>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .cancel_connector_sign_in(name)
                .await
                .map_err(connector_error)
        }))
    }

    /// Signs a connector out on this computer, run without the bridge lock; the vendor is
    /// not told. Input: the connector. Output: the page.
    pub fn connector_sign_out(
        &self,
        name: String,
    ) -> Result<Unlocked<ConnectorsView>, BridgeError> {
        let client = self.connectors_client()?;
        Ok(Box::pin(async move {
            client
                .connector_sign_out(name)
                .await
                .map_err(connector_error)
        }))
    }

    /// What the Connectors page says each harness reaches, one row per harness kind in
    /// settings order, in the engine crate's own words (DEC-141), with no engine call.
    /// Output: the rows.
    pub fn connector_reach(&self) -> Vec<&'static str> {
        crazyagent::coord::settings::HarnessKind::ALL
            .into_iter()
            .map(crazyagent::connectors::launch::reach)
            .collect()
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

    /// The harness rows come from connectors::launch::reach for every harness kind, in
    /// order, on a bridge with no engine at all.
    #[test]
    fn connector_reach_names_every_harness_in_the_engine_crates_words_without_an_engine() {
        use crazyagent::coord::settings::HarnessKind;
        let rows = NativeBridge::new(None).connector_reach();
        let words: Vec<&'static str> = HarnessKind::ALL
            .into_iter()
            .map(crazyagent::connectors::launch::reach)
            .collect();
        assert_eq!(rows.len(), HarnessKind::ALL.len());
        assert_eq!(rows, words);
    }

    /// The code a bridge call refused with before any engine call. Input: the call's
    /// result. Output: the code.
    fn refused<T>(result: Result<Unlocked<T>, BridgeError>) -> &'static str {
        match result {
            Ok(_) => panic!("a bridge with no engine readied a Connectors call"),
            Err(error) => error.code,
        }
    }

    /// The approval page's four commands need the engine: on a bridge with none each is
    /// not_connected before any call is readied.
    #[test]
    fn approval_commands_need_the_engine() {
        let bridge = NativeBridge::new(None);
        assert_eq!(refused(bridge.connector_approval(7)), "not_connected");
        assert_eq!(refused(bridge.connector_answer(7, true)), "not_connected");
        assert_eq!(refused(bridge.connector_answer(7, false)), "not_connected");
        assert_eq!(
            refused(bridge.connector_writes("alpha".into())),
            "not_connected"
        );
        assert_eq!(
            refused(bridge.connector_revoke_writes("alpha".into(), "linear".into())),
            "not_connected"
        );
        for code in ["connector_approval_unknown", "connector_approval_closed"] {
            assert_eq!(connector_error(EngineError(code.into())).code, code);
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
        assert_eq!(
            refused(bridge.connector_set_sign_in_with(text(), SignInWith::Browser)),
            "not_connected"
        );
        let way = serde_json::from_value::<SignInWith>(serde_json::json!("browser"));
        assert_eq!(
            way.unwrap(),
            SignInWith::Browser,
            "the window's way words are the engine's"
        );
        assert_eq!(refused(bridge.connector_sign_in(text())), "not_connected");
        assert_eq!(
            refused(bridge.connector_sign_in_state(text())),
            "not_connected"
        );
        assert_eq!(
            refused(bridge.cancel_connector_sign_in(text())),
            "not_connected"
        );
        assert_eq!(refused(bridge.connector_sign_out(text())), "not_connected");
    }
}
