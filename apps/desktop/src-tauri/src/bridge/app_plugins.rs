use super::*;
use crazyagent::app_plugins::{Listing, Preview};
use crazyagent::engine::EngineError;
use crazyagent::engine::owner::{APP_PLUGINS_CODES, AccountSecret, OwnerClient, SearchSaved};
use crazyagent::semantic::{SearchChoice, SearchView};
use serde::Serialize;
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
    fn from(_saved: SearchSaved) -> Self {
        SearchSave::Stale { current: 0 }
    }
}

/// The bridge failure for an engine app plugins error: a known code reaches the window as
/// itself, anything else is an unknown outcome with the engine code behind it.
/// Input: the engine error. Output: the bridge failure.
fn app_plugin_error(_error: EngineError) -> BridgeError {
    failure("unknown")
}

/// Reads one manifest file the owner chose in the native chooser: a regular file, at most
/// `MANIFEST_BYTES_MAX` bytes, UTF-8 text. Input: the chosen path. Output: the text, or
/// `app_plugin_file_unreadable`, `app_plugin_request_bounds` or `app_plugin_file_not_text`.
pub fn read_manifest(_path: &Path) -> Result<String, BridgeError> {
    Err(failure("unknown"))
}

/// Turns the key the owner typed into the engine's secret type, refusing an empty one.
/// Input: the typed key. Output: the secret, or `app_plugin_key_invalid`.
fn entered_key(_key: String) -> Result<AccountSecret, BridgeError> {
    Err(failure("unknown"))
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
        let text = format!("schema: 1\n{}", "#".repeat(MANIFEST_BYTES_MAX as usize - 10));
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
        assert!(super::super::picker::chooser_command(super::super::picker::APP_PLUGIN_SCRIPT).is_ok());
    }
}
