use crate::paths::names_one_of;
use serde::{Deserialize, Serialize};

/// The names the engine keeps for itself in a project folder, never shared or picked.
pub const ENGINE_NAMES: [&str; 3] = [".git", ".crazyagent", "graphify-out"];

/// The most top-level folders one project may share by name under Chosen folders.
pub const CHOSEN_MAX: usize = 16;

/// The most secret files or folders the owner may share back with a project's agents.
pub const SECRETS_MAX: usize = 16;

const NAME_BYTES_MAX: usize = 255;

/// The longest shared-back secret path, in bytes.
pub const SECRET_PATH_BYTES_MAX: usize = 512;

/// The refusal code for a policy whose shared folders choice is out of shape.
pub const INVALID: &str = "agent_policy_invalid";

/// Which of the project folder's local folders a project's agents see (DEC-147.9): All, the
/// default, shares every one, gitignored ones included; Chosen folders shares only the
/// top-level folders the owner named; None shares none, so an agent works in its worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedScope {
    #[default]
    All,
    Chosen,
    None,
}

impl SharedScope {
    /// Every choice, the default first.
    pub const ALL: [Self; 3] = [Self::All, Self::Chosen, Self::None];

    /// The choice's stable id, as stored and sent to clients.
    pub fn id(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Chosen => "chosen",
            Self::None => "none",
        }
    }

    /// The name shown to the owner, the same in the desktop app.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All local folders",
            Self::Chosen => "Chosen folders",
            Self::None => "None",
        }
    }

    /// What the choice gives agents, the same in the desktop app.
    pub fn help(self) -> &'static str {
        match self {
            Self::All => {
                "Agents see every folder in the project folder, the ones git ignores included, such as data and programs."
            }
            Self::Chosen => "Agents see only the top-level folders you pick.",
            Self::None => "Agents see only their own worktree.",
        }
    }
}

/// Whether a project's agents may change its shared local folders (DEC-147.9): Read-only, the
/// default, or Read-write for the shared top-level folders git does not track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedAccess {
    #[default]
    ReadOnly,
    ReadWrite,
}

impl SharedAccess {
    /// Every choice, the default first.
    pub const ALL: [Self; 2] = [Self::ReadOnly, Self::ReadWrite];

    /// The choice's stable id, as stored and sent to clients.
    pub fn id(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::ReadWrite => "read_write",
        }
    }

    /// The name shown to the owner, the same in the desktop app.
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read-only",
            Self::ReadWrite => "Read-write",
        }
    }

    /// What the choice lets agents do, the same in the desktop app.
    pub fn help(self) -> &'static str {
        match self {
            Self::ReadOnly => "Agents read shared folders and never change them.",
            Self::ReadWrite => {
                "Agents may also change files in shared top-level folders git does not track, but not in hidden folders such as .venv or .vscode. Folders such as node_modules, target and venv stay writable, and your own tools run what is in them outside the sandbox. Folders inside stay in place: agents can't move or delete them. Tracked files change only in their worktree."
            }
        }
    }
}

/// One project's shared local folders choice (DEC-147.9): which folders, how, the top-level
/// folders picked for Chosen folders and the secret files or folders the owner shared back.
/// A policy saved before the setting existed reads All and Read-only with nothing picked.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedFolders {
    #[serde(default)]
    pub scope: SharedScope,
    #[serde(default)]
    pub access: SharedAccess,
    #[serde(default)]
    pub chosen: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
}

impl SharedFolders {
    /// Checks a choice a client sent: at most [`CHOSEN_MAX`] picked folders, each one plain
    /// top-level name, and at most [`SECRETS_MAX`] shared-back secrets, each a plain path
    /// inside the project, none twice, never `.git` or `.crazyagent`. Takes the choice.
    /// Returns [`INVALID`] for any other.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.chosen.len() > CHOSEN_MAX || self.secrets.len() > SECRETS_MAX {
            return Err(INVALID);
        }
        let names_ok = self.chosen.iter().all(|name| plain_name(name));
        let paths_ok = self.secrets.iter().all(|path| plain_path(path));
        if !names_ok || !paths_ok || repeats(&self.chosen) || repeats(&self.secrets) {
            return Err(INVALID);
        }
        Ok(())
    }

    /// Whether this choice reaches no further than All with Read-only, the only one the Linux
    /// and Windows boundaries can start. Takes the choice. Returns the answer.
    pub fn plain_reach(&self) -> bool {
        self.scope == SharedScope::All && self.access == SharedAccess::ReadOnly
    }

    /// The shared-back secrets this choice cannot reach (L4 r1 L3): under None each one, and
    /// under Chosen folders each one whose top-level folder is not picked, since the folder
    /// kept out around it wins over the share. Takes the choice. Returns the paths, in the
    /// owner's order.
    pub fn unreachable_secrets(&self) -> Vec<&str> {
        self.secrets
            .iter()
            .map(String::as_str)
            .filter(|path| !self.shares(path.split('/').next().unwrap_or_default()))
            .collect()
    }

    /// Whether the owner's top-level folder `top` is shared under this choice. Takes the
    /// folder's name. Returns the answer.
    pub fn shares(&self, top: &str) -> bool {
        match self.scope {
            SharedScope::All => true,
            SharedScope::Chosen => self.chosen.iter().any(|name| name == top),
            SharedScope::None => false,
        }
    }
}

/// Tells whether a name is one plain top-level folder name an owner may pick: not empty, at
/// most 255 bytes, not `.` or `..`, no engine name, no control character, slash or quote.
/// Input: the name. Output: true when plain.
pub fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name != "."
        && name != ".."
        && !names_one_of(name, &ENGINE_NAMES)
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '"' | '\\' | '\''))
}

fn plain_path(path: &str) -> bool {
    path.len() <= SECRET_PATH_BYTES_MAX && path.split('/').all(plain_name)
}

fn repeats(items: &[String]) -> bool {
    items
        .iter()
        .enumerate()
        .any(|(index, item)| items[..index].contains(item))
}
