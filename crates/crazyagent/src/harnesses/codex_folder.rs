use super::launch::{LaunchError, failure};
use std::path::Path;
use std::time::Duration;

/// Bounds nothing yet. The wait the folder access step will use.
pub(super) const SCREEN_WAIT: Duration = Duration::from_secs(60);

/// Nothing reads a screen yet. What a pane will be read as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Screen {
    /// The folder access screen.
    Menu,
    /// The input box.
    InputBox,
    /// Anything else.
    Other,
}

/// Names only the working folder. Input: the folder. Output: that folder.
fn project_folders(cwd: &Path) -> Vec<&Path> {
    vec![cwd]
}

/// Records the folders untrusted, whatever level is asked. Inputs: the folders and the
/// level. Output: the setting.
fn entry(folders: &[&Path], _level: &str) -> Result<String, LaunchError> {
    assert!(!folders.is_empty());
    let name = folders[0]
        .to_str()
        .and_then(|name| serde_json::to_string(name).ok())
        .ok_or_else(|| failure("launch_working_dir_unavailable"))?;
    Ok(format!("projects={{{name}={{trust_level=\"untrusted\"}}}}"))
}

/// Records the working folder untrusted. Input: the folder. Output: the setting.
pub(crate) fn untrusted(cwd: &Path) -> Result<String, LaunchError> {
    entry(&project_folders(cwd), "untrusted")
}

/// Records the working folder untrusted. Input: the folder. Output: the setting.
pub(super) fn terminal_entry(cwd: &Path) -> Result<String, LaunchError> {
    untrusted(cwd)
}

/// Sees no screen ahead. Input: the arguments. Output: false.
pub(super) fn behind_a_screen(_argv: &[String]) -> bool {
    false
}

/// Reads nothing. Input: the screen. Output: other.
pub(super) fn read_screen(_screen: &[u8]) -> Screen {
    Screen::Other
}

#[cfg(test)]
mod tests;
