use super::{ProfileHooks, ProjectSandbox};
use crate::access::AccessHooks;
use crate::net::{SandboxNetwork, SandboxRemote};
use crate::paths::{any_case, canonical};
use crate::ports::REMOTES_MAX;
use crate::profile::seatbelt::sbpl_overlong;
use crate::toolchains::ToolchainHooks;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Stands in for the owner's git: none. Input: the root. Output: no command.
fn no_owner_git(_root: &Path) -> Result<Option<Command>, ()> {
    Ok(None)
}

/// Stands in for the project state folder: one name under the root. Input: the root.
/// Output: the folder.
fn stand_in_state(root: &Path) -> PathBuf {
    root.join(".stand-in-state")
}

/// Stands in for the untracked file list: none. Input: the root. Output: an empty list.
fn nothing_untracked(_root: &Path) -> Option<Vec<String>> {
    Some(Vec::new())
}

/// Stands in for the home entry removal: done. Inputs: the folder, name and code.
/// Output: done.
fn removed(_folder: BorrowedFd<'_>, _name: &str, _code: &'static str) -> Result<(), &'static str> {
    Ok(())
}

/// Stands in for the engine's git. Output: the system git.
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
fn slot_key(account: &str) -> String {
    account.to_owned()
}

/// Stands in for the caller binding: one name under the home. Input: the home.
/// Output: the path.
fn binding(home: &Path) -> PathBuf {
    home.join("stand-in-binding")
}

static ACCESS: AccessHooks = AccessHooks {
    owner_git: no_owner_git,
    harness_folders: Vec::new,
    state_folder: stand_in_state,
    untracked: nothing_untracked,
};

static TOOLCHAIN: ToolchainHooks = ToolchainHooks {
    remove_home_entry: removed,
    sandboxed_git: system_git,
    mates_home,
    launch_state: "stand-in-state",
    remove_agent_tree: tree_removed,
};

static STAND_IN: ProfileHooks = ProfileHooks {
    access: &ACCESS,
    toolchain: &TOOLCHAIN,
    linked_prefix: "stand-in/",
    teammates_folder: ".stand-in-teammates",
    is_agent_branch: never,
    claude_bash_preludes: &["stand-in-snapshots"],
    project_file: "STAND-IN.md",
    claude_instruction_file: "STAND-IN-CLAUDE.md",
    codex_instruction_file: "STAND-IN-AGENTS.md",
    zsh_startup: &[".zshrc"],
    zsh_compiled: ".zwc",
    sh_startup: &[".profile"],
    pinned_entries: Vec::new,
    kept_entries: Vec::new,
    relay_lead_part: "stand-in-lead",
    relay_plugin_folder: "stand-in-relay",
    relay_roster_file: "stand-in-roster",
    configured_sdk_homes_folder: "stand-in-sdk-homes",
    native_profiles_folder: "stand-in-profiles",
    listed_slot_key: slot_key,
    holds_no_sign_in: never_at,
    in_crazyagent_folder: never_at,
    node_prefix: no_node,
    launch_state: "stand-in-state",
    launch_lock: "stand-in.lock",
    codex_system_skills: "stand-in-skills",
    crazyagent_directory: ".stand-in",
    binding_path: binding,
    mailbox_folder: "stand-in-mailbox",
    reply_suffix: ".stand-in-reply",
    answer_part_suffix: ".stand-in-part",
    key_folders: &["stand-in-keys"],
    engine_secret_folders: &["stand-in-secrets"],
};

/// Makes fresh project, home and temp folders. Output: their guard and the three canonical
/// folders.
fn folders() -> (tempfile::TempDir, [PathBuf; 3]) {
    let scratch = tempfile::tempdir().unwrap();
    let root = canonical(scratch.path()).unwrap();
    let made = ["project", "home", "tmp"].map(|name| root.join(name));
    for folder in &made {
        std::fs::create_dir_all(folder).unwrap();
    }
    (scratch, made)
}

/// A sandbox the kernel builds on stand-in hooks writes a profile that denies by default,
/// keeps writes off the launch lock those hooks name and parses.
#[test]
fn a_sandbox_on_stand_in_hooks_denies_their_launch_lock_and_parses() {
    let (_scratch, [project, home, tmp]) = folders();
    let sandbox =
        ProjectSandbox::new(&STAND_IN, &project, &home, &tmp, SandboxNetwork::Open).unwrap();
    let profile = sandbox.profile();
    assert!(profile.starts_with("(version 1)\n(deny default)\n"));
    assert!(profile.contains(&any_case("stand-in.lock")), "{profile}");
    assert!(sbpl_overlong(&profile).is_empty());
}

/// A sandbox the kernel builds refuses a remote list past its bound, whatever the hooks.
#[test]
fn a_sandbox_on_stand_in_hooks_refuses_a_remote_list_past_its_bound() {
    let (_scratch, [project, home, tmp]) = folders();
    let remotes = vec![SandboxRemote::AnyHost(443); REMOTES_MAX + 1];
    assert!(
        ProjectSandbox::new(
            &STAND_IN,
            &project,
            &home,
            &tmp,
            SandboxNetwork::Only(remotes)
        )
        .is_err()
    );
}
