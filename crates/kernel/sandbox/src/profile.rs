/// The sandbox runner and the shims every confined agent starts through: the session and
/// pane shims, the pane keeper and its words, and the trait a boundary wraps a command with.
pub mod command;
/// The limits, the Seatbelt lists, the notices and refusal codes, and the variables the
/// engine itself sets in every agent's environment.
pub mod consts;
/// The names that tell an agent's own lineage: the lineage variable and token.
pub mod env;

/// The Seatbelt rule writers: the denies for app launches, a clone's object store and a
/// lead's clone, the root pins and nested git denies, and the packing and bound checks of
/// the regex strings a profile holds.
pub mod seatbelt;

use crate::access::AccessHooks;
use crate::toolchains::ToolchainHooks;
use std::path::{Path, PathBuf};

/// What an agent's profile reads from the engine that builds it: the engine's own folder
/// and file names, the agent home lists and the engine checks the Seatbelt rules name, so
/// this crate names no engine module. The engine passes one value; rows pass stand-ins.
#[derive(Debug)]
pub struct ProfileHooks {
    /// The harness folders and the project state folder the access lists read.
    pub access: &'static AccessHooks,
    /// The agent git and the home entry removal the toolchains read.
    pub toolchain: &'static ToolchainHooks,
    /// The prefix every agent branch name starts with.
    pub linked_prefix: &'static str,
    /// The folder in a lead's checkout its teammates' clones sit in.
    pub teammates_folder: &'static str,
    /// Tells whether a branch name is an agent branch. Input: the name. Output: the answer.
    pub is_agent_branch: fn(&str) -> bool,
    /// The folders in an agent home where Claude Code keeps its bash state.
    #[cfg(unix)]
    pub claude_bash_preludes: &'static [&'static str],
    /// The instructions file the engine writes in an agent's project state folder.
    pub project_file: &'static str,
    /// The global instruction file Claude Code reads from its home.
    pub claude_instruction_file: &'static str,
    /// The global instruction file Codex reads from its home.
    pub codex_instruction_file: &'static str,
    /// The zsh startup files read from a home.
    pub zsh_startup: &'static [&'static str],
    /// The suffix of the compiled form zsh runs in place of a startup file.
    pub zsh_compiled: &'static str,
    /// The bash and sh startup files read from a home.
    pub sh_startup: &'static [&'static str],
    /// The agent home entries a launch walks and pins. Output: the paths, relative to the
    /// home.
    pub pinned_entries: fn() -> Vec<String>,
    /// The agent home entries a launch keeps. Output: the paths, relative to the home.
    pub kept_entries: fn() -> Vec<String>,
    /// The relay's part of a lead's mailbox.
    pub relay_lead_part: &'static str,
    /// The relay plugin folder the engine writes in a home before each launch.
    pub relay_plugin_folder: &'static str,
    /// The relay roster file in a lead's part of the mailbox.
    pub relay_roster_file: &'static str,
    /// The engine folder that holds an rcli master's configured SDK homes.
    pub configured_sdk_homes_folder: &'static str,
    /// The engine folder that holds the native profiles.
    pub native_profiles_folder: &'static str,
    /// The listed slot key of an account. Input: the account. Output: the key.
    #[cfg(unix)]
    pub listed_slot_key: fn(&str) -> String,
    /// Tells whether a folder holds no sign-in's record. Input: the folder. Output: the
    /// answer.
    #[cfg(unix)]
    pub holds_no_sign_in: fn(&Path) -> bool,
    /// Tells whether a path sits in the engine's own folder of a harness. Input: the path.
    /// Output: the answer.
    pub in_crazyagent_folder: fn(&Path) -> bool,
    /// The npm prefix of a node program. Input: the program. Output: the prefix, if any.
    pub node_prefix: fn(&Path) -> Option<PathBuf>,
    /// The engine's own folder in an agent's launch folder.
    pub launch_state: &'static str,
    /// The launch lock file in an agent home.
    pub launch_lock: &'static str,
    /// The folder of Codex's own skills in its home.
    pub codex_system_skills: &'static str,
    /// The engine's folder in a project.
    pub crazyagent_directory: &'static str,
    /// The path a team caller binds its socket at first. Input: the socket. Output: the
    /// path.
    #[cfg(unix)]
    pub binding_path: fn(&Path) -> PathBuf,
    /// The team calls folder in an agent's temp folder.
    pub mailbox_folder: &'static str,
    /// The suffix of a team call's reply.
    pub reply_suffix: &'static str,
    /// The suffix of a team call's reply while it is written.
    pub answer_part_suffix: &'static str,
    /// The engine folders that hold account and plugin keys.
    pub key_folders: &'static [&'static str],
    /// The engine runtime folders that are engine secrets as a whole.
    pub engine_secret_folders: &'static [&'static str],
}

#[cfg(all(test, target_os = "macos"))]
mod tests;
