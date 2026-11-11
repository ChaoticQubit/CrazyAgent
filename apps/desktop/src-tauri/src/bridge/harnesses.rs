use super::{BridgeError, ENGINE_VERSION_MISMATCH, NativeBridge, bounded, caused, failure};
use crazyagent::harnesses::HarnessEntry;
use crazyagent::harnesses::update::{HarnessVersions, UpdateSettings};

const HARNESS_ID_BYTES_MAX: usize = 64;
const UNKNOWN_HARNESS: &str = "harness_update_unknown_harness";
const OUTCOME_UNKNOWN: &str = "harness_update_outcome_unknown";
const HARNESS_CODES: [&str; 17] = [
    "harness_update_unknown_install",
    "harness_update_with_app",
    "harness_update_tool_missing",
    "harness_update_tool_unsafe",
    "harness_update_needs_admin",
    "harness_update_as_root",
    "harness_update_off",
    "harness_update_check_only",
    "harness_update_busy",
    "harness_update_not_installed",
    "harness_update_timeout",
    "harness_update_failed",
    "harness_update_unsupported_here",
    "harness_update_lookup_failed",
    "harness_update_check_unreadable",
    UNKNOWN_HARNESS,
    OUTCOME_UNKNOWN,
];

/// Refuses a harness id the catalog could never hold before it reaches the engine.
/// Input: the id from the window. Output: Ok, or `harness_update_unknown_harness`.
fn harness_id(id: &str) -> Result<(), BridgeError> {
    let fits = !id.is_empty()
        && id.len() <= HARNESS_ID_BYTES_MAX
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if fits {
        Ok(())
    } else {
        Err(failure(UNKNOWN_HARNESS))
    }
}

/// The window code for an engine answer to a harness version, update or settings call: a known
/// harness code passes as itself, an engine version mismatch stays that code, anything else is
/// an unknown outcome. Input: the engine code. Output: the error; raw engine text never passes.
fn harness_failure(engine: &str) -> BridgeError {
    if engine == ENGINE_VERSION_MISMATCH {
        return failure(ENGINE_VERSION_MISMATCH);
    }
    failure(
        HARNESS_CODES
            .into_iter()
            .find(|code| *code == engine)
            .unwrap_or(OUTCOME_UNKNOWN),
    )
}

impl NativeBridge {
    /// Lists every harness the engine knows with its status on this machine, from
    /// the engine's last probe unless `check` asks it to look again now. Input:
    /// whether to probe now. Output: catalog entries in owner order, each with a
    /// plain status such as "Ready", "Not installed" or "Limited control: <reason>".
    pub async fn harness_catalog(&self, check: bool) -> Result<Vec<HarnessEntry>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let entries = self
            .reader()?
            .harness_catalog(check)
            .await
            .map_err(|error| caused("owner_read_failed", &error.0))?;
        assert!(!entries.is_empty());
        bounded(entries)
    }

    /// Reads one harness's installed version, how it was installed and the exact update line,
    /// and has the engine look for a newer version when `look` is set. Inputs: the catalog id
    /// and whether to look now. Output: the versions, or a harness code.
    pub async fn harness_versions(
        &self,
        id: &str,
        look: bool,
    ) -> Result<HarnessVersions, BridgeError> {
        harness_id(id)?;
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let versions = self
            .reader()?
            .harness_versions(id, look)
            .await
            .map_err(|error| harness_failure(&error.0))?;
        if versions.id != id {
            return Err(failure(OUTCOME_UNKNOWN));
        }
        bounded(versions)
    }

    /// Starts the update the owner confirmed; the engine runs it in the background.
    /// Input: the catalog id. Output: Ok once it runs, or a harness code.
    pub async fn harness_update(&self, id: &str) -> Result<(), BridgeError> {
        harness_id(id)?;
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.reader()?
            .harness_update(id)
            .await
            .map_err(|error| harness_failure(&error.0))
    }

    /// Reads the owner's harness update settings. Output: the settings, or a harness code.
    pub async fn harness_update_settings(&self) -> Result<UpdateSettings, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.reader()?
            .harness_update_settings()
            .await
            .map_err(|error| harness_failure(&error.0))
    }

    /// Saves all harness update settings at once. Input: the settings the window shows.
    /// Output: Ok once stored, or a harness code.
    pub async fn harness_set_update_settings(
        &self,
        settings: UpdateSettings,
    ) -> Result<(), BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.reader()?
            .harness_set_update_settings(settings)
            .await
            .map(|_| ())
            .map_err(|error| harness_failure(&error.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every harness code the bridge passes has a sentence in the window, and nothing else
    /// the engine says reaches the window as its own code.
    #[test]
    fn every_harness_code_has_a_window_sentence_and_others_are_unknown() {
        let window = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/api-harness-updates.ts"
        ))
        .unwrap();
        for code in HARNESS_CODES {
            assert!(window.contains(&format!("  {code}: \"")), "{code}");
            assert_eq!(harness_failure(code).code, code);
        }
        for other in ["controller_disconnected", "harness_update_\u{1b}[2J", ""] {
            assert_eq!(harness_failure(other).code, OUTCOME_UNKNOWN);
        }
        assert_eq!(
            harness_failure(ENGINE_VERSION_MISMATCH).code,
            ENGINE_VERSION_MISMATCH
        );
    }

    /// Only an id the catalog could hold reaches the engine.
    #[test]
    fn a_harness_id_is_short_lowercase_words() {
        for id in ["claude-code", "codex", "cursor-agent", "rcli"] {
            assert!(harness_id(id).is_ok(), "{id}");
        }
        let long = "a".repeat(HARNESS_ID_BYTES_MAX + 1);
        for id in ["", "../codex", "Codex", "claude code", "codex;rm", long.as_str()] {
            assert_eq!(harness_id(id).unwrap_err().code, UNKNOWN_HARNESS, "{id}");
        }
    }
}
