use crate::engine::EngineError;
use crate::paths::Platform;
use crate::sandbox::policy::AgentAccess;
use serde_json::{Map, Value, json};
use std::path::Path;

/// The title of the default IDE terminal: the owner's shell under the agent sandbox, which
/// the engine starts outside the IDE server's sandbox and the built-in extension shows
/// through the bridge as its contributed profile [`SANDBOXED_PROFILE`].
pub const SANDBOXED: &str = "Sandboxed";

/// The id of the terminal profile the built-in extension contributes for [`SANDBOXED`].
pub const SANDBOXED_PROFILE: &str = "crazyagent.sandboxed";

/// The label of the owner's login shell as the IDE's own terminal host runs it: inside the
/// IDE server's sandbox, with no network. Tasks run here too.
pub const IDE_SHELL: &str = "IDE shell (IDE sandbox, no network)";

/// The title of the terminal an Own branch project opens by default: the owner's shell under
/// the project's agent sandbox with the project's network choice, which the engine starts and
/// the built-in extension shows as its contributed profile [`OWN_BRANCH_PROFILE`].
pub const OWN_BRANCH: &str = "Own branch";

/// The id of the terminal profile the built-in extension contributes for [`OWN_BRANCH`].
pub const OWN_BRANCH_PROFILE: &str = "crazyagent.ownBranch";

/// The label of the shell the IDE server hosts, at a level where the owner may pick it.
/// Input: the project's level. Output: the label, or none where no such shell is offered.
pub fn shell_label(_access: AgentAccess) -> Option<&'static str> {
    Some(IDE_SHELL)
}

const DETECTED_UNIX: [&str; 6] = ["bash", "zsh", "fish", "tmux", "pwsh", "sh"];

const DETECTED_WINDOWS: [&str; 5] = [
    "PowerShell",
    "Windows PowerShell",
    "Command Prompt",
    "Git Bash",
    "Cygwin",
];

/// Builds the IDE terminal settings for one platform, for the engine's locked Machine
/// settings layer, which the IDE server cannot write: every shell the IDE would detect on
/// its own hidden, the labelled [`IDE_SHELL`] as the only local profile and the one tasks
/// run in, the extension's [`SANDBOXED`] as the default, no task started on its own when a
/// folder opens, and on Windows no WSL profiles. Inputs: the platform and the owner's
/// login shell. Output: the setting keys and values.
pub fn profiles(
    platform: Platform,
    owner_shell: &Path,
    _access: AgentAccess,
) -> Map<String, Value> {
    let (os, detected): (&str, &[&str]) = match platform {
        Platform::MacOs => ("osx", &DETECTED_UNIX[..]),
        Platform::Linux => ("linux", &DETECTED_UNIX[..]),
        Platform::Windows => ("windows", &DETECTED_WINDOWS[..]),
    };
    let ide_shell = match platform {
        Platform::Windows => {
            json!({"source": "PowerShell", "icon": "terminal", "overrideName": true})
        }
        Platform::MacOs | Platform::Linux => json!({
            "path": owner_shell.to_string_lossy(),
            "args": ["-l"],
            "icon": "terminal",
            "overrideName": true,
        }),
    };
    let mut listed = Map::new();
    for name in detected {
        listed.insert((*name).to_owned(), Value::Null);
    }
    listed.insert(IDE_SHELL.to_owned(), ide_shell.clone());
    let mut settings = Map::new();
    settings.insert(
        format!("terminal.integrated.profiles.{os}"),
        Value::Object(listed),
    );
    settings.insert(
        format!("terminal.integrated.defaultProfile.{os}"),
        json!(SANDBOXED),
    );
    settings.insert(
        format!("terminal.integrated.automationProfile.{os}"),
        ide_shell,
    );
    settings.insert("task.allowAutomaticTasks".to_owned(), json!("off"));
    if platform == Platform::Windows {
        settings.insert(
            "terminal.integrated.useWslProfiles".to_owned(),
            json!(false),
        );
    }
    let profiles = &settings[&format!("terminal.integrated.profiles.{os}")];
    assert!(profiles.get(SANDBOXED).is_none());
    assert!(settings.values().filter(|value| value.is_object()).count() == 2);
    assert!(
        profiles[IDE_SHELL] == settings[&format!("terminal.integrated.automationProfile.{os}")]
    );
    settings
}

/// The IDE terminal settings for this machine, for the Remote settings file the engine
/// writes before each IDE server start: [`profiles`] for the current platform, with the
/// owner's login shell read from the account database, never from `SHELL`. Output: the
/// keys, or `ide_shell_unknown` when the account database names no shell for the owner.
#[cfg(unix)]
pub fn machine_profiles(access: AgentAccess) -> Result<Map<String, Value>, EngineError> {
    use crate::engine::owner::failure;
    let shell =
        crate::sandbox::ide_shell::owner_shell().map_err(|_| failure("ide_shell_unknown"))?;
    if !shell.is_absolute() {
        return Err(failure("ide_shell_unknown"));
    }
    Ok(profiles(Platform::CURRENT, &shell, access))
}

/// The IDE terminal settings for this machine where the IDE shell is PowerShell and needs
/// no login shell path. Output: [`profiles`] for the current platform.
#[cfg(not(unix))]
pub fn machine_profiles(access: AgentAccess) -> Result<Map<String, Value>, EngineError> {
    Ok(profiles(Platform::CURRENT, Path::new(""), access))
}

#[cfg(test)]
mod tests;
