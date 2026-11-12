use super::*;
use crate::app_plugins::{Listing, Preview};
use crate::engine::owner::{AccountSecret, SearchSaved};
use crate::semantic::{SearchChoice, SearchView};

impl OwnerController {
    fn app_plugins_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads every app plugin, bundled first. Output: the listing, or an
    /// `APP_PLUGINS_CODES` refusal.
    pub async fn app_plugins(&self) -> Result<Listing, EngineError> {
        self.app_plugins_client()?.app_plugins().await
    }

    /// Reads a manifest's trust screen without saving it. Input: the manifest text.
    /// Output: ready or refused with line and plain sentence.
    pub async fn app_plugin_preview(&self, yaml: String) -> Result<Preview, EngineError> {
        self.app_plugins_client()?.app_plugin_preview(yaml).await
    }

    /// Reads the trust screen of a file in the owner's plugins folder. Input: the name.
    /// Output: ready or refused.
    pub async fn app_plugin_review(&self, name: String) -> Result<Preview, EngineError> {
        self.app_plugins_client()?.app_plugin_review(name).await
    }

    /// Adds a confirmed manifest. Inputs: the text, the SHA-256 its trust screen showed
    /// and whether it replaces a user plugin of that name. Output: the listing.
    pub async fn app_plugin_add(
        &self,
        yaml: String,
        sha256: String,
        replace: bool,
    ) -> Result<Listing, EngineError> {
        self.app_plugins_client()?
            .app_plugin_add(yaml, sha256, replace)
            .await
    }

    /// Trusts a reviewed file in the owner's folder. Inputs: the name and the SHA-256 its
    /// trust screen showed. Output: the listing.
    pub async fn app_plugin_trust(
        &self,
        name: String,
        sha256: String,
    ) -> Result<Listing, EngineError> {
        self.app_plugins_client()?
            .app_plugin_trust(name, sha256)
            .await
    }

    /// Turns a plugin on or off. Inputs: the name and the switch. Output: the listing.
    pub async fn app_plugin_set_enabled(
        &self,
        name: String,
        enabled: bool,
    ) -> Result<Listing, EngineError> {
        self.app_plugins_client()?
            .app_plugin_set_enabled(name, enabled)
            .await
    }

    /// Removes one of the owner's plugins. Input: the name. Output: the listing.
    pub async fn app_plugin_remove(&self, name: String) -> Result<Listing, EngineError> {
        self.app_plugins_client()?.app_plugin_remove(name).await
    }

    /// Binds a declared secret to an API-key account's saved key. Inputs: plugin, secret
    /// and the account's credential reference. Output: the listing.
    pub async fn app_plugin_bind_account(
        &self,
        name: String,
        secret: String,
        account: String,
    ) -> Result<Listing, EngineError> {
        self.app_plugins_client()?
            .app_plugin_bind_account(name, secret, account)
            .await
    }

    /// Saves a key entered for a declared secret, sent as its own frame. Inputs: plugin,
    /// secret and the key. Output: the listing.
    pub async fn app_plugin_set_key(
        &self,
        name: String,
        secret: String,
        key: AccountSecret,
    ) -> Result<Listing, EngineError> {
        self.app_plugins_client()?
            .app_plugin_set_key(name, secret, key)
            .await
    }

    /// Clears a secret's binding and deletes the plugin's own key. Inputs: plugin and
    /// secret. Output: the listing.
    pub async fn app_plugin_unbind(
        &self,
        name: String,
        secret: String,
    ) -> Result<Listing, EngineError> {
        self.app_plugins_client()?
            .app_plugin_unbind(name, secret)
            .await
    }

    /// Reads the Search settings page. Output: choice, options, model state, progress.
    pub async fn search_settings(&self) -> Result<SearchView, EngineError> {
        self.app_plugins_client()?.search_settings().await
    }

    /// Saves what search ranks with. Inputs: the choice, the host the confirm named for an
    /// HTTP plugin, and the version read. Output: saved with the page, or stale.
    pub async fn set_search(
        &self,
        choice: SearchChoice,
        confirm_host: Option<String>,
        version: u64,
    ) -> Result<SearchSaved, EngineError> {
        self.app_plugins_client()?
            .set_search(choice, confirm_host, version)
            .await
    }

    /// Starts the local model download the owner confirmed. Input: the size the confirm
    /// showed. Output: the Search page, or `search_model_confirm` when it is not the pinned
    /// size.
    pub async fn search_model_download(
        &self,
        confirm_bytes: u64,
    ) -> Result<SearchView, EngineError> {
        self.app_plugins_client()?
            .search_model_download(confirm_bytes)
            .await
    }

    /// Cancels the local model download. Output: the Search page.
    pub async fn search_model_cancel(&self) -> Result<SearchView, EngineError> {
        self.app_plugins_client()?.search_model_cancel().await
    }

    /// Deletes the downloaded local model. Output: the Search page.
    pub async fn search_model_delete(&self) -> Result<SearchView, EngineError> {
        self.app_plugins_client()?.search_model_delete().await
    }
}
