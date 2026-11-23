use kernel_sandbox::access::AccessHooks;
use kernel_sandbox::error::SandboxError;
#[cfg(target_os = "windows")]
use kernel_sandbox::error::invalid;
use kernel_sandbox::grants::GrantHooks;
use kernel_sandbox::host::LinuxHost;
use kernel_sandbox::lines::*;
#[cfg(test)]
use kernel_sandbox::net::SandboxNetwork;
#[cfg(all(test, unix))]
use kernel_sandbox::net::SandboxRemote;
#[cfg(target_os = "windows")]
use kernel_sandbox::paths::clean_absolute;
#[cfg(all(test, target_os = "macos"))]
use kernel_sandbox::paths::{any_case, fold_case};
use kernel_sandbox::policy::PolicyHooks;
#[cfg(all(test, target_os = "macos"))]
use kernel_sandbox::ports::REMOTES_MAX;
use kernel_sandbox::profile::command::*;
use kernel_sandbox::profile::consts::*;
use kernel_sandbox::profile::env::*;
#[cfg(all(test, unix))]
use kernel_sandbox::profile::linux_holds_fully;
#[cfg(test)]
use kernel_sandbox::profile::seatbelt::credential_places;
#[cfg(all(test, target_os = "macos"))]
use kernel_sandbox::profile::seatbelt::*;
use kernel_sandbox::profile::{ProfileHooks, ProjectSandbox, linux_unbridged_notice_on};
#[cfg(all(test, unix))]
use kernel_sandbox::settings::LinuxSockets;
use kernel_sandbox::settings::{LinuxStrict, LinuxSupervisor};
#[cfg(all(test, target_os = "macos"))]
use kernel_sandbox::shared_folders::OPEN_TOPS_MAX;
#[cfg(target_os = "macos")]
use kernel_sandbox::tool_cache::ToolCacheHooks;
use kernel_sandbox::toolchains::ToolchainHooks;
#[cfg(target_os = "windows")]
use std::ffi::OsStr;
#[cfg(all(test, unix))]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub mod access_store;
#[cfg(all(test, target_os = "macos"))]
mod git_deny_tests;
pub mod grants;
pub mod grants_store;
pub mod ide_shell;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod policy_store;
pub mod shared_folders;
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod test_agent;
#[cfg(target_os = "macos")]
pub mod tool_cache;
#[cfg(all(test, unix))]
mod toolchains_tests;
pub mod warden;
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
pub use windows::owner_git::OwnerGit;

/// Owner git's own container, which only Windows has (V244-WIN-GIT-UNC-SIBLINGS): elsewhere
/// this type has no value, so a publish or `@` list there never routes its git anywhere and
/// runs it as the owner, as before.
#[cfg(not(target_os = "windows"))]
#[derive(Debug)]
pub enum OwnerGit {}

#[cfg(not(target_os = "windows"))]
impl OwnerGit {
    /// Never runs: no value of this type exists outside Windows. Input: the command.
    /// Output: none.
    pub fn wrap(&self, _git: &Command) -> Result<Command, SandboxError> {
        match *self {}
    }

    /// Never runs: no value of this type exists outside Windows. Output: none.
    pub fn tmp(&self) -> &Path {
        match *self {}
    }

    /// Never runs: no value of this type exists outside Windows. Output: none.
    pub fn git(&self) -> &Path {
        match *self {}
    }
}

/// How the tmux host wraps a confined pane, built from this module's sandbox values: the
/// runner, the keeper and pane shim words and variables, and which variables reach a pane.
/// Output: the value the kernel tmux host is opened with.
#[cfg(unix)]
pub fn confinement() -> kernel_processes::terminal::tmux::Confinement {
    kernel_processes::terminal::tmux::Confinement {
        sandbox_exec: SANDBOX_EXEC,
        #[cfg(target_os = "macos")]
        keeper: KEEPER,
        #[cfg(target_os = "macos")]
        keeper_deadline_variable: KEEPER_DEADLINE,
        #[cfg(target_os = "macos")]
        keeper_state_variable: KEEPER_STATE,
        #[cfg(target_os = "macos")]
        pane_shim: PANE_SHIM,
        variable_allowed: agent_variable_allowed,
        #[cfg(target_os = "macos")]
        unkept_pane,
    }
}

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

/// The code a launch refusal has of its own, so a caller shows its plain line: the strict
/// setting's ([`LINUX_NOT_FULL`]), the Windows loopback's
/// ([`MODEL_LOOPBACK_UNAVAILABLE_WINDOWS`]) and, on Linux, the hidden folders' three
/// ([`LINUX_HIDDEN_NEEDS_NAMESPACES`], [`LINUX_HIDDEN_MOUNTED_AGAIN`],
/// [`LINUX_HIDDEN_MOUNTS_NOT_PRIVATE`]). Input: the refusal. Output: the code, or none.
pub fn refusal_code(error: &SandboxError) -> Option<&'static str> {
    let SandboxError::Unavailable(reason) = error else {
        return None;
    };
    match reason.as_str() {
        LINUX_NOT_FULL_LINE => Some(LINUX_NOT_FULL),
        MODEL_LOOPBACK_WINDOWS_LINE => Some(MODEL_LOOPBACK_UNAVAILABLE_WINDOWS),
        #[cfg(target_os = "linux")]
        linux::HIDDEN_NEEDS_NAMESPACES => Some(LINUX_HIDDEN_NEEDS_NAMESPACES),
        #[cfg(target_os = "linux")]
        linux::HIDDEN_MOUNTED_AGAIN => Some(LINUX_HIDDEN_MOUNTED_AGAIN),
        #[cfg(target_os = "linux")]
        linux::HIDDEN_MOUNTS_NOT_PRIVATE => Some(LINUX_HIDDEN_MOUNTS_NOT_PRIVATE),
        _ => None,
    }
}

/// [`linux_unbridged_notice_on`] for this computer as it was last read, under the owner's
/// call supervisor setting ([`LinuxHost::under`]). Inputs: the project's network choice,
/// the agent's sandbox as launched and the setting, none when it cannot be read, which
/// counts as no supervisor. Output: the sentences, or none.
pub fn linux_unbridged_notice(
    choice: kernel_sandbox::policy::AgentNetwork,
    sandbox: &ProjectSandbox,
    calls: Option<LinuxSupervisor>,
) -> Option<String> {
    let host = linux_host_kept().map(|host| host.under(calls));
    linux_unbridged_notice_on(host, choice, sandbox)
}

/// What this computer holds as [`LinuxHost`] says, read through the sandbox helper its
/// launches take, again once what is kept is a minute old. No input. Output: none on
/// another system; on Linux the facts, each false when it cannot be read, so its gap line
/// is shown.
pub fn linux_host() -> Option<LinuxHost> {
    #[cfg(target_os = "linux")]
    {
        Some(linux::host_now())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// What this computer holds as [`linux_host`] last read it, however long ago, so a caller
/// inside a database transaction starts no helper; it reads only when nothing was read
/// yet. No input. Output: as [`linux_host`].
pub fn linux_host_kept() -> Option<LinuxHost> {
    #[cfg(target_os = "linux")]
    {
        Some(linux::host_kept())
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// The engine's real agent policy hooks, the only [`PolicyHooks`] production code passes:
/// this Linux read again ([`linux_host`]) and as last kept ([`linux_host_kept`]), the
/// bounded read for another repository ([`linux_other_git`]) and a project's local folders
/// ([`access_local_folders`]).
pub const POLICY_HOOKS: PolicyHooks = PolicyHooks {
    linux_host,
    linux_host_kept,
    linux_other_git,
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
    #[cfg(target_os = "windows")]
    windows_agent_command: windows_agent_command_hook,
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
/// the research runner resolved, a native lead's teammates' home, the launch folder's
/// engine folder name and the agent tree removal of [`toolchain_remove_agent_tree`].
pub const TOOLCHAIN_HOOKS: ToolchainHooks = ToolchainHooks {
    #[cfg(unix)]
    remove_home_entry: toolchain_remove_home_entry,
    #[cfg(unix)]
    sandboxed_git: crate::research::runner::sandboxed_git,
    mates_home: crate::launch_context::mates_home,
    launch_state: crate::launch_context::STATE,
    remove_agent_tree: toolchain_remove_agent_tree,
};

/// The tool cache hooks the engine passes: the pinning flag clearing of
/// [`tool_cache_unpin`], the agent tree removal of [`toolchain_remove_agent_tree`] and the
/// engine's priming process.
#[cfg(target_os = "macos")]
pub const TOOL_CACHE_HOOKS: ToolCacheHooks = ToolCacheHooks {
    unpin: tool_cache_unpin,
    remove_agent_tree: toolchain_remove_agent_tree,
    priming_command: tool_cache::priming_command,
};

/// Clears the pinning flags an agent may set on an entry as Clean up does. Inputs: the open
/// parent and the entry's name. Output: none.
#[cfg(target_os = "macos")]
fn tool_cache_unpin(parent: std::os::fd::BorrowedFd<'_>, name: &str) {
    crate::team::cleanup::unpin(parent, name);
}

/// Removes a folder an agent wrote as Clean up removes an agent's tree, with the engine's
/// refusal text as the error. Input: the folder. Output: done, or the error.
fn toolchain_remove_agent_tree(folder: &Path) -> std::io::Result<()> {
    crate::team::cleanup::remove_agent_tree(folder).map_err(|error| std::io::Error::other(error.0))
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

/// Tells whether a project may hold a git repository besides its own top-level `.git`,
/// which an agent may still change under the call supervisor: on Linux what a bounded read
/// of the folder finds; elsewhere yes, where no Linux line shows. Input: the project
/// folder. Output: true when one may be there.
pub fn linux_other_git(project: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::holds_other_git(project)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = project;
        true
    }
}

/// A project sandbox whose profile is already installed on disk. Launch paths take
/// this instead of a bare command, so no agent process can start outside its
/// boundary: Seatbelt on macOS, Landlock or bubblewrap on Linux, AppContainer with a job
/// object on Windows.
#[derive(Debug)]
pub struct InstalledSandbox {
    sandbox: ProjectSandbox,
    profile: PathBuf,
    unconfined: bool,
    calls: LinuxSupervisor,
    #[cfg(target_os = "linux")]
    linux: linux::LinuxSandbox,
    #[cfg(target_os = "windows")]
    windows: windows::WindowsSandbox,
}

impl InstalledSandbox {
    /// Installs the sandbox's profile at `profile` (see [`ProjectSandbox::install`]); on
    /// Linux, picks the Landlock helper next to the engine's executable or bubblewrap; on
    /// Windows, checks the helper next to the engine's executable first, then writes its
    /// boundary profile with the engine's grants folder (see
    /// [`windows::WindowsSandbox::install`]). `calls` and `strict` are the owner's "Linux call
    /// supervisor" and [`LinuxStrict`] choices, which every launch must pass and only Linux
    /// acts on. Output: the sandbox bound to that profile, the install refusal, or
    /// `Unavailable` where this system has no boundary.
    pub fn install(
        sandbox: ProjectSandbox,
        profile: &Path,
        calls: LinuxSupervisor,
        strict: LinuxStrict,
    ) -> Result<Self, SandboxError> {
        #[cfg(not(target_os = "linux"))]
        let _ = strict;
        if HIDDEN_UNHELD && !sandbox.hidden().is_empty() {
            return Err(SandboxError::Unavailable(HIDDEN_NEEDS_SEATBELT.into()));
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if !sandbox.kept_out().is_empty() || sandbox.open_tops().is_some() {
            return Err(SandboxError::Unavailable(HIDDEN_NEEDS_SEATBELT.into()));
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if !sandbox.shared_writes().is_empty() {
            return Err(SandboxError::Unavailable(
                SHARED_WRITES_NEED_SEATBELT.into(),
            ));
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if sandbox
            .agent_files()
            .is_some_and(|rule| rule.choice != kernel_sandbox::policy::IdeAgentFiles::AllowAll)
        {
            return Err(SandboxError::Unavailable(AGENT_FILES_NEED_SEATBELT.into()));
        }
        #[cfg(target_os = "windows")]
        {
            let grants = plain_grants(windows::grants_folder()?)?;
            Self::install_with_grants(sandbox, profile, calls, &grants)
        }
        #[cfg(not(target_os = "windows"))]
        {
            sandbox.install(profile)?;
            assert!(profile.is_file());
            #[cfg(target_os = "linux")]
            let linux = {
                let helper = linux::helper_beside_engine()?;
                let data = kernel_processes::finding::engine_root();
                linux::LinuxSandbox::detect(sandbox.clone(), &helper)?.supervise(calls, data)
            };
            #[cfg(target_os = "linux")]
            if strict == LinuxStrict::On && !linux.holds_fully() {
                return Err(SandboxError::Unavailable(LINUX_NOT_FULL_LINE.into()));
            }
            Ok(Self {
                sandbox,
                profile: profile.to_path_buf(),
                unconfined: false,
                calls,
                #[cfg(target_os = "linux")]
                linux,
            })
        }
    }

    /// The Windows arm of [`Self::install`], once its checks pass: the helper beside the
    /// engine and the boundary profile written with `grants`, which `install` gives as the
    /// engine's grants folder and a test as a folder of its own (CR-1614). Inputs: the policy,
    /// the profile path, the call supervisor choice and the grants folder, absolute and plain.
    /// Output: the boundary, or the refusal.
    #[cfg(target_os = "windows")]
    pub(crate) fn install_with_grants(
        sandbox: ProjectSandbox,
        profile: &Path,
        calls: LinuxSupervisor,
        grants: &Path,
    ) -> Result<Self, SandboxError> {
        assert!(grants.is_absolute(), "{}", grants.display());
        assert!(
            !grants.as_os_str().to_string_lossy().contains('/'),
            "{}",
            grants.display()
        );
        let helper = kernel_sandbox::windows::existing_helper(
            &kernel_sandbox::windows::helper_beside_engine()?,
        )?;
        let windows = windows::WindowsSandbox::install(sandbox.clone(), &helper, profile, grants)?;
        assert!(profile.is_file());
        Ok(Self {
            sandbox,
            profile: profile.to_path_buf(),
            unconfined: false,
            calls,
            windows,
        })
    }

    /// Marks this boundary as a Full access project's (DEC-102): every command it wraps then
    /// starts with no Seatbelt profile (see [`ProjectSandbox::wrap_unconfined`]), keeping
    /// the agent environment, the worktree start and the process group. Only macOS runs
    /// Full access; elsewhere [`Self::wrap`] refuses an unconfined boundary. Output: the
    /// marked boundary.
    pub fn unconfined(mut self) -> Self {
        self.unconfined = true;
        self
    }

    /// Whether this boundary runs its commands with no Seatbelt profile.
    pub fn is_unconfined(&self) -> bool {
        self.unconfined
    }

    /// What a run's stop inside this boundary checks of the processes it left outside its
    /// group: its Full access lineage when unconfined, a sandboxed one on Linux where the
    /// backend leaves a process in a session of its own running (`LinuxSandbox::ends_every_session`),
    /// else nothing, as the warden, a process namespace or the job ends them all. Output: the
    /// check.
    pub fn lineage_check(&self) -> LineageCheck {
        if self.unconfined {
            return LineageCheck::FullAccess;
        }
        #[cfg(target_os = "linux")]
        if !self.linux.ends_every_session() {
            return LineageCheck::Sandboxed;
        }
        LineageCheck::Unneeded
    }

    /// The program every wrapped command starts: `sandbox-exec` on macOS, or the session
    /// shim's `perl` for a Full access boundary, the Landlock helper or bwrap on Linux, the
    /// boundary helper on Windows.
    pub fn launcher(&self) -> &Path {
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        if self.unconfined {
            return Path::new(SESSION_SHIM[0]);
        }
        #[cfg(target_os = "linux")]
        {
            self.linux.launcher()
        }
        #[cfg(target_os = "windows")]
        {
            self.windows.launcher()
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            Path::new(SANDBOX_EXEC)
        }
    }

    /// The sandbox this profile was generated from.
    pub fn sandbox(&self) -> &ProjectSandbox {
        &self.sandbox
    }

    /// The installed profile file: the one `sandbox-exec` loads, or the boundary profile the
    /// Windows helper reads.
    pub fn profile(&self) -> &Path {
        &self.profile
    }

    /// The owner's "Linux call supervisor" choice this boundary was installed with; only
    /// Linux acts on it.
    pub fn linux_supervisor(&self) -> LinuxSupervisor {
        self.calls
    }

    /// Rebuilds a harness command to run inside this boundary; see [`ProjectSandbox::wrap`]
    /// on macOS, [`linux::LinuxSandbox`] on Linux and [`windows::WindowsSandbox`] on Windows.
    pub fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if self.unconfined {
            return Err(SandboxError::Unavailable(FULL_ACCESS_NEEDS_MACOS.into()));
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        if self.unconfined {
            let command = self.sandbox.wrap_unconfined(harness)?;
            assert_eq!(command.get_program(), self.launcher().as_os_str());
            return Ok(command);
        }
        #[cfg(target_os = "linux")]
        let command = Sandbox::wrap(&self.linux, harness)?;
        #[cfg(target_os = "windows")]
        let command = Sandbox::wrap(&self.windows, harness)?;
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let command = {
            let command = self.sandbox.wrap(&self.profile, harness)?;
            assert_eq!(command.get_args().nth(1), Some(self.profile.as_os_str()));
            command
        };
        assert_eq!(command.get_program(), self.launcher().as_os_str());
        Ok(command)
    }
}

impl Sandbox for InstalledSandbox {
    fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        InstalledSandbox::wrap(self, harness)
    }
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

/// The engine's grants folder when it is a plain drive path, which `install_with_grants`
/// asserts: the data root comes from the owner's `CRAZYAGENT_HOME`, so `C:/data` or a UNC
/// root is refused here, as the boundary install refused it before CR-1614, never a panic.
/// Input: the folder. Output: the folder, or `Invalid`.
#[cfg(target_os = "windows")]
pub(crate) fn plain_grants(grants: PathBuf) -> Result<PathBuf, SandboxError> {
    assert!(!grants.as_os_str().is_empty());
    if !clean_absolute(&grants) {
        return Err(invalid("grants folder must exist"));
    }
    assert!(grants.is_absolute());
    Ok(grants)
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
mod linux_sockets_tests {
    use super::*;
    use kernel_sandbox::elf::test_support::executable;

    struct Case {
        _temp: tempfile::TempDir,
        root: PathBuf,
    }

    fn case() -> Case {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["project", "home", "tmp"] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        fs::write(
            root.join("glibc"),
            executable(true, false, Some("/lib64/ld-linux-x86-64.so.2")),
        )
        .unwrap();
        fs::write(
            root.join("musl"),
            executable(true, false, Some("/lib/ld-musl-x86_64.so.1")),
        )
        .unwrap();
        fs::write(root.join("static"), executable(true, false, None)).unwrap();
        for (script, interpreter) in [
            ("on_glibc", root.join("glibc")),
            ("on_musl", root.join("musl")),
            ("on_script", root.join("on_glibc")),
            ("on_relative", PathBuf::from("node")),
        ] {
            fs::write(root.join(script), format!("#!{}\n", interpreter.display())).unwrap();
        }
        Case { _temp: temp, root }
    }

    fn sandbox(case: &Case, network: SandboxNetwork, sockets: LinuxSockets) -> ProjectSandbox {
        ProjectSandbox::new(
            &crate::sandbox::PROFILE_HOOKS,
            &case.root.join("project"),
            &case.root.join("home"),
            &case.root.join("tmp"),
            network,
        )
        .unwrap()
        .limit_sockets(sockets)
    }

    fn model() -> SandboxNetwork {
        SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)])
    }

    fn local() -> SandboxNetwork {
        SandboxNetwork::Only(vec![SandboxRemote::Loopback(8080)])
    }

    #[test]
    fn the_setting_defaults_to_tcp_only_and_keeps_its_wire_names() {
        let case = case();
        let fresh = ProjectSandbox::new(
            &crate::sandbox::PROFILE_HOOKS,
            &case.root.join("project"),
            &case.root.join("home"),
            &case.root.join("tmp"),
            model(),
        )
        .unwrap();
        assert_eq!(fresh.linux_sockets(), LinuxSockets::TcpOnly);
        for (sockets, name) in [
            (LinuxSockets::TcpOnly, "\"tcp_only\""),
            (LinuxSockets::Refuse, "\"refuse\""),
            (LinuxSockets::TcpPorts, "\"tcp_ports\""),
        ] {
            assert_eq!(serde_json::to_string(&sockets).unwrap(), name);
            assert_eq!(serde_json::from_str::<LinuxSockets>(name).unwrap(), sockets);
        }
        assert!(serde_json::from_str::<LinuxSockets>("\"udp\"").is_err());
    }

    #[test]
    fn tcp_only_starts_a_glibc_harness_and_refuses_one_that_cannot_ask_over_tcp() {
        let case = case();
        let named = sandbox(&case, model(), LinuxSockets::TcpOnly);
        assert!(named.resolves_over_tcp());
        assert_eq!(named.linux_refusal(&case.root.join("glibc")), None);
        for program in ["musl", "static", "on_musl", "absent"] {
            assert_eq!(
                named.linux_refusal(&case.root.join(program)),
                Some(LINUX_RESOLVER_NEEDS_UDP),
                "{program}"
            );
        }
        for network in [
            local(),
            SandboxNetwork::Only(Vec::new()),
            SandboxNetwork::Open,
        ] {
            let quiet = sandbox(&case, network, LinuxSockets::TcpOnly);
            assert!(!quiet.resolves_over_tcp());
            assert_eq!(quiet.linux_refusal(&case.root.join("musl")), None);
        }
    }

    #[test]
    fn tcp_only_judges_a_script_by_its_interpreter_and_names_a_deeper_chain_plainly() {
        let case = case();
        let named = sandbox(&case, model(), LinuxSockets::TcpOnly);
        assert_eq!(named.linux_refusal(&case.root.join("on_glibc")), None);
        assert_eq!(
            named.linux_refusal(&case.root.join("on_musl")),
            Some(LINUX_RESOLVER_NEEDS_UDP)
        );
        for program in ["on_script", "on_relative"] {
            assert_eq!(
                named.linux_refusal(&case.root.join(program)),
                Some(LINUX_HARNESS_UNCHECKED),
                "{program}"
            );
        }
        let quiet = sandbox(&case, local(), LinuxSockets::TcpOnly);
        assert_eq!(quiet.linux_refusal(&case.root.join("on_script")), None);
        let unchecked = crate::config::plain_problem(LINUX_HARNESS_UNCHECKED).unwrap();
        for part in [
            "cannot check",
            "'TCP only'",
            "does not start",
            "'TCP ports only'",
            "reach any host",
            "Settings",
        ] {
            assert!(unchecked.contains(part), "{unchecked}");
        }
    }

    #[test]
    fn refuse_starts_no_limited_network_agent() {
        let case = case();
        for network in [model(), local(), SandboxNetwork::Only(Vec::new())] {
            let refused = sandbox(&case, network, LinuxSockets::Refuse);
            assert!(!refused.resolves_over_tcp());
            assert_eq!(
                refused.linux_refusal(&case.root.join("glibc")),
                Some(LINUX_LIMITED_NETWORK_OFF)
            );
        }
        let open = sandbox(&case, SandboxNetwork::Open, LinuxSockets::Refuse);
        assert_eq!(open.linux_refusal(&case.root.join("glibc")), None);
    }

    #[test]
    fn only_a_nested_supervised_launch_with_its_network_held_is_full() {
        let case = case();
        let tcp = |network| sandbox(&case, network, LinuxSockets::TcpOnly);
        for held in [
            SandboxNetwork::Open,
            SandboxNetwork::Only(Vec::new()),
            local(),
        ] {
            let held = tcp(held);
            assert!(linux_holds_fully(true, true, false, &held));
            assert!(
                !linux_holds_fully(false, true, false, &held),
                "no process space"
            );
            assert!(
                !linux_holds_fully(true, false, false, &held),
                "no supervisor"
            );
        }
        let hosted = tcp(model());
        assert!(linux_holds_fully(true, true, true, &hosted), "routed");
        assert!(
            !linux_holds_fully(true, true, false, &hosted),
            "a port on any host from this computer's network"
        );
        let low = tcp(SandboxNetwork::Only(vec![
            SandboxRemote::AnyHost(443),
            SandboxRemote::Loopback(80),
        ]));
        assert!(low.routed().is_none());
        assert!(
            !linux_holds_fully(true, true, true, &low),
            "a loopback port below the bridge keeps it on this computer's network"
        );
        for sockets in [LinuxSockets::TcpPorts, LinuxSockets::Refuse] {
            let other = sandbox(&case, local(), sockets);
            assert!(!linux_holds_fully(true, true, true, &other), "{sockets:?}");
        }
        let open = sandbox(&case, SandboxNetwork::Open, LinuxSockets::TcpPorts);
        assert!(
            linux_holds_fully(true, true, false, &open),
            "an open network has no limit to hold"
        );
        let below = tcp(SandboxNetwork::Only(vec![SandboxRemote::Loopback(80)]));
        assert!(
            !linux_holds_fully(true, true, false, &below),
            "a loopback port below the bridge is not bridged"
        );
    }

    #[test]
    fn a_full_launch_is_one_no_network_line_names_on_a_host_that_holds_everything() {
        let case = case();
        let host = LinuxHost {
            nested: true,
            scoped: true,
            supervised: true,
            root: false,
            armored: false,
            routed: true,
        };
        let networks = [
            SandboxNetwork::Open,
            SandboxNetwork::Only(Vec::new()),
            local(),
            model(),
            SandboxNetwork::Only(vec![SandboxRemote::Loopback(80)]),
            SandboxNetwork::Only(vec![
                SandboxRemote::AnyHost(443),
                SandboxRemote::Loopback(80),
            ]),
            SandboxNetwork::Only(vec![
                SandboxRemote::AnyHost(443),
                SandboxRemote::Loopback(8080),
            ]),
        ];
        for network in networks {
            let launched = sandbox(&case, network, LinuxSockets::TcpOnly);
            let routed = launched.routed().is_some();
            let full = linux_holds_fully(true, true, routed, &launched);
            for choice in [
                kernel_sandbox::policy::AgentNetwork::None,
                kernel_sandbox::policy::AgentNetwork::Https,
                kernel_sandbox::policy::AgentNetwork::Open,
            ] {
                let line = linux_unbridged_notice_on(Some(host), choice, &launched);
                assert_eq!(
                    full,
                    line.is_none(),
                    "{:?} under {choice:?}: {line:?}",
                    launched.network()
                );
            }
        }
    }

    #[test]
    fn the_windows_loopback_refusal_has_its_own_code_and_reads_plainly() {
        assert!(model_loopback_windows(&SandboxError::Unavailable(
            MODEL_LOOPBACK_WINDOWS_LINE.into()
        )));
        assert!(!model_loopback_windows(&SandboxError::Unavailable(
            "other".into()
        )));
        assert!(!model_loopback_windows(&SandboxError::Invalid(
            MODEL_LOOPBACK_WINDOWS_LINE.into()
        )));
        for code in [
            MODEL_LOOPBACK_UNAVAILABLE_WINDOWS,
            kernel_sandbox::policy::SANDBOX_FULL_UNAVAILABLE_WINDOWS,
        ] {
            assert!(
                crate::config::plain_problem(code).is_some(),
                "{code} has a plain sentence"
            );
        }
    }

    #[test]
    fn the_strict_refusal_has_its_own_code_and_reads_plainly() {
        assert_eq!(
            refusal_code(&SandboxError::Unavailable(LINUX_NOT_FULL_LINE.into())),
            Some(LINUX_NOT_FULL)
        );
        assert_eq!(
            refusal_code(&SandboxError::Unavailable("other".into())),
            None
        );
        assert_eq!(
            refusal_code(&SandboxError::Invalid(LINUX_NOT_FULL_LINE.into())),
            None
        );
        let line = crate::config::plain_problem(LINUX_NOT_FULL);
        assert!(line.is_some(), "the strict refusal has a plain sentence");
        let line = line.unwrap();
        for part in [
            "Refuse agents without the full Linux sandbox",
            "Agent access",
            "Settings",
        ] {
            assert!(line.contains(part), "{line}");
        }
    }

    /// rv-code4 F2 (#622): each Linux refusal of hidden folders, where bubblewrap cannot
    /// start agents in namespaces of their own, where a second mount shows the folder and
    /// where the helper cannot cut the agent's mounts off (V24x-LINUX-NEST-MOUNT-PROPAGATION),
    /// has its own code, so the owner reads what to change instead of the generic line.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_hidden_folder_refusals_have_their_own_codes_and_read_plainly() {
        for (reason, code, parts) in [
            (
                linux::HIDDEN_NEEDS_NAMESPACES,
                LINUX_HIDDEN_NEEDS_NAMESPACES,
                ["bubblewrap", "namespaces", "Hidden folders"],
            ),
            (
                linux::HIDDEN_MOUNTED_AGAIN,
                LINUX_HIDDEN_MOUNTED_AGAIN,
                [
                    "mounted a second time",
                    "remove that mount",
                    "Hidden folders",
                ],
            ),
            (
                linux::HIDDEN_MOUNTS_NOT_PRIVATE,
                LINUX_HIDDEN_MOUNTS_NOT_PRIVATE,
                ["user namespaces", "later mount", "Hidden folders"],
            ),
        ] {
            let refused = SandboxError::Unavailable(reason.into());
            assert_eq!(refusal_code(&refused), Some(code));
            let line = crate::config::plain_problem(code);
            assert!(line.is_some(), "{code} has a plain sentence");
            let line = line.unwrap();
            for part in parts {
                assert!(line.contains(part), "{line}");
            }
        }
    }

    #[test]
    fn each_refusal_reads_plainly_and_names_the_setting() {
        let udp = crate::config::plain_problem(LINUX_RESOLVER_NEEDS_UDP).unwrap();
        for part in [
            "cannot look up names over TCP",
            "'TCP only'",
            "'TCP ports only'",
            "Settings",
            "UDP and other sockets reach any host",
        ] {
            assert!(udp.contains(part), "{udp}");
        }
        let off = crate::config::plain_problem(LINUX_LIMITED_NETWORK_OFF).unwrap();
        for part in [
            "Linux limited-network sockets",
            "does not start",
            "Settings",
        ] {
            assert!(off.contains(part), "{off}");
        }
    }

    #[test]
    fn tcp_ports_starts_any_harness_and_moves_no_lookup_to_tcp() {
        let case = case();
        let ports = sandbox(&case, model(), LinuxSockets::TcpPorts);
        assert!(!ports.resolves_over_tcp());
        for program in [
            "glibc",
            "musl",
            "static",
            "on_glibc",
            "on_musl",
            "on_script",
            "on_relative",
        ] {
            assert_eq!(
                ports.linux_refusal(&case.root.join(program)),
                None,
                "{program}"
            );
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod order_tests {
    use super::*;

    /// CTX-M1 F1: the order check the profile asserts holds for a profile as the engine
    /// writes it, and fails once a write allow follows a root pin or once a pin is gone, so
    /// the profile's assert can trip.
    #[test]
    fn a_root_pin_a_later_write_allow_overrides_fails_the_order_check() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["project", "home", "tmp"] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        let sandbox = ProjectSandbox::new(
            &crate::sandbox::PROFILE_HOOKS,
            &root.join("project"),
            &root.join("home"),
            &root.join("tmp"),
            SandboxNetwork::Open,
        )
        .unwrap();
        let pins: Vec<String> = sandbox
            .writable_roots()
            .map(|root| root_pin(root))
            .collect();
        assert_eq!(pins.len(), 3);
        let text = sandbox.profile();
        let allow = format!(
            "(allow file-read* file-write* (subpath \"{}\"))\n",
            root.join("home").display()
        );

        assert!(denies_follow_write_allows(&text, &pins));
        assert!(!denies_follow_write_allows(
            &format!("{text}{allow}"),
            &pins
        ));
        assert!(!denies_follow_write_allows(
            &text.replace(&pins[1], ""),
            &pins
        ));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod access_grant_tests {
    use super::*;
    use kernel_sandbox::policy::AccessGrant;

    fn case() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["project", "home", "tmp", "owner-tmp"] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        (temp, root)
    }

    /// F08: a socket grant is a connect-only rule ahead of the network section with its
    /// write deny after every write allow, a port grant joins a limited network on
    /// loopback, and the temp folder grant becomes a writable root handed over as TMPDIR
    /// with every socket node in it kept unwritable.
    #[test]
    fn grants_add_a_connect_only_socket_a_loopback_port_and_the_owner_s_temp_folder() {
        let (_temp, root) = case();
        let socket = root.join("owner-tmp").join("discord-ipc-0");
        let owner_tmp = root.join("owner-tmp");
        let sandbox = ProjectSandbox::new(
            &crate::sandbox::PROFILE_HOOKS,
            &root.join("project"),
            &root.join("home"),
            &root.join("tmp"),
            SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
        )
        .unwrap()
        .grant_access(
            &[
                AccessGrant::Socket {
                    path: socket.clone(),
                },
                AccessGrant::Port { port: 47_821 },
                AccessGrant::TempFolder,
            ],
            Some(&owner_tmp),
        )
        .unwrap();
        assert_eq!(
            sandbox.network(),
            &SandboxNetwork::Only(vec![
                SandboxRemote::AnyHost(443),
                SandboxRemote::Loopback(47_821)
            ])
        );
        assert_eq!(sandbox.granted_sockets(), std::slice::from_ref(&socket));
        assert_eq!(sandbox.owner_tmp(), Some(owner_tmp.as_path()));
        assert!(sandbox.writes(&owner_tmp.join("note.txt")));
        let text = sandbox.profile();
        let path = socket.display();
        assert!(text.contains(&format!(
            "(allow network-outbound (remote unix-socket (path-literal \"{path}\")))"
        )));
        assert!(!text.contains(&format!(
            "(allow network-bind (local unix-socket (path-literal \"{path}\")))"
        )));
        assert!(text.contains(&format!("(deny file-write* (literal \"{path}\"))")));
        assert!(text.contains("(allow network-outbound (remote ip \"localhost:47821\"))"));
        assert!(text.contains(&format!(
            "(deny file-write*\n  (require-all\n    (subpath \"{}\")\n    (vnode-type SOCKET)))",
            owner_tmp.display()
        )));
        let env = sandbox.base_env(true);
        let tmpdir = env
            .iter()
            .find(|(name, _)| *name == "TMPDIR")
            .map(|(_, value)| value.clone());
        assert_eq!(tmpdir, Some(owner_tmp.clone().into_os_string()));
    }

    /// F08: a grant beyond its kind's bound, a socket in a missing folder, a temp folder
    /// grant without the owner's folder and a port that overfills a limited network are
    /// refused; an open network takes a port grant as it is.
    #[test]
    fn grants_that_cannot_be_held_are_refused() {
        let (_temp, root) = case();
        let fresh = |network| {
            ProjectSandbox::new(
                &crate::sandbox::PROFILE_HOOKS,
                &root.join("project"),
                &root.join("home"),
                &root.join("tmp"),
                network,
            )
            .unwrap()
        };
        let socket = |name: &str| AccessGrant::Socket {
            path: root.join("owner-tmp").join(name),
        };
        let only = || SandboxNetwork::Only(Vec::new());
        assert!(
            fresh(only())
                .grant_access(&[socket("a"), socket("b"), socket("c")], None)
                .is_err()
        );
        let missing = AccessGrant::Socket {
            path: root.join("gone").join("x"),
        };
        assert!(fresh(only()).grant_access(&[missing], None).is_err());
        assert!(
            fresh(only())
                .grant_access(&[AccessGrant::TempFolder], None)
                .is_err()
        );
        let full: Vec<SandboxRemote> = (1..=REMOTES_MAX as u16)
            .map(SandboxRemote::AnyHost)
            .collect();
        assert!(
            fresh(SandboxNetwork::Only(full))
                .grant_access(&[AccessGrant::Port { port: 47_821 }], None)
                .is_err()
        );
        let open = fresh(SandboxNetwork::Open)
            .grant_access(&[AccessGrant::Port { port: 47_821 }], None)
            .unwrap();
        assert_eq!(open.network(), &SandboxNetwork::Open);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod profile_string_tests {
    use super::*;

    /// The engine secrets name every engine secret folder the hooks list under the runtime,
    /// and no engine folder the hooks leave out.
    #[test]
    fn the_engine_secrets_name_the_secret_folders_the_hooks_list() {
        static STAND_IN: ProfileHooks = ProfileHooks {
            engine_secret_folders: &["stand-in-secrets"],
            ..PROFILE_HOOKS
        };
        let runtime = tempfile::tempdir().unwrap();
        let secrets = engine_secrets_in(&STAND_IN, runtime.path()).unwrap();
        assert!(
            secrets.contains(&runtime.path().join("stand-in-secrets")),
            "{secrets:?}"
        );
        assert!(
            !secrets.contains(&runtime.path().join(crate::ide::FOLDER)),
            "{secrets:?}"
        );
    }

    /// The launch denies keep writes off the launch lock the hooks name, and name no other
    /// lock.
    #[test]
    fn the_launch_denies_keep_writes_off_the_lock_the_hooks_name() {
        static STAND_IN: ProfileHooks = ProfileHooks {
            launch_lock: "stand-in.lock",
            ..PROFILE_HOOKS
        };
        let denies = launch_denies(&STAND_IN, Path::new("/private/tmp/agent/home"));
        assert!(denies.contains(&any_case("stand-in.lock")), "{denies}");
        assert!(
            !denies.contains(&any_case(crate::launch_context::LOCK)),
            "{denies}"
        );
    }

    /// v2.4.0 gate: the folded secret names, 1300 bytes as one regex, split into bodies each
    /// Seatbelt reads, every name kept whole and in order; a list that fits stays one body
    /// in the shape it had, and no names give no body.
    #[test]
    fn regex_bodies_pack_whole_names_under_the_reader_limit() {
        let names: Vec<String> = kernel_sandbox::shared_folders::SECRET_NAMES
            .iter()
            .map(|name| fold_case(name))
            .collect();
        let prefix = format!("^/private/tmp/{}/(.*/)?", "p".repeat(200));
        let bodies = regex_bodies(&prefix, &names, "(/.*)?$");
        assert!(bodies.len() >= 2, "{bodies:#?}");
        assert!(bodies.iter().all(|body| body.len() <= SBPL_STRING_MAX));
        let mut at = 0;
        for body in &bodies {
            let group = &body[prefix.len() + 1..body.len() - "(/.*)?$".len() - 1];
            let start = at;
            while at < names.len() && names[start..=at].join("|").len() <= group.len() {
                at += 1;
            }
            assert_eq!(names[start..at].join("|"), group);
        }
        assert_eq!(at, names.len());
        assert_eq!(
            regex_bodies("^/r/", &["a".into(), "b".into()], "(/.*)?$"),
            vec!["^/r/(a|b)(/.*)?$".to_owned()]
        );
        assert!(regex_bodies("^/r/", &[], "(/.*)?$").is_empty());
    }

    /// The guard names every string past the limit, none in a profile that parses, and an
    /// unclosed quote too.
    #[test]
    fn sbpl_overlong_names_every_string_past_the_reader_limit() {
        let fits = "a".repeat(SBPL_STRING_MAX);
        let long = "b".repeat(SBPL_STRING_MAX + 1);
        let longer = "c".repeat(2 * SBPL_STRING_MAX);
        let text = format!(
            "(deny file-read* (subpath \"{fits}\") (literal \"{long}\") (regex #\"{longer}\"))"
        );
        assert_eq!(sbpl_overlong(&text), vec![long.as_str(), longer.as_str()]);
        assert!(sbpl_overlong(&format!("(literal \"{fits}\")")).is_empty());
        assert_eq!(sbpl_overlong("(literal \"/open"), vec!["/open"]);
    }

    /// SA vid 202: a name is spelled with every code point the case-insensitive volume folds
    /// onto its letters, also those for two or three letters (U+00DF and U+1E9E for `ss`,
    /// U+FB00 to U+FB06 for `ff`, `fi`, `fl`, `ffi`, `ffl` and `st`), and the Greek question
    /// mark and varia for `;` and the backtick, and U+0131 for `i` as NTFS maps it. In a
    /// pattern a fold reaches into a wildcard
    /// next to the letters, never into a literal or an alternation, so the env templates the
    /// secret rule gives back stay exact.
    #[test]
    fn a_name_is_spelled_with_every_fold_of_the_volume_also_for_two_or_three_letters() {
        let spelled = [
            ("config", "[cC][oO][nN]([fF](i|I|\u{131})|\u{fb01})[gG]"),
            (
                "instructions.md",
                concat!(
                    "(i|I|\u{131})[nN]((s|S|\u{17f})[tT]|\u{fb05}|\u{fb06})",
                    "[rR][uU][cC][tT](i|I|\u{131})[oO][nN](s|S|\u{17f})\\.[mM][dD]",
                ),
            ),
            (
                ".ssh",
                "\\.((s|S|\u{17f})(s|S|\u{17f})|\u{df}|\u{1e9e})[hH]",
            ),
            (
                "ffi",
                "([fF][fF](i|I|\u{131})|[fF]\u{fb01}|\u{fb00}(i|I|\u{131})|\u{fb03})",
            ),
            (".git", "\\.[gG](i|I|\u{131})[tT]"),
            ("k", "(k|K|\u{212a})"),
            (";`", "(;|\u{37e})(`|\u{1fef})"),
        ];
        for (name, pattern) in spelled {
            assert_eq!(any_case(name), pattern, "{name}");
        }
        assert_eq!(
            fold_case(".*service"),
            ".*((s|S|\u{17f})|\u{df}|\u{1e9e})[eE][rR][vV](i|I|\u{131})[cC][eE]"
        );
        assert_eq!(
            fold_case("of.*"),
            "[oO]([fF]|\u{fb00}|\u{fb01}|\u{fb02}|\u{fb03}|\u{fb04}).*"
        );
        assert_eq!(
            fold_case("secrets?"),
            "(s|S|\u{17f})[eE][cC][rR][eE][tT](s|S|\u{17f})?"
        );
        let templates = fold_case(kernel_sandbox::shared_folders::SECRET_TEMPLATES);
        assert!(!templates.contains(['\u{df}', '\u{1e9e}']));
    }

    /// B5 r2 (rv-sec3 F1, rv-code4 F1): path text with a long fold run, such as a project
    /// folder named with 7 `s` or 10 `f` or a top-level folder with 60, is spelled with no
    /// panic and no runaway list: a stretch past the spelling limit takes one of its pieces
    /// at each place, its fewest pieces' worth of places and the rest up to one per letter
    /// optional, since Seatbelt reads no `{m,n}` count. A pattern keeps every stretch exact.
    #[test]
    fn a_long_fold_run_in_path_text_is_spelled_in_bounded_length_never_a_panic() {
        let s = "((s|S|\u{17f})|\u{df}|\u{1e9e})";
        assert_eq!(any_case("ssss"), format!("{s}{s}{s}?{s}?"));
        let f = "([fF]|\u{fb00})";
        assert_eq!(any_case("ffffff"), format!("{f}{f}{f}{f}?{f}?{f}?"));
        let root = format!("/Users/o/Glasssssss/{}/{}", "f".repeat(10), "s".repeat(60));
        let spelled = any_case(&root);
        assert!(spelled.len() <= 20 * root.len(), "{}", spelled.len());
        assert!(!spelled.contains('{'), "{spelled}");
        assert!(std::panic::catch_unwind(|| fold_case("sssss")).is_err());
    }

    /// The default secret rule and 64 long open top-level names give a profile with no string
    /// past the limit; a project folder so deep that one secret name no longer fits, and the
    /// most open names at the longest length, which pass the 64 KiB cap, are each refused at
    /// install with `Invalid`, never a panic, before a profile sandbox-exec cannot load is
    /// written.
    #[test]
    fn a_profile_keeps_every_string_seatbelt_reads_or_is_refused_at_install() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let (home, tmp, profiles) = (base.join("home"), base.join("tmp"), base.join("sb"));
        let project = base.join("project");
        for folder in [&home, &tmp, &profiles, &project] {
            fs::create_dir_all(folder).unwrap();
        }
        let open: Vec<String> = (0..64)
            .map(|at| format!("{at:02}{}", "n".repeat(40)))
            .collect();
        let wide = ProjectSandbox::new(
            &crate::sandbox::PROFILE_HOOKS,
            &project,
            &home,
            &tmp,
            SandboxNetwork::Open,
        )
        .unwrap()
        .keep_project_secrets(&project, &[])
        .unwrap()
        .open_only_tops(&open)
        .unwrap();
        let text = wide.profile();
        assert!(sbpl_overlong(&text).is_empty());
        assert!(text.matches("(require-not (regex #\"").count() >= 2);
        assert!(text.matches(KEPT_OUT_DENY).count() >= 3);
        wide.install(&profiles.join("wide.sb")).unwrap();

        let most: Vec<String> = (0..OPEN_TOPS_MAX)
            .map(|at| format!("{at:03}{}", "n".repeat(252)))
            .collect();
        let crowded = ProjectSandbox::new(
            &crate::sandbox::PROFILE_HOOKS,
            &project,
            &home,
            &tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .keep_project_secrets(&project, &[])
        .unwrap()
        .open_only_tops(&most)
        .unwrap();
        let text = crowded.profile();
        assert!(sbpl_overlong(&text).is_empty());
        assert!(text.len() > PROFILE_BYTES_MAX);
        let target = profiles.join("crowded.sb");
        match crowded.install(&target) {
            Err(SandboxError::Invalid(reason)) => {
                assert_eq!(reason, "the sandbox profile is larger than its 64 KiB cap")
            }
            other => panic!("{other:?}"),
        }
        assert!(!target.exists());

        let mut deep = base.join("deep");
        while deep.as_os_str().len() < 1000 {
            let room = 1000 - deep.as_os_str().len();
            deep = deep.join("d".repeat(room.clamp(1, 200)));
        }
        fs::create_dir_all(&deep).unwrap();
        let narrow = ProjectSandbox::new(
            &crate::sandbox::PROFILE_HOOKS,
            &deep,
            &home,
            &tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .keep_project_secrets(&deep, &[])
        .unwrap();
        assert!(!sbpl_overlong(&narrow.profile()).is_empty());
        let target = profiles.join("deep.sb");
        match narrow.install(&target) {
            Err(SandboxError::Invalid(reason)) => assert_eq!(
                reason,
                "a path or name in the sandbox profile is longer than Seatbelt reads"
            ),
            other => panic!("{other:?}"),
        }
        assert!(!target.exists());
    }

    /// V243-FARM2-PRODUCT-SMALL: the agent home's config and kept-entry denies and the
    /// regranted flag and `.git` denies match a path and everything below it as one `X(/|$)`
    /// regex, never with a `(/.*)` tail, which libsandbox fails to compile again in lower case
    /// after a fold group with no bracket class before it (V243-PROFILE-CAP-ROW).
    #[test]
    fn home_denies_match_at_or_below_without_an_optional_tail() {
        let home = Path::new("/private/tmp/.tmpA1b2C3/agent/home");
        let data = Path::new("/private/tmp/.tmpA1b2C3/data");
        let config = agent_config_denies(&PROFILE_HOOKS, home);
        assert!(!config.contains("(/.*)"), "{config}");
        let regranted = regranted_denies(&PROFILE_HOOKS, home, &[data]);
        assert!(!regranted.contains("(/.*)"), "{regranted}");
        let flagged = seatbelt_pattern(home);
        assert!(
            regranted.contains(&format!(
                "(deny file-write-flags file-write-acl (regex #\"^{flagged}(/|$)\"))"
            )),
            "{regranted}"
        );
        let git = any_case(".git");
        for root in [home, data] {
            let below = format!("{}/(.+/)?{git}", seatbelt_pattern(root));
            assert!(
                regranted.contains(&format!("(regex #\"^{below}(/|$)\")")),
                "{regranted}"
            );
        }
    }

    /// V243-FARM2-PRODUCT-SMALL: no regex of a closed-tops agent's profile, a clone's store
    /// denies or a home's config denies holds a `(/.*)` tail. libsandbox fails its second,
    /// lower-case compile of one after a fold group with no bracket class or wildcard before
    /// it, as `^/x/(s|S)(/.*)?$` (probed 11-17), so each is one `X(/|$)` regex instead
    /// (V243-PROFILE-CAP-ROW). The HFS rule alone keeps a tail, after its leading `(.*/)?`,
    /// which compiles.
    #[test]
    fn no_profile_regex_has_a_slash_tail() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let (home, tmp, project) = (base.join("home"), base.join("tmp"), base.join("project"));
        for folder in [&home, &tmp, &project] {
            fs::create_dir_all(folder).unwrap();
        }
        let sandbox = ProjectSandbox::new(
            &crate::sandbox::PROFILE_HOOKS,
            &project,
            &home,
            &tmp,
            SandboxNetwork::Only(Vec::new()),
        )
        .unwrap()
        .keep_project_secrets(&project, &[])
        .unwrap()
        .open_only_tops(&["notes".to_string()])
        .unwrap();
        let text = [
            sandbox.profile(),
            clone_store_denies(&base.join("clone")),
            agent_config_denies(&PROFILE_HOOKS, &home),
        ]
        .concat();
        let hfs = hfs_ignored_deny();
        assert_eq!(text.matches(hfs.as_str()).count(), 1);
        let text = text.replace(hfs.as_str(), "");
        let bodies: Vec<&str> = text
            .split("#\"")
            .skip(1)
            .filter_map(|rest| rest.split_once('"').map(|(body, _)| body))
            .collect();
        assert!(bodies.len() > 20, "{text}");
        let tailed: Vec<&&str> = bodies
            .iter()
            .filter(|body| body.contains("(/.*)"))
            .collect();
        assert!(tailed.is_empty(), "{tailed:#?}");
        let below = bodies.iter().filter(|body| body.ends_with("(/|$)")).count();
        assert!(below >= 20, "{below}");
    }
}

#[cfg(all(test, target_os = "macos"))]
mod firmlink_tests {
    use super::*;

    /// V243-FIRMLINK-SANDBOX-ROOT (sweep #3): a root named through /System/Volumes/Data is
    /// refused, since Seatbelt matches only the short name and every rule written in the long
    /// one would never match; an extra read path typed that way is checked under its short
    /// name, so one holding the engine's data is refused.
    #[test]
    fn a_root_named_through_the_data_volume_is_refused_and_a_read_path_is_checked_short() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["project", "home", "tmp", "engine"] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        let long = |path: &Path| PathBuf::from(format!("/System/Volumes/Data{}", path.display()));
        assert!(long(&root.join("project")).is_dir());
        let network = || SandboxNetwork::Only(Vec::new());
        let (home, tmp) = (root.join("home"), root.join("tmp"));
        let refused = ProjectSandbox::new(
            &crate::sandbox::PROFILE_HOOKS,
            &long(&root.join("project")),
            &home,
            &tmp,
            network(),
        );
        assert!(
            matches!(&refused, Err(SandboxError::Invalid(reason)) if reason.contains("canonical")),
            "{refused:?}"
        );
        assert!(
            ProjectSandbox::new(
                &crate::sandbox::PROFILE_HOOKS,
                &root.join("project"),
                &home,
                &tmp,
                network()
            )
            .is_ok()
        );
        assert_eq!(
            kernel_sandbox::extra_paths::checked(
                &ACCESS_HOOKS,
                long(&root).to_str().unwrap(),
                &root.join("engine")
            ),
            Err("read_path_engine_data")
        );
    }
}
