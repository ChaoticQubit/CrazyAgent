use super::{BridgeError, NativeBridge, Unlocked, caused, failure};
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{
    APP_PLUGINS_CODES, AccountSecret, AppPluginsPage, OwnerClient, OwnerData, OwnerRead,
    PluginGrants, PluginUser, Preview, TurnOff,
};
use std::path::PathBuf;
use std::sync::Arc;

/// The most bytes a folder path from the web view may hold, the engine's own bound.
const FOLDER_BYTES_MAX: usize = 4096;

/// The bridge failure for an engine App plugins error: a listed code reaches the window as
/// itself, anything else is an unknown outcome with the engine code behind it. Input: the
/// engine error. Output: the bridge failure.
pub fn app_plugin_error(error: EngineError) -> BridgeError {
    if error.0 == "controller_disconnected" {
        return failure("not_connected");
    }
    caused(
        APP_PLUGINS_CODES
            .iter()
            .copied()
            .find(|known| *known == error.0)
            .unwrap_or("app_plugin_outcome_unknown"),
        &error.0,
    )
}

/// The folder the owner picked, as the engine reads it: an absolute path within the bound.
/// The bridge never reads the folder; the engine opens it without following a link.
/// Input: the path text. Output: the path, or `app_plugin_request_invalid`.
pub fn picked_folder(folder: String) -> Result<PathBuf, BridgeError> {
    let path = PathBuf::from(folder);
    if path.as_os_str().is_empty()
        || path.as_os_str().len() > FOLDER_BYTES_MAX
        || !path.is_absolute()
    {
        return Err(failure("app_plugin_request_invalid"));
    }
    Ok(path)
}

/// Turns the key the owner typed into the engine's secret type, refusing an empty one.
/// Input: the typed key. Output: the secret, or `app_plugin_key_invalid`.
fn entered_key(key: String) -> Result<AccountSecret, BridgeError> {
    if key.is_empty() {
        return Err(failure("app_plugin_key_invalid"));
    }
    AccountSecret::new(key.into_bytes()).map_err(|_| failure("app_plugin_key_invalid"))
}

impl NativeBridge {
    fn app_plugins_client(&self) -> Result<Arc<OwnerClient>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.controller
            .as_ref()
            .ok_or_else(|| failure("not_connected"))?
            .shared_client()
            .map_err(app_plugin_error)
    }

    /// Reads the App plugins page, run without the bridge lock. Output: every plugin, the
    /// plain words of each permission and the change notices not yet shown.
    pub fn app_plugins(&self) -> Result<Unlocked<AppPluginsPage>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client.app_plugins().await.map_err(app_plugin_error)
        }))
    }

    /// Reads the trust screen of a folder the owner picked, saving nothing, run without the
    /// bridge lock. Input: the folder. Output: ready or refused.
    pub fn app_plugin_preview(&self, folder: String) -> Result<Unlocked<Preview>, BridgeError> {
        let folder = picked_folder(folder)?;
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_preview(folder)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Reads the trust screen of a plugin already in the plugins folder, run without the
    /// bridge lock. Input: the name. Output: ready or refused.
    pub fn app_plugin_review(&self, name: String) -> Result<Unlocked<Preview>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_review(name)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Adds the folder the owner approved, run without the bridge lock. Inputs: the folder,
    /// the SHA-256 its trust screen showed and whether it replaces a plugin of that name.
    /// Output: the page.
    pub fn app_plugin_add(
        &self,
        folder: String,
        sha256: String,
        replace: bool,
    ) -> Result<Unlocked<AppPluginsPage>, BridgeError> {
        let folder = picked_folder(folder)?;
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_add(folder, sha256, replace)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Trusts a plugin in the plugins folder after its review, run without the bridge
    /// lock. Inputs: the name and the SHA-256 its trust screen showed. Output: the page.
    pub fn app_plugin_trust(
        &self,
        name: String,
        sha256: String,
    ) -> Result<Unlocked<AppPluginsPage>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_trust(name, sha256)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Turns a plugin on or off, run without the bridge lock; off may pick, for this one
    /// call, what happens to runs holding it. Inputs: the name, the switch and the choice.
    /// Output: the page.
    pub fn app_plugin_set_enabled(
        &self,
        name: String,
        enabled: bool,
        when_off: Option<TurnOff>,
    ) -> Result<Unlocked<AppPluginsPage>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_set_enabled(name, enabled, when_off)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Reads the runs holding a plugin for the turn-off confirm, run without the bridge
    /// lock. Input: the name. Output: the holders.
    pub fn app_plugin_users(&self, name: String) -> Result<Unlocked<Vec<PluginUser>>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_users(name)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Removes one of the owner's plugins with the key entered for it, run without the
    /// bridge lock. Input: the name. Output: the page.
    pub fn app_plugin_remove(&self, name: String) -> Result<Unlocked<AppPluginsPage>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_remove(name)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Lets a declared secret use an API-key account's saved key, run without the bridge
    /// lock. Inputs: the plugin, the secret and the account id. Output: the page.
    pub fn app_plugin_bind_account(
        &self,
        name: String,
        secret: String,
        account: String,
    ) -> Result<Unlocked<AppPluginsPage>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_bind_account(name, secret, account)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Saves a key the owner entered for a declared secret; it goes to the engine in its own
    /// frame and the bridge keeps no copy. Run without the bridge lock. Inputs: the plugin,
    /// the secret and the key. Output: the page.
    pub fn app_plugin_set_key(
        &self,
        name: String,
        secret: String,
        key: String,
    ) -> Result<Unlocked<AppPluginsPage>, BridgeError> {
        let key = entered_key(key)?;
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_set_key(name, secret, key)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Clears a secret's binding, deleting the key entered for it, run without the bridge
    /// lock. Inputs: the plugin and the secret. Output: the page.
    pub fn app_plugin_unbind(
        &self,
        name: String,
        secret: String,
    ) -> Result<Unlocked<AppPluginsPage>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_unbind(name, secret)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Saves what happens to running work when a plugin is turned off, run without the
    /// bridge lock. Input: the choice. Output: the page.
    pub fn app_plugin_set_when_off(
        &self,
        when_off: TurnOff,
    ) -> Result<Unlocked<AppPluginsPage>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_set_when_off(when_off)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Reads what each bundled code plugin is granted and whether its switch stays locked
    /// on. Output: one row per bundled code plugin, or `owner_read_failed`.
    pub async fn app_plugin_grants(&mut self) -> Result<Vec<PluginGrants>, BridgeError> {
        let reply = self
            .controller()?
            .read(OwnerRead::PluginGrants)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        let OwnerData::PluginGrants { plugins } = reply.data else {
            return Err(failure("unexpected_reply"));
        };
        Ok(plugins)
    }
}

#[cfg(test)]
#[path = "app_plugins_tests.rs"]
mod tests;
