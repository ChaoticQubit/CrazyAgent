use super::*;
use crazyagent::app_plugins::{Listing, Preview};
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{APP_PLUGINS_CODES, AccountSecret, OwnerClient, SearchSaved};
use crazyagent::semantic::{SearchChoice, SearchView};
use serde::Serialize;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

/// The most bytes a picked manifest may have, the engine's own manifest cap (16 KiB).
pub const MANIFEST_BYTES_MAX: u64 = 16 * 1024;

/// How a Search choice save ended, as the window reads it: saved, with the page after it, or
/// stale because the choice changed since the window read it, with the current version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchSave {
    Saved { search: Box<SearchView> },
    Stale { current: u64 },
}

impl From<SearchSaved> for SearchSave {
    fn from(saved: SearchSaved) -> Self {
        match saved {
            SearchSaved::Saved(search) => SearchSave::Saved { search },
            SearchSaved::Stale(current) => SearchSave::Stale { current },
        }
    }
}

/// The bridge failure for an engine app plugins error: a known code reaches the window as
/// itself, anything else is an unknown outcome with the engine code behind it.
/// Input: the engine error. Output: the bridge failure.
fn app_plugin_error(error: EngineError) -> BridgeError {
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

/// Reads one manifest file the owner chose in the native chooser: a regular file, at most
/// `MANIFEST_BYTES_MAX` bytes, UTF-8 text. Input: the chosen path. Output: the text, or
/// `app_plugin_file_unreadable`, `app_plugin_request_bounds` or `app_plugin_file_not_text`.
pub fn read_manifest(path: &Path) -> Result<String, BridgeError> {
    let unreadable = || failure("app_plugin_file_unreadable");
    if !std::fs::metadata(path).map_err(|_| unreadable())?.is_file() {
        return Err(unreadable());
    }
    let file = std::fs::File::open(path).map_err(|_| unreadable())?;
    if !file.metadata().map_err(|_| unreadable())?.is_file() {
        return Err(unreadable());
    }
    let mut bytes = Vec::new();
    file.take(MANIFEST_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unreadable())?;
    if bytes.len() as u64 > MANIFEST_BYTES_MAX {
        return Err(failure("app_plugin_request_bounds"));
    }
    String::from_utf8(bytes).map_err(|_| failure("app_plugin_file_not_text"))
}

/// Reads the manifest at the path the native chooser returned off the async runtime, as
/// `read_manifest` reads it. Input: the chosen path. Output: the text, or its failure, or
/// `picker_failed` when the read could not run.
pub async fn read_picked(path: String) -> Result<String, BridgeError> {
    tokio::task::spawn_blocking(move || read_manifest(Path::new(&path)))
        .await
        .map_err(|_| failure("picker_failed"))?
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

    /// Reads every app plugin, built-in first, run without the bridge lock. Output: the
    /// listing.
    pub fn app_plugins(&self) -> Result<Unlocked<Listing>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client.app_plugins().await.map_err(app_plugin_error)
        }))
    }

    /// Reads a manifest's trust screen without saving it, run without the bridge lock.
    /// Input: the manifest text. Output: ready or refused.
    pub fn app_plugin_preview(&self, yaml: String) -> Result<Unlocked<Preview>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_preview(yaml)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Reads the trust screen of a file in the owner's plugins folder, run without the
    /// bridge lock. Input: the plugin name. Output: ready or refused.
    pub fn app_plugin_review(&self, name: String) -> Result<Unlocked<Preview>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_review(name)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Adds a manifest the owner confirmed, run without the bridge lock. Inputs: the text,
    /// the SHA-256 its trust screen showed and whether it replaces the owner's plugin of that
    /// name. Output: the listing.
    pub fn app_plugin_add(
        &self,
        yaml: String,
        sha256: String,
        replace: bool,
    ) -> Result<Unlocked<Listing>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_add(yaml, sha256, replace)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Trusts a reviewed file in the owner's plugins folder, run without the bridge lock.
    /// Inputs: the name and the SHA-256 its trust screen showed. Output: the listing.
    pub fn app_plugin_trust(
        &self,
        name: String,
        sha256: String,
    ) -> Result<Unlocked<Listing>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_trust(name, sha256)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Turns a plugin on or off, run without the bridge lock. Inputs: the name and the
    /// switch. Output: the listing.
    pub fn app_plugin_set_enabled(
        &self,
        name: String,
        enabled: bool,
    ) -> Result<Unlocked<Listing>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_set_enabled(name, enabled)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Removes one of the owner's plugins, run without the bridge lock. Input: the name.
    /// Output: the listing.
    pub fn app_plugin_remove(&self, name: String) -> Result<Unlocked<Listing>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_remove(name)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Lets a declared secret use an API-key account's saved key, run without the bridge
    /// lock. Inputs: the plugin, the secret and the account id. Output: the listing.
    pub fn app_plugin_bind_account(
        &self,
        name: String,
        secret: String,
        account: String,
    ) -> Result<Unlocked<Listing>, BridgeError> {
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
    /// the secret and the key. Output: the listing.
    pub fn app_plugin_set_key(
        &self,
        name: String,
        secret: String,
        key: String,
    ) -> Result<Unlocked<Listing>, BridgeError> {
        let key = entered_key(key)?;
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_set_key(name, secret, key)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Clears where a secret's key comes from, run without the bridge lock. Inputs: the
    /// plugin and the secret. Output: the listing.
    pub fn app_plugin_unbind(
        &self,
        name: String,
        secret: String,
    ) -> Result<Unlocked<Listing>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .app_plugin_unbind(name, secret)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Reads the Search settings page, run without the bridge lock. Output: the page.
    pub fn search_settings(&self) -> Result<Unlocked<SearchView>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client.search_settings().await.map_err(app_plugin_error)
        }))
    }

    /// Saves what search ranks with, naming the version the window read and, for an HTTP
    /// plugin, the host its confirm named. Run without the bridge lock. Inputs: the choice,
    /// the host and the version. Output: saved with the page, or stale.
    pub fn set_search(
        &self,
        choice: SearchChoice,
        confirm_host: Option<String>,
        version: u64,
    ) -> Result<Unlocked<SearchSave>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .set_search(choice, confirm_host, version)
                .await
                .map(SearchSave::from)
                .map_err(app_plugin_error)
        }))
    }

    /// Starts the built-in model download the owner confirmed, run without the bridge lock.
    /// Input: the size the confirm named, which the engine checks against the pinned size.
    /// Output: the Search page.
    pub fn search_model_download(
        &self,
        confirm_bytes: u64,
    ) -> Result<Unlocked<SearchView>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client
                .search_model_download(confirm_bytes)
                .await
                .map_err(app_plugin_error)
        }))
    }

    /// Cancels the running built-in model download, run without the bridge lock. Output:
    /// the Search page.
    pub fn search_model_cancel(&self) -> Result<Unlocked<SearchView>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client.search_model_cancel().await.map_err(app_plugin_error)
        }))
    }

    /// Deletes the downloaded built-in model, run without the bridge lock. Output: the
    /// Search page.
    pub fn search_model_delete(&self) -> Result<Unlocked<SearchView>, BridgeError> {
        let client = self.app_plugins_client()?;
        Ok(Box::pin(async move {
            client.search_model_delete().await.map_err(app_plugin_error)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Every app plugins code the engine answers reaches the window as itself; anything else
    /// is an unknown outcome with the engine code behind it and raw engine text withheld.
    #[test]
    fn engine_app_plugin_codes_pass_through_unchanged() {
        for code in APP_PLUGINS_CODES {
            assert_eq!(app_plugin_error(EngineError((*code).into())).code, *code);
        }
        assert_eq!(
            app_plugin_error(EngineError("controller_disconnected".into())).code,
            "not_connected"
        );
        let unlisted = app_plugin_error(EngineError("app_plugin_store_torn".into()));
        assert_eq!(unlisted.code, "app_plugin_outcome_unknown");
        assert_eq!(unlisted.cause.as_deref(), Some("app_plugin_store_torn"));
        let raw = app_plugin_error(EngineError("open /Users/mira/engine.db: denied".into()));
        assert_eq!(raw.code, "app_plugin_outcome_unknown");
        assert_eq!(
            raw.cause.as_deref(),
            Some(crazyagent::protocol::TEXT_WITHHELD)
        );
    }

    /// A Search save outcome reaches the window as a tagged object it can switch on.
    #[test]
    fn a_search_save_outcome_serializes_with_its_kind() {
        let stale: SearchSave = SearchSaved::Stale(7).into();
        assert_eq!(
            serde_json::to_value(&stale).unwrap(),
            serde_json::json!({"kind": "stale", "current": 7})
        );
    }

    /// A picked manifest is read whole up to the engine's cap; a larger file, a file that is
    /// not UTF-8, a folder and a missing path each fail with their own code.
    #[test]
    fn a_picked_manifest_is_read_within_its_bounds() {
        let folder = tempfile::tempdir().unwrap();
        let at_cap = folder.path().join("at-cap.yaml");
        let text = format!(
            "schema: 1\n{}",
            "#".repeat(MANIFEST_BYTES_MAX as usize - 10)
        );
        assert_eq!(text.len() as u64, MANIFEST_BYTES_MAX);
        std::fs::write(&at_cap, &text).unwrap();
        assert_eq!(read_manifest(&at_cap).unwrap(), text);
        let over = folder.path().join("over.yaml");
        std::fs::write(&over, format!("{text}#")).unwrap();
        assert_eq!(
            read_manifest(&over).unwrap_err().code,
            "app_plugin_request_bounds"
        );
        let binary = folder.path().join("binary.yaml");
        std::fs::File::create(&binary)
            .unwrap()
            .write_all(&[0x73, 0xff, 0xfe])
            .unwrap();
        assert_eq!(
            read_manifest(&binary).unwrap_err().code,
            "app_plugin_file_not_text"
        );
        assert_eq!(
            read_manifest(folder.path()).unwrap_err().code,
            "app_plugin_file_unreadable"
        );
        assert_eq!(
            read_manifest(&folder.path().join("gone.yaml"))
                .unwrap_err()
                .code,
            "app_plugin_file_unreadable"
        );
    }

    /// An empty key is refused in the bridge before any engine call; a typed one becomes the
    /// engine's secret type and its text never reaches a debug print.
    #[test]
    fn an_entered_key_must_have_text() {
        assert_eq!(
            entered_key(String::new()).unwrap_err().code,
            "app_plugin_key_invalid"
        );
        let secret = entered_key("sk-e2e-synthetic-key".into()).unwrap();
        assert!(!format!("{secret:?}").contains("sk-e2e"));
    }

    /// The manifest chooser is one of the fixed scripts and asks for a manifest file.
    #[test]
    fn the_manifest_chooser_is_a_fixed_script() {
        assert!(super::super::picker::APP_PLUGIN_SCRIPT.contains("choose file"));
        assert!(super::super::picker::APP_PLUGIN_SCRIPT.contains(".yaml"));
        #[cfg(target_os = "macos")]
        assert!(
            super::super::picker::chooser_command(super::super::picker::APP_PLUGIN_SCRIPT).is_ok()
        );
    }
}
