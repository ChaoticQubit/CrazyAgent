use super::*;
use crate::engine::owner::{AccountSecret, AppPluginsPage, OwnerClient, PluginUser};
use kernel_plugin_host::usage::TurnOff;
use kernel_plugin_host::views::Preview;
use std::path::PathBuf;

impl OwnerController {
    fn app_plugins_client(&self) -> Result<&OwnerClient, EngineError> {
        self.client
            .as_deref()
            .ok_or_else(|| EngineError("controller_disconnected".into()))
    }

    /// Reads the App plugins page: every plugin, bundled first, the plain words for each
    /// permission and the change notices not yet shown. Output: the page, or an
    /// `APP_PLUGINS_CODES` refusal.
    pub async fn app_plugins(&self) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_client()?.app_plugins().await
    }

    /// Reads the trust screen of a folder the owner picked, saving nothing. Input: the
    /// folder. Output: ready, or refused with the line or field and a plain sentence.
    pub async fn app_plugin_preview(&self, folder: PathBuf) -> Result<Preview, EngineError> {
        self.app_plugins_client()?.app_plugin_preview(folder).await
    }

    /// Reads the trust screen of a plugin already in the plugins folder. Input: the name.
    /// Output: ready or refused.
    pub async fn app_plugin_review(&self, name: String) -> Result<Preview, EngineError> {
        self.app_plugins_client()?.app_plugin_review(name).await
    }

    /// Adds a folder the owner approved. Inputs: the folder, the SHA-256 its trust screen
    /// showed and whether it replaces a plugin of that name. Output: the page.
    pub async fn app_plugin_add(
        &self,
        folder: PathBuf,
        sha256: String,
        replace: bool,
    ) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_client()?
            .app_plugin_add(folder, sha256, replace)
            .await
    }

    /// Trusts a plugin in the plugins folder after its review. Inputs: the name and the
    /// SHA-256 its trust screen showed. Output: the page.
    pub async fn app_plugin_trust(
        &self,
        name: String,
        sha256: String,
    ) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_client()?
            .app_plugin_trust(name, sha256)
            .await
    }

    /// Turns a plugin on or off; off may pick, for this one call, what happens to runs
    /// holding it. Inputs: the name, the switch and the one-time choice. Output: the page.
    pub async fn app_plugin_set_enabled(
        &self,
        name: String,
        enabled: bool,
        when_off: Option<TurnOff>,
    ) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_client()?
            .app_plugin_set_enabled(name, enabled, when_off)
            .await
    }

    /// Reads the runs holding a plugin, for the turn-off confirm. Input: the name. Output:
    /// the holders.
    pub async fn app_plugin_users(&self, name: String) -> Result<Vec<PluginUser>, EngineError> {
        self.app_plugins_client()?.app_plugin_users(name).await
    }

    /// Removes one of the owner's plugins with the keys entered for it. Input: the name.
    /// Output: the page.
    pub async fn app_plugin_remove(&self, name: String) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_client()?.app_plugin_remove(name).await
    }

    /// Binds a declared secret to a saved API-key account. Inputs: the plugin, the secret
    /// and the account reference. Output: the page.
    pub async fn app_plugin_bind_account(
        &self,
        name: String,
        secret: String,
        account: String,
    ) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_client()?
            .app_plugin_bind_account(name, secret, account)
            .await
    }

    /// Saves a key the owner entered for a declared secret. Inputs: the plugin, the secret
    /// and the key. Output: the page.
    pub async fn app_plugin_set_key(
        &self,
        name: String,
        secret: String,
        key: AccountSecret,
    ) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_client()?
            .app_plugin_set_key(name, secret, key)
            .await
    }

    /// Clears a secret's binding. Inputs: the plugin and the secret. Output: the page.
    pub async fn app_plugin_unbind(
        &self,
        name: String,
        secret: String,
    ) -> Result<AppPluginsPage, EngineError> {
        self.app_plugins_client()?
            .app_plugin_unbind(name, secret)
            .await
    }
}
