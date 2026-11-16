use super::*;
use crate::app_plugins::Preview;
use crate::connectors::approvals::{AllowedWrites, ApprovalView};
use crate::connectors::audit::ActivityView;
use crate::connectors::sign_in::SignInState;
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

    /// Starts a connector's sign-in through the owner's browser. Input: the connector.
    /// Output: the address the owner's browser opens.
    pub async fn connector_sign_in(&self, name: String) -> Result<String, EngineError> {
        self.connectors_client()?.connector_sign_in(name).await
    }

    /// Reads where a connector's sign-in stands. Input: the connector. Output: its state.
    pub async fn connector_sign_in_state(&self, name: String) -> Result<SignInState, EngineError> {
        self.connectors_client()?
            .connector_sign_in_state(name)
            .await
    }

    /// Cancels a connector's waiting sign-in. Input: the connector. Output: where it stands
    /// after, still waiting while the browser's answer is being redeemed.
    pub async fn cancel_connector_sign_in(&self, name: String) -> Result<SignInState, EngineError> {
        self.connectors_client()?
            .cancel_connector_sign_in(name)
            .await
    }

    /// Signs a connector out at the owner's ask; the vendor is not told. Input: the
    /// connector. Output: the page.
    pub async fn connector_sign_out(&self, name: String) -> Result<ConnectorsView, EngineError> {
        self.connectors_client()?.connector_sign_out(name).await
    }

    /// Saves the owner's own OAuth app for a connector, the secret of a confidential app
    /// in its own frame; saving signs the connector out. Inputs: the connector, the client
    /// id and the secret if any. Output: the page, which never holds the secret.
    pub async fn connector_set_own_app(
        &self,
        name: String,
        client_id: String,
        secret: Option<AccountSecret>,
    ) -> Result<ConnectorsView, EngineError> {
        self.connectors_client()?
            .connector_set_own_app(name, client_id, secret)
            .await
    }

    /// Removes the owner's own OAuth app for a connector, which then signs in the automatic
    /// way and is signed out. Input: the connector. Output: the page.
    pub async fn connector_clear_own_app(
        &self,
        name: String,
    ) -> Result<ConnectorsView, EngineError> {
        self.connectors_client()?
            .connector_clear_own_app(name)
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

    /// Reads one connector write that waits for, or had, the owner's answer. Input: the
    /// approval id from its notice. Output: the approval, or `connector_approval_unknown`.
    pub async fn connector_approval(&self, id: i64) -> Result<ApprovalView, EngineError> {
        self.connectors_client()?.connector_approval(id).await
    }

    /// Answers one waiting connector write once. Inputs: the approval id and whether the
    /// owner allows it. Output: the approval after the answer, or `connector_approval_closed`
    /// when it was answered or its wait ran out first.
    pub async fn answer_connector_write(
        &self,
        id: i64,
        allow: bool,
    ) -> Result<ApprovalView, EngineError> {
        self.connectors_client()?
            .answer_connector_write(id, allow)
            .await
    }

    /// Reads the connectors a project may write with without asking again. Input: the
    /// project. Output: them by name, or `connector_project_unknown`.
    pub async fn connector_writes(&self, project: String) -> Result<AllowedWrites, EngineError> {
        self.connectors_client()?.connector_writes(project).await
    }

    /// Takes back a project's standing allow for one connector, so its next write asks
    /// again. Inputs: the project and the connector. Output: the standing allows left.
    pub async fn revoke_connector_writes(
        &self,
        project: String,
        connector: String,
    ) -> Result<AllowedWrites, EngineError> {
        self.connectors_client()?
            .revoke_connector_writes(project, connector)
            .await
    }
}
