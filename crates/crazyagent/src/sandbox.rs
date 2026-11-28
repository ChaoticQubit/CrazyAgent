use kernel_sandbox::access::AccessHooks;
#[cfg(target_os = "windows")]
use kernel_sandbox::error::SandboxError;
use kernel_sandbox::grants::GrantHooks;
#[cfg(test)]
use kernel_sandbox::net::SandboxNetwork;
#[cfg(all(test, unix))]
use kernel_sandbox::net::SandboxRemote;
use kernel_sandbox::policy::PolicyHooks;
use kernel_sandbox::profile::ProfileHooks;
#[cfg(all(test, unix))]
use kernel_sandbox::profile::linux_holds_fully;
#[cfg(test)]
use kernel_sandbox::profile::seatbelt::credential_places;
#[cfg(all(test, unix))]
use kernel_sandbox::settings::LinuxSockets;
#[cfg(target_os = "macos")]
use kernel_sandbox::tool_cache::ToolCacheHooks;
use kernel_sandbox::toolchains::ToolchainHooks;
#[cfg(target_os = "windows")]
use std::ffi::OsStr;
#[cfg(any(all(test, unix), target_os = "windows"))]
use std::fs;
use std::path::Path;
#[cfg(target_os = "windows")]
use std::path::PathBuf;
#[cfg(target_os = "windows")]
use std::process::Command;

pub mod access_store;
#[cfg(all(test, target_os = "macos"))]
mod git_deny_tests;
pub mod grants;
pub mod grants_store;
pub mod ide_shell;
pub mod policy_store;
#[cfg(all(test, target_os = "macos"))]
mod profile_engine_tests;
pub mod shared_folders;
#[cfg(target_os = "macos")]
pub mod tool_cache;
#[cfg(all(test, unix))]
mod toolchains_tests;
#[cfg(target_os = "windows")]
use kernel_sandbox::windows::{GRANTS_FOLDER, profile as windows};
/// What the share-stop rows share on the test farm: the shares and the runner user that
/// `.github/farm/unc-share-setup.ps1` sets up, the server session that is the measure (P1),
/// and the planted shapes that point git at those shares (V244-WIN-GIT-UNC-SIBLINGS N1).
#[cfg(all(test, target_os = "windows"))]
pub(crate) mod windows_farm;
#[cfg(all(test, target_os = "windows"))]
mod windows_tests;

/// The loopback ports the owner's configured model endpoints listen on, which a tools-on
/// agent must not bind before the owner's model server does. An endpoint URL that does not
/// parse names no port. Input: the model access settings. Output: the ports, sorted, once
/// each.
pub fn owner_ports(access: &crate::config::ModelAccess) -> Vec<u16> {
    let mut ports: Vec<u16> = access
        .connections
        .iter()
        .filter_map(|connection| match &connection.endpoint {
            crate::config::ModelEndpoint::Http { url } => url::Url::parse(url).ok(),
            crate::config::ModelEndpoint::Native {} => None,
        })
        .filter(|url| match url.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => false,
        })
        .filter_map(|url| url.port_or_known_default())
        .collect();
    ports.sort_unstable();
    ports.dedup();
    assert!(ports.len() <= access.connections.len());
    ports
}

/// Why a tools-on launch does not start: the engine's settings files exist but do not read,
/// so the owner's configured local model ports are known nowhere (OWNER-PORTS-FAIL-OPEN).
pub const OWNER_PORTS_UNREADABLE: &str = "owner_ports_settings_unreadable";

/// The owner's configured local model ports, read from the engine's two settings files at
/// each tools-on launch, as [`owner_ports`] lists them; none when no file exists. A file that
/// does not read fails closed and logs only the code, since its error text can carry the
/// file's lines. Inputs: the global and project settings files. Output: the ports, or
/// [`OWNER_PORTS_UNREADABLE`].
pub fn settings_owner_ports(global: &Path, project: &Path) -> Result<Vec<u16>, &'static str> {
    let Ok(config) = crate::config::resolve_config(global, project) else {
        eprintln!("{OWNER_PORTS_UNREADABLE}");
        return Err(OWNER_PORTS_UNREADABLE);
    };
    Ok(config
        .settings
        .model_access
        .map_or_else(Vec::new, |access| owner_ports(&access)))
}

const ENGINE_SECRET_FOLDERS: &[&str] = &[
    crate::terminal::FOLDER,
    crate::ide::FOLDER,
    crate::engine::bootstrap::NATIVE_PROFILES_FOLDER,
    crate::engine::bootstrap::CONFIGURED_SDK_HOMES_FOLDER,
    #[cfg(unix)]
    kernel_storage::migrations::BACKUPS,
];

/// Builds the one command every Windows agent process starts from: the boundary helper next
/// to the engine's executable with `--profile <profile> -- <program>`, and a cleared
/// environment holding exactly [`agent_process_env`]. Inputs: the installed boundary profile, the
/// absolute program, the base variables and the harness's variables (None removes one).
/// Output: the command, `Unavailable` without the helper, or `Invalid` for an unsafe input.
#[cfg(target_os = "windows")]
pub fn agent_command<'a>(
    profile: &Path,
    program: &Path,
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Command, SandboxError> {
    windows::agent_command(profile, program, base, harness)
}

/// [`agent_command`] for the profile hooks, which take the harness variables as a list.
/// Inputs and output as [`agent_command`].
#[cfg(target_os = "windows")]
fn windows_agent_command_hook(
    profile: &Path,
    program: &Path,
    base: &[(&str, &OsStr)],
    harness: Vec<(&OsStr, Option<&OsStr>)>,
) -> Result<Command, SandboxError> {
    windows::agent_command(profile, program, base, harness)
}

/// The engine's grants folder, [`kernel_sandbox::windows::GRANTS_FOLDER`] in its data root,
/// made when missing. Output: the folder, `Unavailable` when the data root is unknown, or the
/// I/O error.
#[cfg(target_os = "windows")]
pub(super) fn grants_folder() -> Result<PathBuf, SandboxError> {
    let layout =
        crate::paths::Layout::current().map_err(|error| SandboxError::Unavailable(error.0))?;
    let grants = layout.root().join(GRANTS_FOLDER);
    fs::create_dir_all(&grants)?;
    assert!(grants.is_dir());
    Ok(grants)
}

/// The engine's real agent policy hooks, the only [`PolicyHooks`] production code passes:
/// a project's local folders ([`access_local_folders`]).
pub const POLICY_HOOKS: PolicyHooks = PolicyHooks {
    local_folders: access_local_folders,
};

/// A project's top-level folders the owner may pick for Chosen folders, as
/// [`kernel_sandbox::shared_folders::local_folders`] reads them with the engine's [`ACCESS_HOOKS`].
/// Input: the project folder. Output: the names.
fn access_local_folders(root: &Path) -> Vec<String> {
    kernel_sandbox::shared_folders::local_folders(&ACCESS_HOOKS, root)
}

/// The engine's real project access hooks, the only [`AccessHooks`] production code passes:
/// the owner's git for a project folder ([`access_owner_git`]), the harness folders
/// ([`harness_folders`]), a project's state folder and the paths git leaves untracked
/// ([`access_untracked`]).
pub const ACCESS_HOOKS: AccessHooks = AccessHooks {
    owner_git: access_owner_git,
    harness_folders,
    state_folder: crate::projects_home::state_folder,
    untracked: access_untracked,
};

/// The refusal code of a launch whose engine state folder has no parent, so there is no
/// engine root to guard its sandbox against.
pub(crate) const ENGINE_ROOT_MISSING: &str = "engine_root_missing";

/// A team launch's line for [`ENGINE_ROOT_MISSING`]: the plain sandbox line with that code.
pub(crate) const NO_ENGINE_ROOT: &str =
    "the agent sandbox could not be set up. (engine_root_missing)";

/// A research launch's refusal for [`ENGINE_ROOT_MISSING`], as its setup step's code.
pub(crate) const NO_RESEARCH_ENGINE_ROOT: crate::research::runner::RunError =
    crate::research::runner::RunError::Setup(ENGINE_ROOT_MISSING);

/// The engine's real profile hooks, the only [`ProfileHooks`] production code passes: its
/// access and toolchain hooks, and the engine's own folder and file names, agent home lists
/// and checks the Seatbelt rules name.
pub const PROFILE_HOOKS: ProfileHooks = ProfileHooks {
    access: &ACCESS_HOOKS,
    toolchain: &TOOLCHAIN_HOOKS,
    linked_prefix: crate::agent_git::LINKED_PREFIX,
    teammates_folder: crate::agent_git::TEAMMATES_FOLDER,
    is_agent_branch: crate::agent_git::is_agent_branch,
    #[cfg(unix)]
    claude_bash_preludes: &crate::agent_home::CLAUDE_BASH_PRELUDES,
    project_file: crate::agent_home::PROJECT_FILE,
    claude_instruction_file: crate::agent_home::Harness::Claude.instruction_file(),
    codex_instruction_file: crate::agent_home::Harness::Codex.instruction_file(),
    zsh_startup: &crate::agent_home::ZSH_STARTUP,
    zsh_compiled: crate::agent_home::ZSH_COMPILED,
    sh_startup: &crate::agent_home::SH_STARTUP,
    pinned_entries: crate::agent_home::pinned_entries,
    kept_entries: crate::agent_home::kept_entries,
    relay_lead_part: crate::coord::relay::LEAD_PART,
    relay_plugin_folder: crate::coord::relay::PLUGIN_FOLDER,
    relay_roster_file: crate::coord::relay::ROSTER_FILE,
    configured_sdk_homes_folder: crate::engine::bootstrap::CONFIGURED_SDK_HOMES_FOLDER,
    native_profiles_folder: crate::engine::bootstrap::NATIVE_PROFILES_FOLDER,
    #[cfg(unix)]
    listed_slot_key: crate::engine::bootstrap::listed_slot_key,
    #[cfg(unix)]
    holds_no_sign_in: profile_holds_no_sign_in,
    in_crazyagent_folder: crate::harnesses::in_crazyagent_folder,
    node_prefix: crate::harnesses::node_prefix,
    launch_state: crate::launch_context::STATE,
    launch_lock: crate::launch_context::LOCK,
    codex_system_skills: crate::launch_context::CODEX_SYSTEM_SKILLS,
    crazyagent_directory: kernel_common::names::CRAZYAGENT_DIRECTORY,
    #[cfg(unix)]
    binding_path: crate::team::caller::binding_path,
    mailbox_folder: crate::team::tool::MAILBOX_FOLDER,
    reply_suffix: crate::team::tool::REPLY_SUFFIX,
    answer_part_suffix: crate::team::tool::ANSWER_PART_SUFFIX,
    key_folders: &crate::storage::accounts::KEY_FOLDERS,
    engine_secret_folders: ENGINE_SECRET_FOLDERS,
    plain_record_folders: &[
        crate::agent_home::SKILLS_FOLDER,
        crate::agent_home::FILES_FOLDER,
        crate::agent_home::GIVEN_FOLDER,
        crate::agent_home::LEVELS_FOLDER,
        crate::agent_home::WRITTEN_FOLDER,
    ],
    #[cfg(target_os = "windows")]
    windows_agent_command: windows_agent_command_hook,
    #[cfg(target_os = "windows")]
    grants_folder,
};

/// Tells whether a folder holds no sign-in's record, as a listed master session's slot home
/// must. Input: the folder. Output: the answer.
#[cfg(unix)]
fn profile_holds_no_sign_in(folder: &Path) -> bool {
    crate::harnesses::launch::holds_no_sign_in(folder).is_ok()
}

/// The engine's real grant hooks, the only [`GrantHooks`] production code passes: the
/// harness folders ([`harness_folders`]) and the IDE ports the engine recorded
/// ([`grants::ide_ports`]).
pub const GRANT_HOOKS: GrantHooks = GrantHooks {
    harness_folders,
    ide_ports: grants::ide_ports,
};

/// The engine's real agent toolchain hooks, the only [`ToolchainHooks`] production code
/// passes: the cargo home entry removal of [`toolchain_remove_home_entry`], the real git
/// the research runner resolved, the shell startup removal of
/// [`toolchain_remove_shell_startup`], a native lead's teammates' home, the launch folder's
/// engine folder name and the agent tree removal of [`toolchain_remove_agent_tree`].
pub const TOOLCHAIN_HOOKS: ToolchainHooks = ToolchainHooks {
    #[cfg(unix)]
    remove_home_entry: toolchain_remove_home_entry,
    #[cfg(unix)]
    sandboxed_git: crate::research::runner::sandboxed_git,
    #[cfg(unix)]
    remove_shell_startup: toolchain_remove_shell_startup,
    mates_home: crate::launch_context::mates_home,
    launch_state: crate::launch_context::STATE,
    remove_agent_tree: toolchain_remove_agent_tree,
};

/// The tool cache hooks the engine passes: the pinning flag clearing of
/// [`tool_cache_unpin`], the by-handle entry removal of [`tool_cache_remove_agent_entry`] and
/// the engine's priming process.
#[cfg(target_os = "macos")]
pub const TOOL_CACHE_HOOKS: ToolCacheHooks = ToolCacheHooks {
    unpin: tool_cache_unpin,
    remove_agent_entry: tool_cache_remove_agent_entry,
    priming_command: tool_cache::priming_command,
};

/// Clears the pinning flags an agent may set on an entry as Clean up does. Inputs: the open
/// parent and the entry's name. Output: none.
#[cfg(target_os = "macos")]
fn tool_cache_unpin(parent: std::os::fd::BorrowedFd<'_>, name: &str) {
    crate::team::cleanup::unpin(parent, name);
}

/// Removes one entry of an agent's tool cache as [`toolchain_remove_home_entry`] does, but
/// keeps the code for a folder too deep to remove, which Clean up names to the owner.
/// Inputs: the open cache folder, the entry name and the fallback code. Output: done, or the
/// refusal code.
#[cfg(target_os = "macos")]
fn tool_cache_remove_agent_entry(
    folder: std::os::fd::BorrowedFd<'_>,
    name: &str,
    otherwise: &'static str,
) -> Result<(), &'static str> {
    use crate::team::cleanup::{FOLDER_TOO_DEEP, home_code, remove_home_entry};
    remove_home_entry(folder, name).map_err(|problem| {
        if problem.0 == FOLDER_TOO_DEEP {
            FOLDER_TOO_DEEP
        } else {
            home_code(&problem, otherwise)
        }
    })
}

/// The absolute tool cache folder a removal of an agent's caches refused as too deep to
/// remove. Input: the removal's error. Output: the folder, or None for any other refusal or
/// a folder whose path is not text.
#[cfg(target_os = "macos")]
pub(crate) fn too_deep_folder(problem: &std::io::Error) -> Option<String> {
    let refused = problem
        .get_ref()?
        .downcast_ref::<kernel_sandbox::tool_cache::EntryRefused>()?;
    if refused.code != crate::team::cleanup::FOLDER_TOO_DEEP || !refused.folder.is_absolute() {
        return None;
    }
    refused.folder.to_str().map(str::to_owned)
}

/// Removes a folder an agent wrote as Clean up removes an agent's tree, with the engine's
/// refusal text as the error. Input: the folder. Output: done, or the error.
fn toolchain_remove_agent_tree(folder: &Path) -> std::io::Result<()> {
    crate::team::cleanup::remove_agent_tree(folder).map_err(|error| std::io::Error::other(error.0))
}

/// Removes a kept home's shell startup files as the engine's home cleanup does
/// ([`crate::agent_home::remove_shell_startup`]). Inputs: the home and the engine data
/// root, both canonical. Output: done, or `agent_home_unwritable` when one stays.
#[cfg(unix)]
fn toolchain_remove_shell_startup(home: &Path, root: &Path) -> Result<(), &'static str> {
    crate::agent_home::remove_shell_startup(home, root).map_err(|_| "agent_home_unwritable")
}

/// Removes one entry of an agent's own cargo home as the engine's home cleanup does, with
/// its walk budget code kept and every other refusal given as `otherwise`. Inputs: the open
/// cargo home, the entry name and the fallback code. Output: done, or the refusal code.
#[cfg(unix)]
fn toolchain_remove_home_entry(
    folder: std::os::fd::BorrowedFd<'_>,
    name: &str,
    otherwise: &'static str,
) -> Result<(), &'static str> {
    crate::team::cleanup::remove_home_entry(folder, name)
        .map_err(|problem| crate::team::cleanup::home_code(&problem, otherwise))
}

/// The paths git leaves untracked in the project folder `root`, as the shared folders plan
/// reads them: what the onboarding draft lists on Unix, none elsewhere. Input: the folder.
/// Output: the paths, or `None` when they cannot be listed.
#[cfg(unix)]
fn access_untracked(root: &Path) -> Option<Vec<String>> {
    crate::onboarding::draft::untracked(root)
}

/// The paths git leaves untracked in the project folder, which no system but Unix lists for
/// the shared folders plan. Input: the folder. Output: always `None`.
#[cfg(not(unix))]
fn access_untracked(_root: &Path) -> Option<Vec<String>> {
    None
}

/// The owner's git for the project folder `root` as the access lists run it, with the
/// engine's own git when it has one. Input: the canonical project folder. Output: the
/// command, `Ok(None)` when the folder holds no repository, or `Err(())` when the owner's
/// git is refused or unavailable.
fn access_owner_git(root: &Path) -> Result<Option<std::process::Command>, ()> {
    use crate::project_files::NoOwnerGit;
    match crate::project_files::owner_git(root, crate::project_files::engine_git()) {
        Ok(command) => Ok(Some(command)),
        Err(NoOwnerGit::Absent) => Ok(None),
        Err(NoOwnerGit::Refused | NoOwnerGit::Unavailable) => Err(()),
    }
}

/// Makes a scratch folder the engine owns for one run: a new private folder `name` in
/// `folder`, which must sit in the engine folder
/// ([`kernel_sandbox::engine_scratch::EngineScratch::make`]). Inputs: the engine folder,
/// the folder in it and the new name. Output: the scratch, or `Invalid` when the folder is
/// not in the engine folder or the make refuses.
pub(crate) fn engine_scratch(
    engine: &Path,
    folder: &Path,
    name: &str,
) -> Result<kernel_sandbox::engine_scratch::EngineScratch, kernel_sandbox::error::SandboxError> {
    let engine = kernel_sandbox::paths::canonical(engine)?;
    let folder = kernel_sandbox::paths::canonical(folder)?;
    let parts = folder
        .strip_prefix(&engine)
        .ok()
        .and_then(|inside| {
            inside
                .iter()
                .map(|part| part.to_str())
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| {
            kernel_sandbox::error::invalid("engine scratch parent is not in the engine folder")
        })?;
    kernel_sandbox::engine_scratch::EngineScratch::make(&PROFILE_HOOKS, &engine, &parts, name)
}

/// The folders and files in the owner's home that belong to a harness, as every harness
/// manifest names them, in catalog order: agents never read them and no project or sandbox
/// root may sit inside one. Takes nothing. Returns the names, relative to the home.
pub(crate) fn harness_folders() -> Vec<&'static str> {
    crate::harnesses::registry::registry()
        .all()
        .iter()
        .flat_map(|harness| harness.owner_folders().iter().map(String::as_str))
        .collect()
}

#[cfg(test)]
mod harness_folder_tests {
    use super::credential_places;
    use crate::harnesses::registry::registry;

    #[test]
    fn the_owner_secret_list_holds_every_harness_folder_where_the_three_stood_before() {
        let places: Vec<&str> = credential_places(&crate::sandbox::PROFILE_HOOKS).collect();
        let from = places.iter().position(|place| *place == ".kube").unwrap() + 1;
        let to = places
            .iter()
            .position(|place| *place == ".cargo/credentials")
            .unwrap();
        let listed: Vec<&str> = registry()
            .all()
            .iter()
            .flat_map(|harness| harness.owner_folders().iter().map(String::as_str))
            .collect();
        assert_eq!(
            places[from..from + 3],
            [".codex", ".claude", ".claude.json"]
        );
        assert_eq!(places[from..to], listed);
    }
}

#[cfg(all(test, unix))]
mod linux_sockets_tests;
