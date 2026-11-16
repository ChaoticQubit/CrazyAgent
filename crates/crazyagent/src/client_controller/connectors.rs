use super::*;
use crate::app_plugins::Preview;
use crate::connectors::audit::ActivityView;
use crate::connectors::{ConnectorSettings, ConnectorsView, SignInWith, Who};
use crate::engine::owner::{AccountSecret, ConnectorsSaved};

impl OwnerController {
    fn connectors_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads the Connectors page. Input: the project whose switches to include, if any.
    /// Output: the page, or a `CONNECTORS_CODES` refusal.
    pub async fn connectors(&self, project: Option<String>) -> Result<ConnectorsView, EngineError> {
        self.connectors_client()?.connectors(project).await
    }

    /// Reads a connector manifest's trust screen without saving it. Input: the manifest
    /// text. Output: ready or refused with line and plain sentence.
    pub async fn connector_preview(&self, yaml: String) -> Result<Preview, EngineError> {
        self.connectors_client()?.connector_preview(yaml).await
    }

    /// Reads the trust screen of a listed connector. Input: the name. Output: ready or
    /// refused.
    pub async fn connector_review(&self, name: String) -> Result<Preview, EngineError> {
        self.connectors_client()?.connector_review(name).await
    }

    /// Adds a confirmed connector manifest. Inputs: the text, the SHA-256 its trust screen
    /// showed and whether it replaces the owner's connector of that name. Output: the page.
    pub async fn connector_add(
        &self,
        yaml: String,
        sha256: String,
        replace: bool,
    ) -> Result<ConnectorsView, EngineError> {
        self.connectors_client()?
            .connector_add(yaml, sha256, replace)
            .await
    }

    /// Trusts a reviewed connector file in the owner's folder. Inputs: the name and the
    /// SHA-256 its trust screen showed. Output: the page.
    pub async fn connector_trust(
        &self,
        name: String,
        sha256: String,
    ) -> Result<ConnectorsView, EngineError> {
        self.connectors_client()?
            .connector_trust(name, sha256)
            .await
    }

    /// Removes one of the owner's connectors with its keys and switches. Input: the name.
    /// Output: the page.
    pub async fn connector_remove(&self, name: String) -> Result<ConnectorsView, EngineError> {
        self.connectors_client()?.connector_remove(name).await
    }

    /// Saves a key entered for a connector's secret, sent as its own frame. Inputs:
    /// connector, secret and the key. Output: the page.
    pub async fn connector_set_key(
        &self,
        name: String,
        secret: String,
        key: AccountSecret,
    ) -> Result<ConnectorsView, EngineError> {
        self.connectors_client()?
            .connector_set_key(name, secret, key)
            .await
    }

    /// Deletes the key saved for a connector's secret. Inputs: connector and secret.
    /// Output: the page.
    pub async fn connector_clear_key(
        &self,
        name: String,
        secret: String,
    ) -> Result<ConnectorsView, EngineError> {
        self.connectors_client()?
            .connector_clear_key(name, secret)
            .await
    }

    /// Turns a connector on or off for a project and says who it reaches. Inputs: project,
    /// connector, the switch and who. Output: the page with that project's switches.
    pub async fn connector_grant(
        &self,
        project: String,
        connector: String,
        enabled: bool,
        who: Who,
    ) -> Result<ConnectorsView, EngineError> {
        self.connectors_client()?
            .connector_grant(project, connector, enabled, who)
            .await
    }

    /// Chooses how a connector whose file offers a key and a browser sign-in signs in.
    /// Inputs: the connector and the way. Output: the page.
    pub async fn connector_set_sign_in_with(
        &self,
        name: String,
        way: SignInWith,
    ) -> Result<ConnectorsView, EngineError> {
        self.connectors_client()?
            .connector_set_sign_in_with(name, way)
            .await
    }

    /// Saves the connector settings. Inputs: the settings and the version read. Output:
    /// saved with the page, or stale.
    pub async fn set_connector_settings(
        &self,
        settings: ConnectorSettings,
        version: u64,
    ) -> Result<ConnectorsSaved, EngineError> {
        self.connectors_client()?
            .set_connector_settings(settings, version)
            .await
    }

    /// Reads the connector activity log. Inputs: the connector and the project to filter
    /// by, none for all. Output: the view, or a `CONNECTORS_CODES` refusal.
    pub async fn connector_activity(
        &self,
        connector: Option<String>,
        project: Option<String>,
    ) -> Result<ActivityView, EngineError> {
        self.connectors_client()?
            .connector_activity(connector, project)
            .await
    }
}
