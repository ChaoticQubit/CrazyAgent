use crate::paths::canonical;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(unix)]
const PER_USER_TEMP: &str = "/private/var/folders/";

/// One row of a project's grant log as Agent access lists it: when, what happened, the
/// grant in the owner's words and who did it (`owner`, or `question <id>` for an Allow).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogView {
    pub at_ms: i64,
    pub action: String,
    pub grant: String,
    pub by: String,
}

/// The engine's own temp folder when it is the owner's private one: on macOS a folder
/// under `/private/var/folders/`, owned by the engine's user and closed to every other
/// user. A shared temp folder such as `/tmp` is never handed to agents. Takes nothing.
/// Returns the canonical folder, or `temp_folder_shared` or `temp_folder_needs_macos`.
pub fn owner_temp_folder() -> Result<PathBuf, &'static str> {
    if !cfg!(target_os = "macos") {
        return Err("temp_folder_needs_macos");
    }
    private_temp(&std::env::temp_dir())
}

#[cfg(unix)]
fn private_temp(folder: &Path) -> Result<PathBuf, &'static str> {
    use std::os::unix::fs::MetadataExt;
    let real = canonical(folder).map_err(|_| "temp_folder_shared")?;
    let meta = std::fs::symlink_metadata(&real).map_err(|_| "temp_folder_shared")?;
    let mine = meta.uid() == nix::unistd::getuid().as_raw();
    let private = meta.mode() & 0o077 == 0;
    let per_user = real
        .to_str()
        .is_some_and(|text| text.starts_with(PER_USER_TEMP));
    if !meta.is_dir() || !mine || !private || !per_user {
        return Err("temp_folder_shared");
    }
    assert!(real.is_absolute());
    Ok(real)
}

#[cfg(not(unix))]
fn private_temp(_folder: &Path) -> Result<PathBuf, &'static str> {
    Err("temp_folder_needs_macos")
}

/// The plain sentence for a grant refusal code, the same in the desktop app and the tool
/// reply. Input: the code. Output: the sentence.
pub fn refusal_text(code: &str) -> &'static str {
    match code {
        "temp_folder_shared" => {
            "Your temp folder is shared with other users on this computer, so it is never given to agents."
        }
        "temp_folder_needs_macos" => {
            "Your temp folder can be given to agents only on macOS in this version."
        }
        "grant_socket_needs_macos" => "A socket can be allowed only on macOS in this version.",
        "grant_socket_not_plain" => {
            "The socket path must be a full path of letters, digits, slashes, dots, dashes and underscores, at most 103 bytes, in a folder that exists."
        }
        "grant_socket_secret" => {
            "That socket sits with your keys or credentials, so it is never allowed."
        }
        "grant_socket_engine" => {
            "That socket belongs to CrazyAgent itself, so it is never allowed."
        }
        "grant_socket_agent" => {
            "That socket controls a terminal, an editor or an SSH connection (tmux, screen, dtach, Zellij, kitty, WezTerm, Alacritty, iTerm2, Emacs, Neovim, Kakoune, VS Code, Cursor and the like) or hands out your SSH keys, which can run anything as you, so it is never allowed."
        }
        "grant_socket_host" => {
            "That socket controls containers or virtual machines (Docker, containerd, Podman, Colima, Lima, OrbStack and the like), which can reach your whole computer, so it is never allowed."
        }
        "grant_socket_keys" => {
            "That socket belongs to a key or password agent (gpg-agent, 1Password, SSH or another agent), so it is never allowed."
        }
        "grant_socket_owner" => {
            "That socket belongs to the system or another user, has a second name, or sits in a folder other users can change, so it is never allowed."
        }
        "grant_socket_refused" => {
            "That socket cannot be allowed. A socket can be allowed only when it is there now, belongs to the owner with one name, sits in a folder only the owner can change, and is no socket of CrazyAgent itself, of keys or credentials, of containers or virtual machines, or of a terminal, editor, SSH connection or key agent. Check the path and that the program making it runs, then ask again."
        }
        "grant_socket_missing" => {
            "There is no socket at that path now. Start the program that makes it, then allow it again."
        }
        "grant_port_reserved" => {
            "That port belongs to your sign-in, your local model or CrazyAgent itself (its IDE or model proxy), or controls Docker, a browser or a debugger (Node, Java, Python or Go), so it is never allowed."
        }
        "grant_port_dynamic" => {
            "Ports from 49152 up are lent by the system to CrazyAgent's IDE and other apps for a while, so they are never allowed. Pick a fixed port below 49152."
        }
        "agent_grants_invalid" => {
            "A project allows at most 2 sockets, 4 ports and your temp folder, each once."
        }
        "grant_full" => "This project already allows as many extras of that kind as it can.",
        "grant_requests_full" => {
            "Four access requests of this project are still waiting for the owner."
        }
        "grant_reason_invalid" => "Give a reason in plain words.",
        _ => "The request could not be recorded.",
    }
}

#[cfg(test)]
mod tests;
