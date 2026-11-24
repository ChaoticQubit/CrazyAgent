use crate::access::AccessHooks;
#[cfg(target_os = "windows")]
use crate::error::SandboxError;
use crate::profile::ProfileHooks;
use crate::toolchains::ToolchainHooks;
#[cfg(target_os = "windows")]
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Stands in for the owner's git: none. Input: the root. Output: no command.
fn no_owner_git(_root: &Path) -> Result<Option<Command>, ()> {
    Ok(None)
}

/// Stands in for the project state folder: one name under the root. Input: the root.
/// Output: the folder.
#[cfg(unix)]
fn stand_in_state(root: &Path) -> PathBuf {
    root.join(".stand-in-state")
}

/// Stands in for the project state folder as the Windows boundary reads it: the root's
/// `.crazyagent` while it is a real folder, else the root itself, so a project without one
/// keeps its root `context` guarded. Input: the root. Output: the folder.
#[cfg(target_os = "windows")]
fn stand_in_state(root: &Path) -> PathBuf {
    let inner = root.join(kernel_common::names::CRAZYAGENT_DIRECTORY);
    match std::fs::symlink_metadata(&inner) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => inner,
        _ => root.to_path_buf(),
    }
}

/// Stands in for the untracked file list: none. Input: the root. Output: an empty list.
fn nothing_untracked(_root: &Path) -> Option<Vec<String>> {
    Some(Vec::new())
}

/// Stands in for the home entry removal: done. Inputs: the folder, name and code.
/// Output: done.
#[cfg(unix)]
fn removed(_folder: BorrowedFd<'_>, _name: &str, _code: &'static str) -> Result<(), &'static str> {
    Ok(())
}

/// Stands in for the engine's git. Output: the system git.
#[cfg(unix)]
fn system_git() -> &'static Path {
    Path::new("/usr/bin/git")
}

/// Stands in for the teammates' home: one name under the home. Input: the home.
/// Output: the folder.
fn mates_home(home: &Path) -> PathBuf {
    home.join("stand-in-mates")
}

/// Stands in for the agent tree removal: done. Input: the folder. Output: done.
fn tree_removed(_folder: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn startup_removed(_home: &Path, _root: &Path) -> Result<(), &'static str> {
    Ok(())
}

/// Stands in for every name check: no. Input: the name. Output: false.
fn never(_name: &str) -> bool {
    false
}

/// Stands in for every path check: no. Input: the path. Output: false.
fn never_at(_path: &Path) -> bool {
    false
}

/// Stands in for the node prefix: none. Input: the program. Output: none.
fn no_node(_program: &Path) -> Option<PathBuf> {
    None
}

/// Stands in for the listed slot key: the account as given. Input: the account.
/// Output: the key.
#[cfg(unix)]
fn slot_key(account: &str) -> String {
    account.to_owned()
}

/// Stands in for the caller binding: one name under the home. Input: the home.
/// Output: the path.
#[cfg(unix)]
fn binding(home: &Path) -> PathBuf {
    home.join("stand-in-binding")
}

/// Stands in for the agent home's pinned entries, each folder ahead of what it holds: the
/// folders a launch keeps only part of, cargo's registry link and each config folder, as
/// the profile requires of every hooks value. Output: the paths below the home.
fn pinned() -> Vec<String> {
    ["Library", ".cargo", ".cargo/registry", ".config"]
        .map(str::to_owned)
        .to_vec()
}

/// Stands in for the agent home's entries a launch keeps whole: its state folder, lock and
/// relay folder, so the profile's flag and ACL deny names each. Output: the paths below the
/// home.
fn kept() -> Vec<String> {
    ["stand-in-state", "stand-in.lock", "stand-in-relay"]
        .map(str::to_owned)
        .to_vec()
}

/// Stands in for the engine's Windows agent command: the boundary helper beside the test
/// binary, as the engine's own hook builds it. Inputs: the installed profile, the program,
/// the base and the harness variables. Output: the command, or the refusal.
#[cfg(target_os = "windows")]
fn helper_command(
    profile: &Path,
    program: &Path,
    base: &[(&str, &OsStr)],
    harness: Vec<(&OsStr, Option<&OsStr>)>,
) -> Result<Command, SandboxError> {
    crate::windows::profile::agent_command(profile, program, base, harness)
}

/// Stands in for the engine's grants folder: one folder under the system temp folder, made
/// when missing, outside every row's own temp folder. Output: the folder, or the I/O error.
#[cfg(target_os = "windows")]
fn stand_in_grants() -> Result<PathBuf, SandboxError> {
    let grants = std::env::temp_dir().join("stand-in-grants");
    std::fs::create_dir_all(&grants)?;
    let grants = crate::paths::canonical(&grants)?;
    assert!(grants.is_dir());
    Ok(grants)
}

pub(crate) static ACCESS: AccessHooks = AccessHooks {
    owner_git: no_owner_git,
    harness_folders: Vec::new,
    state_folder: stand_in_state,
    untracked: nothing_untracked,
};

static TOOLCHAIN: ToolchainHooks = ToolchainHooks {
    #[cfg(unix)]
    remove_home_entry: removed,
    #[cfg(unix)]
    sandboxed_git: system_git,
    #[cfg(unix)]
    remove_shell_startup: startup_removed,
    mates_home,
    launch_state: "stand-in-state",
    remove_agent_tree: tree_removed,
};

pub(crate) static STAND_IN: ProfileHooks = ProfileHooks {
    access: &ACCESS,
    toolchain: &TOOLCHAIN,
    linked_prefix: "stand-in/",
    teammates_folder: ".stand-in-teammates",
    is_agent_branch: never,
    #[cfg(unix)]
    claude_bash_preludes: &["stand-in-snapshots"],
    project_file: "STAND-IN.md",
    claude_instruction_file: "STAND-IN-CLAUDE.md",
    codex_instruction_file: "STAND-IN-AGENTS.md",
    zsh_startup: &[".zshrc"],
    zsh_compiled: ".zwc",
    sh_startup: &[".profile"],
    pinned_entries: pinned,
    kept_entries: kept,
    relay_lead_part: "stand-in-lead",
    relay_plugin_folder: "stand-in-relay",
    relay_roster_file: "stand-in-roster",
    configured_sdk_homes_folder: "stand-in-sdk-homes",
    native_profiles_folder: "stand-in-profiles",
    #[cfg(unix)]
    listed_slot_key: slot_key,
    #[cfg(unix)]
    holds_no_sign_in: never_at,
    in_crazyagent_folder: never_at,
    node_prefix: no_node,
    launch_state: "stand-in-state",
    launch_lock: "stand-in.lock",
    codex_system_skills: ".bundle",
    crazyagent_directory: ".stand-in",
    #[cfg(unix)]
    binding_path: binding,
    mailbox_folder: "stand-in-mailbox",
    reply_suffix: ".stand-in-reply",
    answer_part_suffix: ".stand-in-part",
    key_folders: &["stand-in-keys"],
    engine_secret_folders: &["stand-in-secrets"],
    #[cfg(target_os = "windows")]
    windows_agent_command: helper_command,
    #[cfg(target_os = "windows")]
    grants_folder: stand_in_grants,
};
