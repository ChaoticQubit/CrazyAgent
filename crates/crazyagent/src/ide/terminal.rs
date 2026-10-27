use crate::paths::Platform;
use crate::protocol::ProjectId;
use crate::sandbox::ide_shell::COMMAND;
use serde_json::{Map, Value, json};
use std::path::Path;

/// The label of the default IDE terminal: the owner's shell inside the agent sandbox.
pub const SANDBOXED: &str = "Sandboxed";

/// The label of the owner's own login shell, outside every sandbox.
pub const YOUR_SHELL: &str = "Your shell";

const DETECTED_UNIX: [&str; 6] = ["bash", "zsh", "fish", "tmux", "pwsh", "sh"];

const DETECTED_WINDOWS: [&str; 5] = [
    "PowerShell",
    "Windows PowerShell",
    "Command Prompt",
    "Git Bash",
    "Cygwin",
];

/// What the two IDE terminal profiles start: the engine executable with the project for
/// "Sandboxed", and the owner's login shell for "Your shell".
#[derive(Debug, Clone, Copy)]
pub struct ShellLaunch<'a> {
    pub executable: &'a Path,
    pub project: &'a ProjectId,
    pub owner_shell: &'a Path,
}

/// Builds the IDE terminal settings for one platform: the profile list holding only
/// "Sandboxed" and "Your shell" (every shell the IDE would detect on its own is hidden),
/// "Sandboxed" as the default and as the profile tasks run in, and on Windows no WSL
/// profiles. Inputs: the platform and what the profiles start. Output: the setting keys
/// and values for the engine's settings layer.
pub fn profiles(platform: Platform, launch: &ShellLaunch<'_>) -> Map<String, Value> {
    let (os, detected): (&str, &[&str]) = match platform {
        Platform::MacOs => ("osx", &DETECTED_UNIX[..]),
        Platform::Linux => ("linux", &DETECTED_UNIX[..]),
        Platform::Windows => ("windows", &DETECTED_WINDOWS[..]),
    };
    let sandboxed = json!({
        "path": launch.executable.to_string_lossy(),
        "args": [COMMAND, "--project", launch.project.as_str()],
        "icon": "shield",
        "overrideName": true,
    });
    let your_shell = match platform {
        Platform::Windows => {
            json!({"source": "PowerShell", "icon": "terminal", "overrideName": true})
        }
        Platform::MacOs | Platform::Linux => json!({
            "path": launch.owner_shell.to_string_lossy(),
            "args": ["-l"],
            "icon": "terminal",
            "overrideName": true,
        }),
    };
    let mut listed = Map::new();
    for name in detected {
        listed.insert((*name).to_owned(), Value::Null);
    }
    listed.insert(SANDBOXED.to_owned(), sandboxed.clone());
    listed.insert(YOUR_SHELL.to_owned(), your_shell);
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
        sandboxed,
    );
    if platform == Platform::Windows {
        settings.insert(
            "terminal.integrated.useWslProfiles".to_owned(),
            json!(false),
        );
    }
    assert!(settings.values().filter(|value| value.is_object()).count() == 2);
    assert!(
        settings[&format!("terminal.integrated.profiles.{os}")][SANDBOXED]
            == settings[&format!("terminal.integrated.automationProfile.{os}")]
    );
    settings
}

#[cfg(test)]
mod tests;
