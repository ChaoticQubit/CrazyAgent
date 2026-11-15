use super::{BridgeError, ENGINE_VERSION_MISMATCH, NativeBridge, bounded, caused, failure};
use crazyagent::harnesses::HarnessEntry;
use crazyagent::harnesses::finding::{self, Found};
use crazyagent::harnesses::update::{HarnessVersions, INSTALL_LIST_INVALID, UpdateSettings};

const HARNESS_ID_BYTES_MAX: usize = 64;
const UNKNOWN_HARNESS: &str = "harness_update_unknown_harness";
const OUTCOME_UNKNOWN: &str = "harness_update_outcome_unknown";
const HARNESS_CODES: [&str; 28] = [
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
    "harness_update_check_failed",
    "harness_update_start_failed",
    "harness_update_settings_unreadable",
    "harness_update_unavailable",
    "harness_install_no_tool",
    "harness_install_off",
    "harness_install_already",
    "harness_install_not_found",
    "harness_install_failed",
    "harness_install_needs_node",
    INSTALL_LIST_INVALID,
];

/// The refusals a Check again or a chosen harness file may carry to the window; any other
/// engine text becomes `harness_path_refused`, so raw engine text never reaches the window.
const FOUND_CODES: [&str; 13] = [
    finding::UNKNOWN_HARNESS,
    finding::PATH_INVALID,
    finding::PATH_NOT_FOUND,
    finding::PATH_NOT_EXECUTABLE,
    finding::PATH_AGENT_SPACE,
    finding::PATH_SHIM,
    finding::PATH_CHANGEABLE,
    finding::PATH_UNREADABLE,
    "native_auth_host_executable_unsafe",
    "native_auth_host_executable_changed",
    "native_auth_host_unavailable",
    "native_auth_host_registration_frozen",
    "owner_runtime_stale",
];
const FOUND_REFUSED: &str = "harness_path_refused";

/// The plain code for a refusal of a found or chosen harness file. Input: the engine code.
/// Output: a known code, or `harness_path_refused`.
fn found_code(engine: &str) -> &'static str {
    if engine == ENGINE_VERSION_MISMATCH {
        return ENGINE_VERSION_MISMATCH;
    }
    FOUND_CODES
        .into_iter()
        .find(|code| *code == engine)
        .unwrap_or(FOUND_REFUSED)
}

/// Refuses a harness id that has no chosen file before it reaches the engine.
/// Input: the id from the window. Output: Ok for codex or claude-code, else the refusal.
fn choosable(id: &str) -> Result<(), BridgeError> {
    if finding::CHOOSABLE.contains(&id) {
        Ok(())
    } else {
        Err(failure(finding::UNKNOWN_HARNESS))
    }
}

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

    /// Starts the install the owner confirmed for a harness that is not on this machine; the
    /// engine runs it in the background. Input: the catalog id. Output: Ok once it runs, or a
    /// harness code.
    pub async fn harness_install(&self, id: &str) -> Result<(), BridgeError> {
        harness_id(id)?;
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.reader()?
            .harness_install(id)
            .await
            .map_err(|error| harness_failure(&error.0))
    }

    /// Queues the installs of several harnesses that are not on this machine; the engine runs
    /// them one after another in the background. Input: the catalog ids, at most one per
    /// catalog harness. Output: Ok once queued, or a harness code.
    pub async fn harness_install_many(&self, ids: &[String]) -> Result<(), BridgeError> {
        if ids.is_empty() || ids.len() > crazyagent::harnesses::specs().len() {
            return Err(failure(INSTALL_LIST_INVALID));
        }
        for id in ids {
            harness_id(id)?;
        }
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.reader()?
            .harness_install_many(ids)
            .await
            .map_err(|error| harness_failure(&error.0))
    }

    /// Reads the harness ids still waiting in the engine's install queue, in the order they
    /// will run. Output: the ids, or a harness code.
    pub async fn harness_queued(&self) -> Result<Vec<String>, BridgeError> {
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        self.reader()?
            .harness_queued()
            .await
            .map_err(|error| harness_failure(&error.0))
    }

    /// Has the engine look for Codex or Claude Code again and pin it for sign-in when the engine
    /// started without it. Input: the catalog id. Output: what was found, with any refusal as
    /// a plain code, or the refusal of the request.
    pub async fn harness_find(&self, id: &str) -> Result<Found, BridgeError> {
        choosable(id)?;
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let found = self
            .reader()?
            .harness_find(id)
            .await
            .map_err(|error| failure(found_code(&error.0)))?;
        bounded(plain_found(found))
    }

    /// Saves the harness file the owner chose, after every engine check, and looks again with
    /// it; none clears the choice. Inputs: the catalog id and the absolute path from the
    /// chooser or typed by the owner. Output: what was found, or the plain refusal.
    pub async fn harness_choose_path(
        &self,
        id: &str,
        path: Option<&str>,
    ) -> Result<Found, BridgeError> {
        choosable(id)?;
        let path = match path {
            Some(text) => {
                let trimmed = text.trim_end_matches(['\n', '\r']);
                let fits = trimmed.len() <= finding::PATH_BYTES_MAX
                    && std::path::Path::new(trimmed).is_absolute()
                    && !trimmed.chars().any(char::is_control);
                if !fits {
                    return Err(failure(finding::PATH_INVALID));
                }
                Some(std::path::PathBuf::from(trimmed))
            }
            None => None,
        };
        if !self.status().connected {
            return Err(failure("not_connected"));
        }
        let found = self
            .reader()?
            .harness_choose_path(id, path.as_deref())
            .await
            .map_err(|error| failure(found_code(&error.0)))?;
        bounded(plain_found(found))
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

/// Keeps a found answer to plain codes: a refusal's engine text is replaced by its known code
/// or `harness_path_refused`. Input: the engine answer. Output: the answer for the window.
fn plain_found(found: Found) -> Found {
    match found {
        Found::Refused { path, code } => Found::Refused {
            path,
            code: found_code(&code).to_owned(),
        },
        other => other,
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
    fn a_found_refusal_reaches_the_window_only_as_a_known_code() {
        let refused = |code: &str| Found::Refused {
            path: "/opt/homebrew/bin/codex".into(),
            code: code.into(),
        };
        assert_eq!(
            plain_found(refused("native_auth_host_executable_unsafe")),
            refused("native_auth_host_executable_unsafe")
        );
        assert_eq!(
            plain_found(refused("Permission denied (os error 13) at /Users/someone")),
            refused(FOUND_REFUSED)
        );
        let missing = Found::Missing {
            looked: vec!["~/.local/bin".into()],
        };
        assert_eq!(plain_found(missing.clone()), missing);
        assert!(choosable("codex").is_ok());
        assert!(choosable("claude-code").is_ok());
        for id in ["claude", "aider", "rcli", ""] {
            assert_eq!(
                choosable(id).unwrap_err().code,
                finding::UNKNOWN_HARNESS,
                "{id}"
            );
        }
    }

    #[test]
    fn a_harness_id_is_short_lowercase_words() {
        for id in ["claude-code", "codex", "cursor-agent", "rcli"] {
            assert!(harness_id(id).is_ok(), "{id}");
        }
        let long = "a".repeat(HARNESS_ID_BYTES_MAX + 1);
        for id in [
            "",
            "../codex",
            "Codex",
            "claude code",
            "codex;rm",
            long.as_str(),
        ] {
            assert_eq!(harness_id(id).unwrap_err().code, UNKNOWN_HARNESS, "{id}");
        }
    }
}
