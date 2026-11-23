use kernel_sandbox::access::AccessHooks;
use kernel_sandbox::elf;
use kernel_sandbox::error::{SandboxError, invalid};
use kernel_sandbox::grants::GrantHooks;
use kernel_sandbox::hidden_paths::HIDDEN_MAX;
use kernel_sandbox::host::LinuxHost;
use kernel_sandbox::lines::*;
use kernel_sandbox::net::{SandboxNetwork, SandboxRemote};
use kernel_sandbox::paths::{
    any_case, at_or_below, canonical, clean_absolute, clear_of_secrets, fold_case, granted_root,
    owner_home, owner_secrets, plain_top, regex_escape,
};
use kernel_sandbox::policy::PolicyHooks;
use kernel_sandbox::ports::{
    BRIDGE_PORT_MIN, KEPT_PORTS_MAX, OWNER_PORTS, PROXY_PORTS, RELAY_PORTS, REMOTES_MAX,
};
use kernel_sandbox::profile::ProfileHooks;
use kernel_sandbox::profile::command::*;
use kernel_sandbox::profile::consts::*;
use kernel_sandbox::profile::env::*;
use kernel_sandbox::profile::seatbelt::*;
use kernel_sandbox::settings::{LinuxSockets, LinuxStrict, LinuxSupervisor};
use kernel_sandbox::shared_folders::OPEN_TOPS_MAX;
#[cfg(target_os = "macos")]
use kernel_sandbox::tool_cache::ToolCacheHooks;
use kernel_sandbox::toolchains::ToolchainHooks;
use std::ffi::{OsStr, OsString};
use std::fs;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
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

/// Tells whether a launch holds the full Linux sandbox (DEC-185): a process space of its own
/// (the nested backend), the call supervisor running, and its network limits held, so none
/// of the gap lines would show for it. An open network has no limit to hold; a limited one
/// needs TCP-only sockets and, for a port on any host, a network of its own (DEC-179 SBX-2).
/// Inputs: what the launch got and its sandbox. Output: true when it holds all of it.
pub fn linux_holds_fully(
    nested: bool,
    supervised: bool,
    routed: bool,
    sandbox: &ProjectSandbox,
) -> bool {
    assert!(
        !routed || nested,
        "only a nested launch has a network of its own"
    );
    if !nested || !supervised {
        return false;
    }
    let SandboxNetwork::Only(remotes) = &sandbox.network else {
        return true;
    };
    if sandbox.linux_sockets != LinuxSockets::TcpOnly {
        return false;
    }
    remotes.iter().all(|remote| match remote {
        SandboxRemote::Loopback(port) => *port >= BRIDGE_PORT_MIN,
        SandboxRemote::AnyHost(_) => routed,
    })
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
        kernel_sandbox::linux::helper::HIDDEN_MOUNTS_NOT_PRIVATE => Some(LINUX_HIDDEN_MOUNTS_NOT_PRIVATE),
        _ => None,
    }
}

/// What a launch tells the owner in the agent's messages when a Linux that nests agents
/// still leaves it on this computer's network: for an agent that could have a network of
/// its own ([`ProjectSandbox::routed`]) on a host that does not route, [`LINUX_UNROUTED_LINE`]
/// or, with no loopback port or the supervisor on, [`LINUX_UNROUTED_ANY_HOST_LINE`]; none
/// when it routes; then [`linux_unbridged_line`] for the lowest port below
/// [`BRIDGE_PORT_MIN`], else under None [`LINUX_PORTS_ANY_HOST_LINE`]. Inputs: this Linux,
/// the network choice, the sandbox as launched. Output: the sentences, or none.
pub fn linux_unbridged_notice_on(
    host: Option<LinuxHost>,
    choice: kernel_sandbox::policy::AgentNetwork,
    sandbox: &ProjectSandbox,
) -> Option<String> {
    let host = host.filter(|host| host.nested)?;
    let SandboxNetwork::Only(remotes) = &sandbox.network else {
        return None;
    };
    let reaches = remotes
        .iter()
        .any(|remote| matches!(remote, SandboxRemote::Loopback(_)));
    if sandbox.routed().is_some() {
        if host.routed {
            return None;
        }
        let wide = reaches && !host.supervised;
        let line = if wide {
            LINUX_UNROUTED_LINE
        } else {
            LINUX_UNROUTED_ANY_HOST_LINE
        };
        return Some(line.to_owned());
    }
    let low = remotes
        .iter()
        .filter_map(|remote| match remote {
            SandboxRemote::Loopback(port) if *port < BRIDGE_PORT_MIN => Some(*port),
            _ => None,
        })
        .min();
    let any = remotes
        .iter()
        .any(|remote| matches!(remote, SandboxRemote::AnyHost(_)));
    let tcp = sandbox.linux_sockets == LinuxSockets::TcpOnly;
    if let Some(port) = low.filter(|_| host.routed && any && tcp) {
        return Some(linux_unbridged_line(port));
    }
    if let Some(port) = sandbox.unbridged_port() {
        assert!(port < BRIDGE_PORT_MIN);
        return Some(linux_unbridged_line(port));
    }
    let unsaid =
        choice == kernel_sandbox::policy::AgentNetwork::None && sandbox.bridged_ports().is_none();
    (reaches && unsaid).then(|| LINUX_PORTS_ANY_HOST_LINE.to_owned())
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
    crazyagent_directory: crate::projects_home::CRAZYAGENT_DIRECTORY,
    #[cfg(unix)]
    binding_path: crate::team::caller::binding_path,
    mailbox_folder: crate::team::tool::MAILBOX_FOLDER,
    reply_suffix: crate::team::tool::REPLY_SUFFIX,
    answer_part_suffix: crate::team::tool::ANSWER_PART_SUFFIX,
    key_folders: &crate::storage::accounts::KEY_FOLDERS,
    engine_secret_folders: ENGINE_SECRET_FOLDERS,
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

/// The Seatbelt boundary for one agent of one project: writes only inside the
/// project folder (never its `context/` or git hooks and config), the agent's
/// worktrees and its own home and temp dirs, never the launch files, instructions,
/// skills or settings its harness loads from that home, reads of system paths plus
/// those roots, and network per [`SandboxNetwork`].
#[derive(Debug, Clone)]
pub struct ProjectSandbox {
    hooks: &'static ProfileHooks,
    project_root: PathBuf,
    agent_home: PathBuf,
    agent_tmp: PathBuf,
    worktrees: Vec<PathBuf>,
    read_roots: Vec<PathBuf>,
    tool_roots: Vec<PathBuf>,
    tool_files: Vec<PathBuf>,
    execs: Vec<PathBuf>,
    scratch: Option<PathBuf>,
    hidden: Vec<PathBuf>,
    kept_out: Vec<PathBuf>,
    revealed: Vec<PathBuf>,
    links: Vec<PathBuf>,
    network: SandboxNetwork,
    linux_sockets: LinuxSockets,
    kept_ports: Vec<u16>,
    owner_home: PathBuf,
    engine_secrets: Vec<PathBuf>,
    carved_home: Option<(PathBuf, PathBuf)>,
    checkout: Option<Checkout>,
    clone: Option<PathBuf>,
    sockets: Vec<PathBuf>,
    served: Vec<PathBuf>,
    reached: Vec<PathBuf>,
    shut: Vec<PathBuf>,
    read_only: Vec<PathBuf>,
    fixed: Vec<PathBuf>,
    shared: Vec<PathBuf>,
    teammates: bool,
    answers: Option<PathBuf>,
    setup: Option<TeammatesSetup>,
    tool_cache: Option<(PathBuf, bool)>,
    lead_branch: Option<String>,
    adds_worktrees: bool,
    agent_files: Option<kernel_sandbox::policy::AgentFilesRule>,
    granted: Vec<PathBuf>,
    owner_tmp: Option<PathBuf>,
    project_secrets: Option<PathBuf>,
    secrets_revealed: Vec<PathBuf>,
    shared_writes: Vec<PathBuf>,
    open_tops: Option<Vec<String>>,
    node_bin: Option<PathBuf>,
    owner_config: bool,
    job_folder: bool,
    #[cfg(unix)]
    engine_home: Option<bool>,
}

/// What the engine's git may write, with a linked lead's rights, while it adds the lead's
/// native teammates' linked worktrees (OP-9b): the teammates' folder inside the lead's
/// worktree and the project repository's worktree records.
#[derive(Debug, Clone)]
struct TeammatesSetup {
    folder: PathBuf,
    records: PathBuf,
}

/// The one git worktree a confined agent writes in place of the whole project folder,
/// with the two parts of the project repository its commits need: the worktree's own
/// admin folder under `.git/worktrees` and the shared object store.
#[derive(Debug, Clone)]
struct Checkout {
    worktree: PathBuf,
    admin: PathBuf,
    objects: PathBuf,
}

impl ProjectSandbox {
    /// Validates canonical, existing, quote-free directories that do not contain the
    /// owner's home, do not sit inside a harness's own folder there, and neither hold nor
    /// sit inside an owner or system secret path. Output: the sandbox, or `Invalid`.
    pub fn new(
        hooks: &'static ProfileHooks,
        project_root: &Path,
        agent_home: &Path,
        agent_tmp: &Path,
        network: SandboxNetwork,
    ) -> Result<Self, SandboxError> {
        let owner = owner_home()?;
        let secrets: Vec<PathBuf> =
            owner_secrets(&owner, &(hooks.access.harness_folders)()).collect();
        for root in [project_root, agent_home, agent_tmp] {
            granted_root(root, &owner, &secrets, &(hooks.access.harness_folders)())?;
        }
        if let SandboxNetwork::Only(remotes) = &network
            && (remotes.len() > REMOTES_MAX
                || remotes.iter().any(|remote| match remote {
                    SandboxRemote::Loopback(port) | SandboxRemote::AnyHost(port) => *port == 0,
                }))
        {
            return Err(invalid("network remotes must be 1..8 nonzero ports"));
        }
        Ok(Self {
            hooks,
            project_root: project_root.to_path_buf(),
            agent_home: agent_home.to_path_buf(),
            agent_tmp: agent_tmp.to_path_buf(),
            worktrees: Vec::new(),
            read_roots: Vec::new(),
            tool_roots: Vec::new(),
            tool_files: Vec::new(),
            execs: Vec::new(),
            scratch: None,
            hidden: Vec::new(),
            kept_out: Vec::new(),
            revealed: Vec::new(),
            links: Vec::new(),
            network,
            linux_sockets: LinuxSockets::default(),
            kept_ports: Vec::new(),
            owner_home: owner,
            engine_secrets: Vec::new(),
            carved_home: None,
            checkout: None,
            clone: None,
            sockets: Vec::new(),
            served: Vec::new(),
            reached: Vec::new(),
            shut: Vec::new(),
            read_only: Vec::new(),
            fixed: Vec::new(),
            shared: Vec::new(),
            teammates: false,
            answers: None,
            setup: None,
            tool_cache: None,
            lead_branch: None,
            adds_worktrees: false,
            agent_files: None,
            granted: Vec::new(),
            owner_tmp: None,
            project_secrets: None,
            secrets_revealed: Vec::new(),
            shared_writes: Vec::new(),
            open_tops: None,
            node_bin: None,
            owner_config: false,
            job_folder: false,
            #[cfg(unix)]
            engine_home: None,
        })
    }

    /// Adds the narrow extras the owner allowed the project's agents (F08): each socket
    /// grant as a connect-and-read grant of its exact path in a list of its own, never bind,
    /// write or unlink; each port grant as an outbound loopback port when the network is
    /// limited (an open network reaches it already); and the temp folder grant as the
    /// owner's private temp folder, written as a root and handed over as `TMPDIR`, with every
    /// socket node in it kept unwritable. Inputs: the saved grants and the owner's temp
    /// folder as [`kernel_sandbox::grants::owner_temp_folder`] gives it, none when it cannot be given.
    /// Output: the widened sandbox, or `Invalid`; sockets and the temp folder need Seatbelt.
    pub fn grant_access(
        mut self,
        grants: &[kernel_sandbox::policy::AccessGrant],
        owner_tmp: Option<&Path>,
    ) -> Result<Self, SandboxError> {
        if grants.len() > kernel_sandbox::policy::GRANTS_MAX {
            return Err(invalid("too many access grants"));
        }
        for grant in grants {
            if !grant.plain() {
                return Err(invalid("an access grant is not plain"));
            }
            match grant {
                kernel_sandbox::policy::AccessGrant::Socket { path } => {
                    if !cfg!(target_os = "macos") {
                        return Err(invalid("unix socket grants need Seatbelt"));
                    }
                    let folder = path.parent().filter(|folder| {
                        fs::symlink_metadata(folder).is_ok_and(|meta| meta.is_dir())
                    });
                    let known = self
                        .sockets
                        .iter()
                        .chain(&self.served)
                        .chain(&self.reached)
                        .chain(&self.shut)
                        .chain(&self.granted)
                        .any(|socket| socket == path);
                    if folder.is_none()
                        || known
                        || self.granted.len() >= kernel_sandbox::policy::GRANT_SOCKETS_MAX
                    {
                        return Err(invalid("a socket grant cannot be held"));
                    }
                    self.granted.push(path.clone());
                }
                kernel_sandbox::policy::AccessGrant::Port { port } => {
                    if let SandboxNetwork::Only(remotes) = &mut self.network {
                        let remote = SandboxRemote::Loopback(*port);
                        if !remotes.contains(&remote) {
                            remotes.push(remote);
                        }
                        if remotes.len() > REMOTES_MAX {
                            return Err(invalid("network remotes must be 1..8 nonzero ports"));
                        }
                    }
                }
                kernel_sandbox::policy::AccessGrant::TempFolder => {
                    if !cfg!(target_os = "macos") {
                        return Err(invalid("the temp folder grant needs Seatbelt"));
                    }
                    let tmp = owner_tmp
                        .ok_or_else(|| invalid("the owner's temp folder cannot be given"))?;
                    let owner = owner_home()?;
                    let secrets: Vec<PathBuf> =
                        owner_secrets(&owner, &(self.hooks.access.harness_folders)()).collect();
                    granted_root(
                        tmp,
                        &owner,
                        &secrets,
                        &(self.hooks.access.harness_folders)(),
                    )?;
                    clear_of_secrets(tmp, &self.engine_secrets)?;
                    self.owner_tmp = Some(tmp.to_path_buf());
                }
            }
        }
        assert!(self.granted.len() <= kernel_sandbox::policy::GRANT_SOCKETS_MAX);
        Ok(self)
    }

    /// The owner's temp folder this sandbox hands over as `TMPDIR` under a temp folder
    /// grant, or none. Takes the sandbox. Returns the folder.
    pub fn owner_tmp(&self) -> Option<&Path> {
        self.owner_tmp.as_deref()
    }

    /// The exact sockets the owner's grants let this agent connect to. Takes the sandbox.
    /// Returns the paths.
    pub fn granted_sockets(&self) -> &[PathBuf] {
        &self.granted
    }

    /// Lets the agent bind and connect one Unix socket at an exact path, read and write that
    /// file and its `<path>.lock`, and read its folder's metadata, nothing else there: its
    /// own Codex app-server socket in the shared daemon folder (DEC-104 K2b). Input: a plain
    /// absolute path of letters, digits, `/`, `.`, `-` and `_` in an existing folder.
    /// Output: the widened sandbox, or `Invalid`; only Seatbelt can hold the grant.
    pub fn allow_unix_socket(mut self, socket: &Path) -> Result<Self, SandboxError> {
        self.socket_room(socket)?;
        if self.sockets.len() >= SOCKETS_MAX {
            return Err(invalid("too many unix sockets"));
        }
        self.sockets.push(socket.to_path_buf());
        assert!(self.sockets.len() <= SOCKETS_MAX && self.socket_grants() <= SOCKET_GRANTS_MAX);
        Ok(self)
    }

    /// Lets the agent serve one Unix socket at an exact path but never connect to it or
    /// unlink it: bind, create and change that file and its `<path>.lock`, and read its
    /// folder's metadata. A native lead's teammates run their tmux server this way, so none
    /// of them reaches it or swaps the node for a listener of its own (rv-audit S1 F1).
    /// Input: as [`Self::allow_unix_socket`]. Output: the widened sandbox, or `Invalid`.
    pub fn serve_unix_socket(mut self, socket: &Path) -> Result<Self, SandboxError> {
        self.socket_room(socket)?;
        self.served.push(socket.to_path_buf());
        assert!(self.socket_grants() <= SOCKET_GRANTS_MAX);
        Ok(self)
    }

    /// Lets the agent connect to one Unix socket at an exact path and read it, never bind
    /// it or create, change, link or unlink a node there, such as a native lead reaching its
    /// teammates' tmux server and its teammates reaching the engine's caller socket (rv-audit
    /// S1 F1, TTDL F1). Input: as [`Self::allow_unix_socket`]. Output: the widened sandbox,
    /// or `Invalid`.
    pub fn reach_unix_socket(mut self, socket: &Path) -> Result<Self, SandboxError> {
        self.socket_room(socket)?;
        self.reached.push(socket.to_path_buf());
        assert!(self.socket_grants() <= SOCKET_GRANTS_MAX);
        Ok(self)
    }

    /// Keeps the agent from writing, unlinking or locking one Unix socket at an exact path
    /// inside its writable folders, with no grant to bind or reach it, such as a native
    /// lead's own tmux socket for its teammates (rv-audit S1 F1). Input: as
    /// [`Self::allow_unix_socket`]. Output: the narrowed sandbox, or `Invalid`.
    pub fn shut_unix_socket(mut self, socket: &Path) -> Result<Self, SandboxError> {
        self.socket_room(socket)?;
        self.shut.push(socket.to_path_buf());
        assert!(self.socket_grants() <= SOCKET_GRANTS_MAX);
        Ok(self)
    }

    /// The Unix socket grants of every use this sandbox holds. Takes the sandbox. Returns
    /// the count.
    fn socket_grants(&self) -> usize {
        self.sockets.len() + self.served.len() + self.reached.len() + self.shut.len()
    }

    /// Checks that one more Unix socket grant fits and names a plain path no grant of this
    /// sandbox names yet: absolute, at most `SOCKET_PATH_BYTES_MAX`, of letters, digits,
    /// `/`, `.`, `-` and `_`, in an existing folder. Takes the socket. Returns Ok, or
    /// `Invalid`; off macOS no Unix socket grant exists.
    fn socket_room(&self, socket: &Path) -> Result<(), SandboxError> {
        if !cfg!(target_os = "macos") {
            return Err(invalid("unix socket grants need Seatbelt"));
        }
        if self.socket_grants() >= SOCKET_GRANTS_MAX {
            return Err(invalid("too many unix sockets"));
        }
        let granted = self
            .sockets
            .iter()
            .chain(&self.served)
            .chain(&self.reached)
            .chain(&self.shut)
            .any(|known| known == socket);
        if granted {
            return Err(invalid("the unix socket already has a grant"));
        }
        let text = socket
            .to_str()
            .ok_or_else(|| invalid("the unix socket path is not plain"))?;
        let plain = socket.is_absolute()
            && text.len() <= SOCKET_PATH_BYTES_MAX
            && !text.ends_with('/')
            && !text.contains("//")
            && text.split('/').all(|part| part != "." && part != "..")
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_'));
        let folder = socket
            .parent()
            .filter(|folder| std::fs::symlink_metadata(folder).is_ok_and(|meta| meta.is_dir()));
        if !plain || folder.is_none() {
            return Err(invalid("the unix socket path is not plain"));
        }
        Ok(())
    }

    /// Keeps the ports of the owner's configured local model endpoints free of this agent's
    /// listeners, beside [`OWNER_PORTS`], when its network is open. Input: the ports, as
    /// [`owner_ports`] lists them. Output: the sandbox, or a refusal for port 0 or more than
    /// `KEPT_PORTS_MAX` ports.
    pub fn keep_ports(mut self, ports: &[u16]) -> Result<Self, SandboxError> {
        if ports.contains(&0) {
            return Err(invalid("kept ports must be nonzero"));
        }
        self.kept_ports.extend(
            ports
                .iter()
                .copied()
                .filter(|port| !OWNER_PORTS.contains(port)),
        );
        self.kept_ports.sort_unstable();
        self.kept_ports.dedup();
        if self.kept_ports.len() > KEPT_PORTS_MAX {
            return Err(invalid("at most 64 kept ports"));
        }
        Ok(self)
    }

    /// Names the engine runtime folder whose secrets no granted root may hold or sit inside:
    /// engine.db, the owner socket, the GitHub files, the account and plugin keys, the upgrade
    /// backups, the native profiles, the configured SDK homes, the terminal and IDE folders
    /// and every engine socket and socket folder in it now; the agent's home and temp may sit
    /// in it. Input: the existing runtime folder. Output: the guarded sandbox, or a refusal.
    pub fn guard_engine(mut self, runtime: &Path) -> Result<Self, SandboxError> {
        let runtime = canonical(runtime)?;
        if !clean_absolute(&runtime) || !runtime.is_dir() {
            return Err(invalid("engine runtime must be an existing plain folder"));
        }
        let secrets = engine_secrets_in(self.hooks, &runtime)?;
        for root in self
            .writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
            .chain(self.tool_files.iter())
        {
            clear_of_secrets(root, &secrets)?;
        }
        self.engine_secrets = secrets;
        Ok(self)
    }

    /// Adds the engine root's secrets for an agent whose runtime folder is one folder in it,
    /// as an owner generation is for a master: every entry guard_engine would name for the
    /// engine root except that folder itself, where the agent's home lives. A master's home
    /// may instead sit in one secret folder ([`carved_folder`]): that folder stays a secret and
    /// only this home is carved back out of it, so every other home, even one made later,
    /// stays denied. Call it after guard_engine on the runtime folder, whose secrets stay.
    /// Inputs: the engine root and the runtime folder. Output: the guarded sandbox, or a
    /// refusal.
    pub fn guard_engine_around(
        mut self,
        engine: &Path,
        runtime: &Path,
    ) -> Result<Self, SandboxError> {
        let engine = canonical(engine)?;
        let runtime = canonical(runtime)?;
        if !clean_absolute(&engine)
            || !engine.is_dir()
            || runtime.parent() != Some(engine.as_path())
        {
            return Err(invalid(
                "engine root must be the existing folder around the runtime",
            ));
        }
        let carved = carved_folder(self.hooks, &engine, &self.agent_home)
            .map(|folder| (self.agent_home.clone(), folder));
        let secrets: Vec<PathBuf> = engine_secrets_in(self.hooks, &engine)?
            .into_iter()
            .filter(|secret| *secret != runtime)
            .collect();
        let around_home: Vec<PathBuf> = secrets
            .iter()
            .filter(|secret| carved.as_ref().is_none_or(|(_, folder)| *secret != folder))
            .cloned()
            .collect();
        for root in self
            .writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
            .chain(self.tool_files.iter())
        {
            if carved.as_ref().is_some_and(|(home, _)| home == root) {
                clear_of_secrets(root, &around_home)?;
            } else {
                clear_of_secrets(root, &secrets)?;
            }
        }
        for secret in secrets {
            if !self.engine_secrets.contains(&secret) {
                self.engine_secrets.push(secret);
            }
        }
        assert!(!self.engine_secrets.contains(&runtime));
        if let Some((home, folder)) = carved {
            assert!(self.engine_secrets.contains(&folder));
            assert!(
                self.secret_paths()
                    .iter()
                    .filter(|secret| **secret != folder)
                    .all(|secret| !home.starts_with(secret) && !secret.starts_with(&home))
            );
            self.carved_home = Some((home, folder));
        }
        Ok(self)
    }

    /// Every path this sandbox denies to the agent although an allowed root might cover
    /// it: the owner's Keychains, SSH, GPG, cloud, gh, git, Codex and Claude credentials,
    /// the system Keychains and, once guarded, the engine runtime's secrets.
    pub fn secret_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> =
            owner_secrets(&self.owner_home, &(self.hooks.access.harness_folders)()).collect();
        paths.extend(self.engine_secrets.iter().cloned());
        paths
    }

    /// Adds one writable worktree the agent may also start in, for a checkout the
    /// engine keeps outside the project folder. Input: a canonical existing folder that
    /// neither holds nor sits inside a path from [`Self::secret_paths`].
    /// Output: the widened sandbox, or a refusal for an unsafe or extra root.
    pub fn allow_worktree(mut self, worktree: &Path) -> Result<Self, SandboxError> {
        if self.worktrees.len() >= WORKTREES_MAX {
            return Err(invalid("too many worktrees"));
        }
        granted_root(
            worktree,
            &owner_home()?,
            &self.secret_paths(),
            &(self.hooks.access.harness_folders)(),
        )?;
        self.worktrees.push(worktree.to_path_buf());
        assert!(self.worktrees.len() <= WORKTREES_MAX);
        Ok(self)
    }

    /// Lets git create a checkout's `.git` link in the project's worktrees folders and the
    /// `gitdir` and `commondir` files of its admin folder, which no other profile may plant
    /// (SECAUDIT-WORKTREE-ADMIN-PLANT). Only the engine's own worktree-add step installs it,
    /// and its callers confine and reinstall the profile before any agent runs.
    /// Output: the sandbox with that exemption.
    #[cfg(unix)]
    pub(crate) fn adding_worktrees(mut self) -> Self {
        self.adds_worktrees = true;
        self
    }

    /// Confines the agent's writes to its own worktree, its `.git/worktrees` admin folder
    /// except its HEAD, which only the engine moves, and the object store, beside its home,
    /// temp dir and extra worktrees; the project stays readable. Makes the admin folder's
    /// `modules` store so every platform guards a real folder there. Input: a canonical
    /// worktree in `worktrees/` whose `.git` link and admin folder name each other.
    /// Output: the confined sandbox, `Invalid` or the disk error.
    pub fn confine_to_worktree(mut self, worktree: &Path) -> Result<Self, SandboxError> {
        if self.checkout.is_some() || self.clone.is_some() || self.setup.is_some() {
            return Err(invalid("the sandbox is already confined to a worktree"));
        }
        if self.read_roots.len() >= READ_ROOTS_MAX {
            return Err(invalid("too many read roots"));
        }
        let worktrees = (self.hooks.access.state_folder)(&self.project_root).join("worktrees");
        if !clean_absolute(worktree)
            || worktree.parent() != Some(worktrees.as_path())
            || canonical(worktree)? != worktree
        {
            return Err(invalid(
                "the worktree must be a canonical folder in worktrees/",
            ));
        }
        let git = self.project_root.join(".git");
        let admin = canonical(&worktree.join(git_link(&worktree.join(".git"), "gitdir: ")?))?;
        if !clean_absolute(&admin) || admin.parent() != Some(git.join("worktrees").as_path()) {
            return Err(invalid(
                "the worktree's git folder must sit in .git/worktrees",
            ));
        }
        let back = canonical(&admin.join(git_link(&admin.join("gitdir"), "")?))?;
        if back != worktree.join(".git") {
            return Err(invalid("the worktree's git folder names another worktree"));
        }
        let objects = git.join("objects");
        if !objects.is_dir() || canonical(&objects)? != objects {
            return Err(invalid("the project's git objects must be a plain folder"));
        }
        let secrets = self.secret_paths();
        for root in [worktree, admin.as_path(), objects.as_path()] {
            clear_of_secrets(root, &secrets)?;
        }
        assert!(
            [worktree, admin.as_path(), objects.as_path()]
                .iter()
                .all(|root| root.starts_with(&self.project_root) && *root != self.project_root)
        );
        ensure_store(&admin.join("modules")).map_err(|problem| match problem.kind() {
            io::ErrorKind::InvalidData => {
                invalid("the worktree's submodule store must be a plain folder")
            }
            _ => SandboxError::Io(problem),
        })?;
        self.read_roots.push(self.project_root.clone());
        self.checkout = Some(Checkout {
            worktree: worktree.to_path_buf(),
            admin,
            objects,
        });
        Ok(self)
    }

    /// Confines an Own branch or Full access agent (DEC-102) to its own clone of the project:
    /// the clone, with its git folder inside it, is the agent's only writable project path,
    /// so it commits on its own branch with full git while the project folder, its `.git`
    /// and the object store the clone borrows through `objects/info/alternates` stay
    /// read-only. Input: a canonical folder in the project's `worktrees/` folder whose
    /// `.git` is a real folder, not a link. Output: the confined sandbox, or `Invalid`.
    pub fn confine_to_clone(mut self, worktree: &Path) -> Result<Self, SandboxError> {
        if self.checkout.is_some() || self.clone.is_some() || self.setup.is_some() {
            return Err(invalid("the sandbox is already confined to a worktree"));
        }
        if self.read_roots.len() >= READ_ROOTS_MAX {
            return Err(invalid("too many read roots"));
        }
        let worktrees = (self.hooks.access.state_folder)(&self.project_root).join("worktrees");
        if !clean_absolute(worktree)
            || worktree.parent() != Some(worktrees.as_path())
            || canonical(worktree)? != worktree
        {
            return Err(invalid(
                "the worktree must be a canonical folder in worktrees/",
            ));
        }
        let git = worktree.join(".git");
        let metadata = fs::symlink_metadata(&git)?;
        if !metadata.is_dir() || canonical(&git)? != git {
            return Err(invalid("the clone's git folder must be a real folder"));
        }
        clear_of_secrets(worktree, &self.secret_paths())?;
        assert!(worktree.starts_with(&self.project_root) && worktree != self.project_root);
        self.read_roots.push(self.project_root.clone());
        self.clone = Some(worktree.to_path_buf());
        Ok(self)
    }

    /// Names the branch a clone lead works on, as the engine records it, so its native
    /// teammates write only the refs of its `<branch>--<name>` teammate branches in the
    /// clone, never the lead's own (AGENT-REF-ALLOW-NARROW). Input: an agent branch, as
    /// [`crate::agent_git::is_agent_branch`] says. Output: the sandbox, or `Invalid` before
    /// [`Self::confine_to_clone`], a second time or for any other branch.
    pub fn lead_branch(mut self, branch: &str) -> Result<Self, SandboxError> {
        if self.clone.is_none() || self.lead_branch.is_some() {
            return Err(invalid(
                "only a clone lead's sandbox names its branch, once",
            ));
        }
        if !(self.hooks.is_agent_branch)(branch) {
            return Err(invalid("the lead's branch must be an agent branch"));
        }
        self.lead_branch = Some(branch.to_owned());
        Ok(self)
    }

    /// Confines the engine's own git, run with a linked lead's rights, to adding the lead's
    /// native teammates' linked worktrees (OP-9b): it writes only the teammates' folder of
    /// the lead's worktree, the project repository's `.git/worktrees` records and the refs
    /// and reflogs of the lead's own `<branch>--<name>` teammate branches with their
    /// `crazyagent` folders (AGENT-REF-ALLOW-NARROW), beside the lead's home and temp dir;
    /// the project stays readable. Makes the teammates' folder as a plain folder. Inputs:
    /// the lead's canonical linked worktree in `worktrees/`, whose `.git` link names a
    /// record in `.git/worktrees`, and its agent branch as the engine records it. Output:
    /// the confined sandbox, `Invalid` or the disk error.
    pub fn confine_to_teammates_setup(
        mut self,
        worktree: &Path,
        branch: &str,
    ) -> Result<Self, SandboxError> {
        if self.checkout.is_some() || self.clone.is_some() || self.setup.is_some() {
            return Err(invalid("the sandbox is already confined to a worktree"));
        }
        if !(self.hooks.is_agent_branch)(branch) {
            return Err(invalid("the lead's branch must be an agent branch"));
        }
        if self.read_roots.len() >= READ_ROOTS_MAX {
            return Err(invalid("too many read roots"));
        }
        let worktrees = (self.hooks.access.state_folder)(&self.project_root).join("worktrees");
        if !clean_absolute(worktree)
            || worktree.parent() != Some(worktrees.as_path())
            || canonical(worktree)? != worktree
        {
            return Err(invalid(
                "the worktree must be a canonical folder in worktrees/",
            ));
        }
        let records = self.project_root.join(".git").join("worktrees");
        let admin = canonical(&worktree.join(git_link(&worktree.join(".git"), "gitdir: ")?))?;
        if admin.parent() != Some(records.as_path()) || canonical(&records)? != records {
            return Err(invalid(
                "the lead's worktree must be a linked worktree of the project",
            ));
        }
        let folder = worktree.join(self.hooks.teammates_folder);
        ensure_store(&folder).map_err(|problem| match problem.kind() {
            io::ErrorKind::InvalidData => invalid("the teammates' folder must be a plain folder"),
            _ => SandboxError::Io(problem),
        })?;
        if canonical(&folder)? != folder {
            return Err(invalid("the teammates' folder must be a plain folder"));
        }
        let secrets = self.secret_paths();
        for root in [folder.as_path(), records.as_path()] {
            clear_of_secrets(root, &secrets)?;
        }
        assert!(
            [folder.as_path(), records.as_path()]
                .iter()
                .all(|root| root.starts_with(&self.project_root) && *root != self.project_root)
        );
        self.read_roots.push(self.project_root.clone());
        self.setup = Some(TeammatesSetup { folder, records });
        self.lead_branch = Some(branch.to_owned());
        Ok(self)
    }

    /// Adds one extra read-only root, such as a harness install directory. Input: a
    /// canonical existing folder that neither holds nor sits inside a path from
    /// [`Self::secret_paths`]. Output: the widened sandbox, or a refusal.
    pub fn allow_read(mut self, root: &Path) -> Result<Self, SandboxError> {
        if self.read_roots.len() >= READ_ROOTS_MAX {
            return Err(invalid("too many read roots"));
        }
        granted_root(
            root,
            &owner_home()?,
            &self.secret_paths(),
            &(self.hooks.access.harness_folders)(),
        )?;
        self.read_roots.push(root.to_path_buf());
        Ok(self)
    }

    /// Lets a Node script harness find the `node` beside it, as its sign-in does: under an npm
    /// prefix ([`crate::harnesses::node_prefix`]) only `bin`, the harness's package folder and
    /// the real file `bin/node` links to become readable, Seatbelt denying credential names
    /// below each folder, and `bin` goes first on PATH; a prefix or node in the project or
    /// agent-writable is skipped. Input: the canonical program. Output: the sandbox, or a refusal.
    pub fn allow_node_for(mut self, program: &Path) -> Result<Self, SandboxError> {
        let Some(prefix) = (self.hooks.node_prefix)(program) else {
            return Ok(self);
        };
        let Some(package) = node_package(&prefix, program) else {
            return Ok(self);
        };
        let folders = [canonical(&prefix.join("bin"))?, canonical(&package)?];
        let node = canonical(&prefix.join("bin").join("node"))?;
        let agents = folders.iter().chain([&node]).any(|path| {
            (self.hooks.in_crazyagent_folder)(path)
                || path.starts_with(&self.project_root)
                || self
                    .writable_roots()
                    .any(|root| path.starts_with(root) || root.starts_with(path))
        });
        if agents {
            return Ok(self);
        }
        for folder in folders {
            if !self.reads(&folder) {
                self = self.allow_node_folder(&folder)?;
            }
        }
        if !self.reads(&node) {
            self = self.allow_node_file(&node)?;
        }
        self.node_bin = Some(prefix.join("bin"));
        assert!(self.node_bin.as_deref().is_some_and(Path::is_absolute));
        Ok(self)
    }

    /// Lets a script harness's interpreters run as the tool resolver found them
    /// ([`kernel_processes::finding::Resolved::grants`]), such as the `node` an npm-built Codex asks `env`
    /// for from nvm, volta or Homebrew: each interpreter's folder becomes readable as a
    /// toolchain folder unless a system or granted folder covers it. Input: the resolved
    /// harness. Output: the sandbox, or a refusal for an interpreter in the project or in a
    /// folder the agent can write.
    pub fn allow_interpreters(
        mut self,
        resolved: &kernel_processes::finding::Resolved,
    ) -> Result<Self, SandboxError> {
        for file in resolved.grants().into_iter().skip(1) {
            let folder = file
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| invalid("an interpreter must sit in a folder"))?;
            let agents = (self.hooks.in_crazyagent_folder)(&folder)
                || folder.starts_with(&self.project_root)
                || self
                    .writable_roots()
                    .any(|root| folder.starts_with(root) || root.starts_with(&folder));
            if agents {
                return Err(invalid("an interpreter sits where the agent can write"));
            }
            let covered = SYSTEM_READ_ROOTS
                .iter()
                .map(Path::new)
                .chain(self.read_roots.iter().map(PathBuf::as_path))
                .chain(self.tool_roots.iter().map(PathBuf::as_path))
                .any(|root| folder.starts_with(root));
            if !covered {
                self = self.allow_node_folder(&folder)?;
            }
        }
        Ok(self)
    }

    /// Whether a system, granted or toolchain folder already makes a path readable. Input: an
    /// absolute path. Output: the answer.
    fn reads(&self, path: &Path) -> bool {
        SYSTEM_READ_ROOTS
            .iter()
            .map(Path::new)
            .chain(self.read_roots.iter().map(PathBuf::as_path))
            .chain(self.tool_roots.iter().map(PathBuf::as_path))
            .any(|root| path.starts_with(root))
    }

    /// Grants the real `node` an npm prefix links to, such as the engine's own Node in its
    /// setup folder, read-only and runnable: under Seatbelt as one toolchain file, elsewhere
    /// its folder as a read root. Input: the canonical existing file. Output: the widened
    /// sandbox, or a refusal when it fails the toolchain file or read root checks.
    fn allow_node_file(mut self, node: &Path) -> Result<Self, SandboxError> {
        if !cfg!(target_os = "macos") {
            let folder = node
                .parent()
                .ok_or_else(|| invalid("node must sit in a folder"))?;
            return self.allow_read(folder);
        }
        if self.tool_files.iter().any(|file| file == node) {
            return Ok(self);
        }
        if self.tool_files.len() >= TOOL_FILES_MAX {
            return Err(invalid("too many toolchain files"));
        }
        clear_of_secrets(node, &self.secret_paths())?;
        if !clean_absolute(node) || !node.is_file() || canonical(node)? != *node {
            return Err(invalid("toolchain files must be existing canonical files"));
        }
        self.tool_files.push(node.to_path_buf());
        assert!(self.tool_files.len() <= TOOL_FILES_MAX);
        Ok(self)
    }

    /// Grants one folder of a Node script harness read-only: under Seatbelt as a toolchain
    /// folder, so every credential name below it stays denied, elsewhere as a read root.
    /// Input: a canonical existing folder. Output: the widened sandbox, or a refusal when the
    /// folder fails [`Self::allow_read`]'s checks or the toolchain folders are full.
    fn allow_node_folder(mut self, folder: &Path) -> Result<Self, SandboxError> {
        if !cfg!(target_os = "macos") {
            return self.allow_read(folder);
        }
        if self.tool_roots.len() >= TOOL_ROOTS_MAX {
            return Err(invalid("too many toolchain folders"));
        }
        granted_root(
            folder,
            &owner_home()?,
            &self.secret_paths(),
            &(self.hooks.access.harness_folders)(),
        )?;
        self.tool_roots.push(folder.to_path_buf());
        assert!(self.tool_roots.len() <= TOOL_ROOTS_MAX);
        Ok(self)
    }

    /// Lets the agent follow one folder link that leads to a read root it already has, such
    /// as the version link uv keeps beside a managed Python: only the link's own text becomes
    /// readable, and no deny gives way to it, since the profile writes it before every deny.
    /// Input: a clean absolute link in a real folder, outside the hidden, kept-out and
    /// [`Self::secret_paths`], leading to a read root. Output: the sandbox, or `Invalid`.
    pub fn allow_read_link(mut self, link: &Path) -> Result<Self, SandboxError> {
        if self.links.len() >= LINKS_MAX {
            return Err(invalid("too many links to read roots"));
        }
        clear_of_secrets(link, &self.secret_paths())?;
        let is_link =
            std::fs::symlink_metadata(link).is_ok_and(|metadata| metadata.file_type().is_symlink());
        let in_real_folder = link
            .parent()
            .is_some_and(|parent| canonical(parent).is_ok_and(|real| real == parent));
        if !clean_absolute(link) || !is_link || !in_real_folder {
            return Err(invalid(
                "a link to a read root must be a clean absolute link in a real folder",
            ));
        }
        let denied = self
            .hidden
            .iter()
            .chain(self.kept_out.iter())
            .any(|path| link.starts_with(path));
        if denied {
            return Err(invalid(
                "a link to a read root must lie outside every hidden and kept-out path",
            ));
        }
        let target = canonical(link)?;
        assert!(target.is_absolute());
        if !self.read_roots.contains(&target) {
            return Err(invalid("a link must lead to a read root already granted"));
        }
        self.links.push(link.to_path_buf());
        assert!(self.links.len() <= LINKS_MAX);
        Ok(self)
    }

    /// Lets the agent start one exact program the engine itself launches, such as its
    /// harness or the engine executable, from an app bundle's `Contents/MacOS`, which no
    /// agent may start otherwise (OP-24). One inside a folder the agent can write stays
    /// denied, since the agent could replace it. Input: a canonical existing regular file
    /// clear of [`Self::secret_paths`]. Output: the widened sandbox, or `Invalid`.
    pub fn allow_exec(mut self, program: &Path) -> Result<Self, SandboxError> {
        clear_of_secrets(program, &self.secret_paths())?;
        if !clean_absolute(program) || !program.is_file() || canonical(program)? != *program {
            return Err(invalid(
                "an allowed program must be an existing canonical file",
            ));
        }
        if self.execs.iter().any(|exec| exec == program) {
            return Ok(self);
        }
        if self.execs.len() >= EXECS_MAX {
            return Err(invalid("too many allowed programs"));
        }
        self.execs.push(program.to_path_buf());
        Ok(self)
    }

    /// Lets the agent run the engine executable that serves its team tools, relay hooks and
    /// monitor: the engine's folder read-only and, by [`Self::allow_exec`], the file itself
    /// startable by exact path, even from `CrazyAgent.app/Contents/MacOS` where the macOS
    /// service runs it (OP24R2-1). Input: the engine's canonical path. Output: the widened
    /// sandbox, or `Invalid`.
    pub fn allow_engine(self, engine: &Path) -> Result<Self, SandboxError> {
        let folder = engine
            .parent()
            .ok_or_else(|| invalid("the engine must sit in a folder"))?;
        self.allow_read(folder)?.allow_exec(engine)
    }

    /// Grants a project's toolchains read-only (see [`kernel_sandbox::toolchains::Toolchains::detect`]):
    /// each folder as [`Self::allow_read`] checks it, with every credential name below it
    /// denied, and each file when it is a canonical regular file clear of
    /// [`Self::secret_paths`]. Only Seatbelt grants them; the Linux and Windows boundaries
    /// leave them out. Input: the toolchains. Output: the widened sandbox, or `Invalid`.
    pub fn allow_tools(
        mut self,
        tools: &kernel_sandbox::toolchains::Toolchains,
    ) -> Result<Self, SandboxError> {
        let owner = owner_home()?;
        let secrets = self.secret_paths();
        for root in tools.roots() {
            if self.tool_roots.contains(root) {
                continue;
            }
            if self.tool_roots.len() >= TOOL_ROOTS_MAX {
                return Err(invalid("too many toolchain folders"));
            }
            granted_root(
                root,
                &owner,
                &secrets,
                &(self.hooks.access.harness_folders)(),
            )?;
            self.tool_roots.push(root.clone());
        }
        for file in tools.files() {
            if self.tool_files.contains(file) {
                continue;
            }
            if self.tool_files.len() >= TOOL_FILES_MAX {
                return Err(invalid("too many toolchain files"));
            }
            clear_of_secrets(file, &secrets)?;
            if !clean_absolute(file) || !file.is_file() || canonical(file)? != *file {
                return Err(invalid("toolchain files must be existing canonical files"));
            }
            self.tool_files.push(file.clone());
        }
        assert!(self.tool_roots.len() <= TOOL_ROOTS_MAX);
        assert!(self.tool_files.len() <= TOOL_FILES_MAX);
        Ok(self)
    }

    /// Grants the owner's extra read-only folders for this project (see
    /// [`kernel_sandbox::extra_paths::checked`]) as toolchain folders are granted, with credential names
    /// carved out, skipping each one that is gone or no longer passes
    /// [`Self::allow_read`]'s checks. Input: the saved folders. Output: the widened sandbox
    /// and how many folders were skipped.
    pub fn allow_extra_reads(mut self, paths: &[PathBuf]) -> Result<(Self, usize), SandboxError> {
        let owner = owner_home()?;
        let secrets = self.secret_paths();
        let mut skipped = 0;
        for path in paths {
            if self.tool_roots.contains(path) {
                continue;
            }
            if self.tool_roots.len() >= TOOL_ROOTS_MAX
                || granted_root(
                    path,
                    &owner,
                    &secrets,
                    &(self.hooks.access.harness_folders)(),
                )
                .is_err()
            {
                skipped += 1;
                continue;
            }
            self.tool_roots.push(path.clone());
        }
        assert!(skipped <= paths.len() && self.tool_roots.len() <= TOOL_ROOTS_MAX);
        Ok((self, skipped))
    }

    /// Grants the agent's tool cache, made by `tool_cache::prepare`, as one more writable
    /// root: a non-working-tree root under the SM1-R rule, so no `.git` entry can be made,
    /// moved or written there, and the root of `pnpm_config_store_dir`, and of `UV_CACHE_DIR`
    /// when its uv markers are in place (T8). Inputs: the cache root, a canonical existing
    /// folder clear of [`Self::secret_paths`] and of every other root, and whether its uv
    /// markers are in place. Output: the widened sandbox, or `Invalid` for an unsafe or second
    /// cache.
    pub fn allow_tool_cache(mut self, root: &Path, uv: bool) -> Result<Self, SandboxError> {
        if self.tool_cache.is_some() {
            return Err(invalid("the sandbox already has a tool cache"));
        }
        granted_root(
            root,
            &owner_home()?,
            &self.secret_paths(),
            &(self.hooks.access.harness_folders)(),
        )?;
        let overlaps = self
            .writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
            .chain([&self.project_root])
            .any(|other| other.starts_with(root) || root.starts_with(other));
        if overlaps {
            return Err(invalid("the tool cache must lie outside every other root"));
        }
        self.tool_cache = Some((root.to_path_buf(), uv));
        assert!(self.writes(root));
        Ok(self)
    }

    /// The program `name` the agent's PATH finds first, if the engine may run it for the
    /// agent (T8 C3), as [`Self::trusted_program`] decides. Input: a plain program name.
    /// Output: the canonical path, or None.
    pub fn tool_program(&self, name: &str) -> Option<PathBuf> {
        assert!(!name.is_empty() && !name.contains('/'));
        let homes = kernel_sandbox::toolchains::ToolHomes::at(&self.owner_home);
        let path = kernel_sandbox::toolchains::agent_path(&homes, AGENT_PATH);
        let found = std::env::split_paths(&path)
            .map(|folder| folder.join(name))
            .find(|candidate| candidate.is_file())?;
        self.trusted_program(&found)
    }

    /// A found program the engine may run for the agent (T8 C3): its canonical path when
    /// that lies in a read-only system or granted tool root and in no root the agent can
    /// write, so the agent cannot swap the program the engine runs. Input: the path found.
    /// Output: the canonical path, or None.
    pub fn trusted_program(&self, found: &Path) -> Option<PathBuf> {
        let program = canonical(found).ok()?;
        let read_only = SYSTEM_READ_ROOTS
            .iter()
            .map(Path::new)
            .chain(self.tool_roots.iter().map(PathBuf::as_path))
            .any(|root| program.starts_with(root));
        let writable = self
            .writable_roots()
            .any(|root| program.starts_with(root) || root.starts_with(&program));
        (read_only && !writable && program.is_file()).then_some(program)
    }

    /// Adds the agent's short writable temp folder (see [`kernel_sandbox::toolchains::make_tmp`]), which
    /// becomes its `TMPDIR`. Input: a canonical existing folder clear of
    /// [`Self::secret_paths`]. Output: the widened sandbox, or `Invalid` for an unsafe or
    /// second folder.
    pub fn allow_scratch(mut self, tmp: &Path) -> Result<Self, SandboxError> {
        if self.scratch.is_some() {
            return Err(invalid("the sandbox already has a temp folder"));
        }
        granted_root(
            tmp,
            &owner_home()?,
            &self.secret_paths(),
            &(self.hooks.access.harness_folders)(),
        )?;
        self.scratch = Some(tmp.to_path_buf());
        Ok(self)
    }

    /// Keeps one folder inside a granted root out of the agent's reach, read and write, as
    /// the secret paths are; the graph step hides the project's `.crazyagent/` from graphify.
    /// Input: a clean absolute path below a root, in a real parent folder, absent or a real
    /// folder. Output: the narrowed sandbox, or `Invalid`. Seatbelt holds it, the Linux
    /// bubblewrap backends too where no second mount shows it; plain Landlock and Windows refuse.
    pub fn hide(mut self, folder: &Path) -> Result<Self, SandboxError> {
        if self.hidden.len() >= HIDDEN_MAX {
            return Err(invalid("too many hidden folders"));
        }
        let inside = self
            .writable_roots()
            .chain(self.read_roots.iter())
            .any(|root| folder.starts_with(root) && folder != root.as_path());
        let parent = folder.parent().filter(|parent| {
            parent
                .canonicalize()
                .is_ok_and(|real| real.as_path() == *parent)
        });
        let plain = fs::symlink_metadata(folder).map_or_else(
            |error| error.kind() == io::ErrorKind::NotFound,
            |metadata| metadata.is_dir() && !metadata.file_type().is_symlink(),
        );
        if !clean_absolute(folder) || !inside || parent.is_none() || !plain {
            return Err(invalid(
                "a hidden folder must be a plain folder path inside a granted root",
            ));
        }
        self.hidden.push(folder.to_path_buf());
        Ok(self)
    }

    /// Keeps the secret files and folders of the project folder `root` closed to the agent
    /// (DEC-147.9): contents, attribute values, writes, runs, links and clones of every path
    /// there with a name in [`kernel_sandbox::shared_folders::SECRET_NAMES`], in any letter case, are denied,
    /// names and sizes stay visible, except under `root/.git` and inside the agent's own
    /// writable roots below it, its worktree or clone, never `root` itself. Each of `revealed`, the secrets the owner
    /// shared back, is readable again, never writable. The deny comes before every older
    /// deny in the profile, so none of those gives way. Inputs: a clean canonical folder the
    /// sandbox already reads and at most [`kernel_sandbox::shared_folders::SECRETS_MAX`] clean paths strictly
    /// inside it. Output: the sandbox, or `Invalid` for a second call or a path out of shape.
    pub fn keep_project_secrets(
        mut self,
        root: &Path,
        revealed: &[PathBuf],
    ) -> Result<Self, SandboxError> {
        if self.project_secrets.is_some() {
            return Err(invalid("the project's secrets are already kept closed"));
        }
        let readable = self
            .read_roots
            .iter()
            .chain([&self.project_root])
            .any(|read| read == root);
        if !clean_absolute(root) || !readable || canonical(root)? != root {
            return Err(invalid(
                "the folder whose secrets stay closed must be a canonical folder the sandbox reads",
            ));
        }
        if revealed.len() > kernel_sandbox::shared_folders::SECRETS_MAX
            || !revealed.iter().all(|path| {
                clean_absolute(path)
                    && path.starts_with(root)
                    && path != root
                    && !path.starts_with(root.join(".git"))
            })
        {
            return Err(invalid(
                "a shared-back secret must be a clean path inside the project folder",
            ));
        }
        self.project_secrets = Some(root.to_path_buf());
        self.secrets_revealed = revealed.to_vec();
        Ok(self)
    }

    /// Keeps every top-level entry of the [`Self::keep_project_secrets`] folder closed as
    /// [`Self::keep_out`] does but `open`, so one made after launch stays closed (L4 r1 L1);
    /// own writable roots below it stay open, never the folder itself. Input: at most
    /// [`OPEN_TOPS_MAX`] names, each [`plain_top`]. Output: the narrowed sandbox, or `Invalid`
    /// without that folder, for a second call or for a name out of shape.
    pub fn open_only_tops(mut self, open: &[String]) -> Result<Self, SandboxError> {
        if self.project_secrets.is_none() || self.open_tops.is_some() {
            return Err(invalid(
                "the open top-level names need the project folder, once",
            ));
        }
        if open.len() > OPEN_TOPS_MAX || !open.iter().all(|name| plain_top(name)) {
            return Err(invalid("an open top-level name must be one plain name"));
        }
        self.open_tops = Some(open.to_vec());
        Ok(self)
    }

    /// Lets the agent write one shared top-level folder of the project folder that git does
    /// not track (DEC-147.9 Read-write) as one more writable root, pinned in place; the secret
    /// names inside it stay closed, and every older deny still wins. Never a hidden folder, an
    /// engine folder or one named `context` ([`kernel_sandbox::shared_folders::never_written`]). Input: a canonical existing folder
    /// directly inside the folder [`Self::keep_project_secrets`] named, clear of
    /// [`Self::secret_paths`] and of every other writable root. Output: the widened sandbox, or
    /// `Invalid`; only Seatbelt holds it.
    pub fn allow_shared_write(mut self, folder: &Path) -> Result<Self, SandboxError> {
        if self.shared_writes.len() >= kernel_sandbox::shared_folders::SHARED_WRITES_MAX {
            return Err(invalid("too many shared writable folders"));
        }
        let in_project = self
            .project_secrets
            .as_ref()
            .is_some_and(|root| folder.parent() == Some(root.as_path()));
        let kept_read_only = match (self.project_secrets.as_ref(), folder.file_name()) {
            (Some(root), Some(name)) => name.to_str().is_none_or(|name| {
                kernel_sandbox::shared_folders::never_written(&ACCESS_HOOKS, root, name)
            }),
            _ => true,
        };
        if !in_project || kept_read_only {
            return Err(invalid(
                "a shared writable folder must be a top-level project folder kept under its secret rule",
            ));
        }
        granted_root(
            folder,
            &owner_home()?,
            &self.secret_paths(),
            &(self.hooks.access.harness_folders)(),
        )?;
        if self
            .writable_roots()
            .any(|root| root.starts_with(folder) || folder.starts_with(root))
        {
            return Err(invalid(
                "a shared writable folder must lie outside every other writable root",
            ));
        }
        self.shared_writes.push(folder.to_path_buf());
        assert!(self.writes(folder));
        assert!(self.shared_writes.len() <= kernel_sandbox::shared_folders::SHARED_WRITES_MAX);
        Ok(self)
    }

    /// The Seatbelt rule of [`Self::open_only_tops`]: every path below the project folder is
    /// kept out but the open top-level names and the agent's own writable roots there.
    /// Output: the rule, empty when no names were set.
    fn closed_tops_rule(&self) -> String {
        let (Some(root), Some(open)) = (self.project_secrets.as_ref(), self.open_tops.as_ref())
        else {
            return String::new();
        };
        assert!(clean_absolute(root));
        let pattern = seatbelt_pattern(root);
        let mut text = format!("{KEPT_OUT_DENY}\n  (require-all\n    (regex #\"^{pattern}/\")");
        let names: Vec<String> = open.iter().map(|name| regex_escape(name)).collect();
        for suffix in ["$", "/"] {
            for body in regex_bodies(&format!("^{pattern}/"), &names, suffix) {
                text.push_str(&format!("\n    (require-not (regex #\"{body}\"))"));
            }
        }
        for own in self.writable_roots().filter(|own| {
            own.starts_with(root) && own.as_path() != root.as_path() && clean_absolute(own)
        }) {
            text.push_str(&format!(
                "\n    (require-not (subpath \"{}\"))",
                own.display()
            ));
        }
        text.push_str("))\n");
        text
    }

    /// The Seatbelt rules of [`Self::keep_project_secrets`]: the deny of secret names in the
    /// project folder outside `.git` and the agent's own writable roots there (never the
    /// shared writable folders); no clone of any folder, and no move or removal of a folder
    /// inside a shared writable folder, so no folder that holds a secret leaves the name rule
    /// by a plain-named parent (L4 r1 H2); then the read allow of env templates
    /// ([`kernel_sandbox::shared_folders::SECRET_TEMPLATES`]) and of each shared-back secret. Output: the
    /// rules, empty when no folder was named.
    fn project_secret_rules(&self) -> String {
        let Some(root) = self
            .project_secrets
            .as_ref()
            .filter(|root| clean_absolute(root))
        else {
            return String::new();
        };
        let names: Vec<String> = kernel_sandbox::shared_folders::SECRET_NAMES
            .iter()
            .map(|name| fold_case(name))
            .collect();
        let own: String = self
            .writable_roots()
            .filter(|own| {
                own.starts_with(root)
                    && own.as_path() != root.as_path()
                    && !self.shared_writes.contains(own)
                    && clean_absolute(own)
            })
            .map(|own| format!("\n    (require-not (subpath \"{}\"))", own.display()))
            .collect();
        let prefix = format!("^{}/(.*/)?", seatbelt_pattern(root));
        let mut text = String::new();
        for body in regex_bodies(&prefix, &names, "(/|$)") {
            text.push_str(&format!(
                "{KEPT_OUT_DENY}\n  (require-all\n    (regex #\"{body}\")\n    (require-not (subpath \"{}/.git\")){own}))\n",
                root.display()
            ));
        }
        text.push_str("(deny file-clone (vnode-type DIRECTORY))\n");
        for folder in self
            .shared_writes
            .iter()
            .filter(|folder| clean_absolute(folder))
        {
            text.push_str(&format!(
                "(deny file-write-unlink\n  (require-all\n    (subpath \"{}\")\n    (vnode-type DIRECTORY)))\n",
                folder.display()
            ));
        }
        text.push_str(&format!(
            "(allow file-read-data file-read-xattr\n  (regex #\"^{}/(.*/)?{}$\"))\n",
            seatbelt_pattern(root),
            fold_case(kernel_sandbox::shared_folders::SECRET_TEMPLATES)
        ));
        let revealed: Vec<&PathBuf> = self
            .secrets_revealed
            .iter()
            .filter(|path| clean_absolute(path))
            .collect();
        if !revealed.is_empty() {
            text.push_str("(allow file-read-data file-read-xattr");
            for path in revealed {
                text.push_str(&format!("\n  (subpath \"{}\")", path.display()));
            }
            text.push_str(")\n");
        }
        text
    }

    /// The project folder whose secret names this sandbox keeps closed, if any.
    pub fn project_secrets(&self) -> Option<&Path> {
        self.project_secrets.as_deref()
    }

    /// The shared top-level project folders this sandbox lets the agent write.
    pub fn shared_writes(&self) -> &[PathBuf] {
        &self.shared_writes
    }

    /// Holds the project's agent folders choice in this sandbox (DEC-124): a Sandboxed IDE
    /// terminal keeps to what the owner chose for the IDE. Only Seatbelt holds it, so the
    /// Linux and Windows boundaries refuse a sandbox with a choice other than Allow all
    /// changes. Input: the rule. Output: the sandbox.
    pub fn protect_agent_files(mut self, rule: kernel_sandbox::policy::AgentFilesRule) -> Self {
        self.agent_files = Some(rule);
        self
    }

    /// Marks the home as one the owner keeps for their own tools, which no launch resets, as
    /// the IDE terminal's and the IDE server's are: the profile leaves out
    /// [`agent_config_denies`], so `npm login`, `git config --global` and `cargo install`
    /// write there and copies keep their flags (RV-DS241-188, -187), and [`Self::install`]
    /// makes no folder there, as [`owner_terminal_env`] leaves out [`AGENT_HOME_ENV`].
    /// Output: the sandbox.
    pub fn keep_owner_config(mut self) -> Self {
        self.owner_config = true;
        self
    }

    /// Names the engine's data root the agent home is judged by, as a master turn's home may
    /// be a configured RCLI home the owner chose in one of their own folders (never HOME
    /// itself or a folder holding it, which [`Self::new`] refuses): [`Self::install`] makes
    /// and removes nothing in a home not strictly below the root, so a link a dotfile manager
    /// made there stays, and clears what holds a config folder's name in one that is
    /// (rv-code2 b8 F1). The profile keeps every deny either way. Input: the canonical data
    /// root. Output: the sandbox.
    #[cfg(unix)]
    pub fn anchor_home(mut self, engine_root: &Path) -> Self {
        assert!(engine_root.is_absolute());
        let below = self.agent_home != engine_root && self.agent_home.starts_with(engine_root);
        self.engine_home = Some(below);
        self
    }

    /// Keeps one file or folder inside a granted root from being read, written or run: its
    /// name, size and dates stay visible, its contents, folder listing and attribute values
    /// do not, so a tool that stats an import does not fail; the graph step keeps the
    /// project's secret-like and git-ignored paths from graphify (MAP-2). Input: a clean
    /// absolute path below a root, of any kind or absent. Output: the narrowed sandbox, or
    /// `Invalid` past [`KEPT_OUT_MAX`]. Only Seatbelt holds it, so the Linux and Windows
    /// boundaries refuse such a sandbox.
    pub fn keep_out(mut self, path: &Path) -> Result<Self, SandboxError> {
        if self.kept_out.len() >= KEPT_OUT_MAX {
            return Err(invalid("too many paths kept out"));
        }
        let inside = self
            .writable_roots()
            .chain(self.read_roots.iter())
            .any(|root| path.starts_with(root) && path != root.as_path());
        if !clean_absolute(path) || !inside {
            return Err(invalid(
                "a path kept out must be a clean absolute path inside a granted root",
            ));
        }
        self.kept_out.push(path.to_path_buf());
        Ok(self)
    }

    /// Whether [`Self::keep_out`] takes the form of `path`, a clean absolute path, so a
    /// caller can check every path before it makes anything. Input: the path. Output: the
    /// answer; a path kept out must still lie inside a granted root.
    pub fn keepable(path: &Path) -> bool {
        clean_absolute(path)
    }

    /// Lets a native-mode lead run its own tmux server (DEC-104): an exact socket grant, as
    /// `allow_unix_socket` gives, for its own socket in its short temp folder, and the tmux
    /// program itself, readable and startable as one file, never its folder: the bundled one
    /// sits in the app's `Contents/MacOS` beside the engine (DEC-157). No other Unix
    /// socket becomes reachable, the engine's own tmux server included, and the server and
    /// every engine client on it stay inside this profile; its teammates run in a server of
    /// their own (see [`Self::serve_tmux_server`]). Only Seatbelt holds it. Input: the socket
    /// (`terminal::harness_socket`) and the canonical tmux program. Output: the widened
    /// sandbox, `Invalid`, or `Unavailable` off macOS.
    pub fn allow_tmux_server(self, socket: &Path, tmux: &Path) -> Result<Self, SandboxError> {
        self.tmux_server(socket, tmux, false)
    }

    /// Lets a native lead's teammates run their own tmux server (rv-audit S1 F1): as
    /// [`Self::allow_tmux_server`], but the socket is served only, as
    /// [`Self::serve_unix_socket`] gives, so no teammate pane the server forks connects to
    /// it, unlinks it or binds a server of its own there. Input: the socket
    /// (`terminal::teammates_socket`) and the canonical tmux program. Output: the widened
    /// sandbox, `Invalid`, or `Unavailable` off macOS.
    pub fn serve_tmux_server(self, socket: &Path, tmux: &Path) -> Result<Self, SandboxError> {
        self.tmux_server(socket, tmux, true)
    }

    /// Splits a native-mode lead's sandbox into the lead's and its teammates' (DEC-104,
    /// rv-audit S1 F1, TTDL F1). The lead runs its own tmux server at `own` and only reaches
    /// the engine's caller socket at `callers` and its teammates' server at `mates`. The
    /// teammates serve `mates`, only reach `callers` and are shut out of `own`, so no agent
    /// binds, blocks or replaces the caller socket the engine answers on (DEC-111 Q18,
    /// rv-secreview RH-1) and no teammate holds a listener or places a node where the lead
    /// connects. They read but never write the launch folders the engine installs in the
    /// lead's home, its hooks, skills and relay plugin, which the lead's Claude Code runs
    /// (TPH), nor move, replace or re-permission that home itself (TPH r1 F1), nor the
    /// shell snapshot and session scripts its Bash commands run first (TPH-2). The teammates
    /// run in their own home and only read the lead's, except the team folders Claude Code
    /// shares, where they make no link or folder but a lock, and never write the `git` on the
    /// lead's path (TPH-2 C). They neither read nor write the lead's own temp folder in the
    /// short temp folder they share ([`kernel_sandbox::toolchains::native_lead_tmp`], TPH-2 residual), so no
    /// teammate reads, races or plants a file the lead stages there. Both are shut out of the
    /// name the engine binds the caller socket at before it moves it over `callers`, so
    /// neither swaps its own listener in there (rv-secreview RH2R2-1). The lead's part of the
    /// relay mailbox is hidden from the teammates but for its `relay.json`, which they only
    /// read, and they never move or re-permission the mailbox or the temp folder holding it
    /// (relay mailbox forgery C1). Neither the lead nor a teammate moves the tool mailbox, so
    /// no answer is written outside the rule that keeps answers the engine's. Inputs: the
    /// three sockets, the canonical tmux program and
    /// the teammates' home beside the lead's, its team folders already made, with the
    /// mailbox in the agent temp folder. Output: the lead's and the teammates' sandboxes,
    /// `Invalid`, or `Unavailable` off macOS.
    #[cfg(unix)]
    pub fn native_team(
        self,
        own: &Path,
        mates: &Path,
        callers: &Path,
        tmux: &Path,
        mates_home: &Path,
    ) -> Result<(Self, Self), SandboxError> {
        let home = self.agent_home.clone();
        granted_root(
            mates_home,
            &self.owner_home,
            &self.secret_paths(),
            &(self.hooks.access.harness_folders)(),
        )?;
        if mates_home.starts_with(&home) || home.starts_with(mates_home) {
            return Err(invalid("the teammates' home must sit beside the lead's"));
        }
        let shared: Vec<PathBuf> = CLAUDE_TEAM_SHARED
            .iter()
            .map(|folder| home.join(folder))
            .collect();
        for folder in &shared {
            granted_root(
                folder,
                &self.owner_home,
                &self.secret_paths(),
                &(self.hooks.access.harness_folders)(),
            )?;
        }
        let binding = (self.hooks.binding_path)(callers);
        let mailbox = self.agent_tmp.join(self.hooks.mailbox_folder);
        let lead_part = mailbox.join(self.hooks.relay_lead_part);
        let mut teammates = self
            .clone()
            .serve_tmux_server(mates, tmux)?
            .reach_unix_socket(callers)?
            .shut_unix_socket(own)?
            .shut_unix_socket(&binding)?
            .allow_read(&home)?
            .hide(&lead_part)?;
        let lead_tmp = teammates
            .scratch
            .as_deref()
            .map(kernel_sandbox::toolchains::native_lead_tmp)
            .ok_or_else(|| invalid("a native team needs the agent temp folder"))?;
        teammates = teammates.hide(&lead_tmp)?;
        teammates.agent_home = mates_home.to_path_buf();
        teammates.revealed = vec![lead_part.join(self.hooks.relay_roster_file)];
        teammates.read_only = vec![
            home.join(self.hooks.launch_state),
            home.join(self.hooks.relay_plugin_folder),
        ];
        teammates.read_only.extend(
            self.hooks
                .claude_bash_preludes
                .iter()
                .map(|folder| home.join(folder)),
        );
        teammates.read_only.extend(
            self.scratch
                .iter()
                .map(|scratch| scratch.join(kernel_sandbox::toolchains::GIT_FOLDER)),
        );
        let answers = mailbox.clone();
        teammates.answers = Some(answers.clone());
        teammates.fixed = vec![home.clone(), self.agent_tmp.clone(), mailbox];
        teammates.fixed.extend(shared.iter().cloned());
        teammates.shared = shared;
        teammates.teammates = true;
        if teammates.writes(&home) {
            return Err(invalid("a teammates' writable root holds the lead's home"));
        }
        let mut lead = self
            .allow_tmux_server(own, tmux)?
            .reach_unix_socket(callers)?
            .reach_unix_socket(mates)?
            .shut_unix_socket(&binding)?;
        lead.fixed.push(answers.clone());
        lead.answers = Some(answers);
        Ok((lead, teammates))
    }

    /// The writes no agent of a native team may make to the engine's answers in the tool
    /// mailbox the team shares, a reply or the part file it is written under, in any spelling
    /// the volume folds together. An agent may still remove a reply, as its tool server does
    /// once it has read one. Output: the rule text, empty for any other profile.
    fn answers_denies(&self) -> String {
        let Some(mailbox) = &self.answers else {
            return String::new();
        };
        let rule = format!(
            "(regex #\"^{}/[^/]+({}|{})$\")",
            seatbelt_pattern(mailbox),
            any_case(self.hooks.reply_suffix),
            any_case(self.hooks.answer_part_suffix),
        );
        format!("(deny file-write* {rule})\n(allow file-write-unlink {rule})\n")
    }

    /// The writes a native lead's teammates may not make in the lead's worktree (OP-9,
    /// OP-9b), none for any other profile: in the lead's own clone, those
    /// [`teammates_clone_denies`] names; in a linked worktree, anything but the teammates'
    /// folder, and the lead's worktree record and the object store, since the engine
    /// commits the teammates' turns, nor the teammates' folder entry, nor make, move or
    /// remove anything directly in it, such as a teammate's folder, where the engine's
    /// setup git adds each teammate worktree (OP-9 r2, OP9B-C1). Output: the rule text,
    /// empty when none applies.
    fn teammates_denies(&self) -> String {
        if !self.teammates {
            return String::new();
        }
        match (&self.clone, &self.checkout) {
            (Some(clone), _) => {
                teammates_clone_denies(self.hooks, clone, self.lead_branch.as_deref())
            }
            (None, Some(checkout)) => {
                let worktree = checkout.worktree.display();
                let root = seatbelt_pattern(&checkout.worktree);
                let teammates = any_case(self.hooks.teammates_folder);
                let text = format!(
                    "(deny file-write*\n  (require-all\n    (subpath \"{worktree}\")\n    (require-not (subpath \"{worktree}/{}\"))))\n(deny file-write*\n  (subpath \"{}\")\n  (subpath \"{}\")\n  (regex #\"^{root}/{teammates}$\"))\n(deny file-write-create file-write-unlink\n  (regex #\"^{root}/{teammates}/[^/]+$\"))\n",
                    self.hooks.teammates_folder,
                    checkout.admin.display(),
                    checkout.objects.display()
                );
                assert_eq!(text.matches("(subpath \"/").count(), 4);
                text
            }
            (None, None) => String::new(),
        }
    }

    /// Grants one tmux server in the agent temp folder, served and reached or served only.
    /// Takes the socket, the canonical tmux program and whether the socket is served only.
    /// Returns the widened sandbox, `Invalid`, or `Unavailable` off macOS.
    fn tmux_server(
        mut self,
        socket: &Path,
        tmux: &Path,
        served_only: bool,
    ) -> Result<Self, SandboxError> {
        if !cfg!(target_os = "macos") {
            return Err(SandboxError::Unavailable(TMUX_SERVER_NEEDS_SEATBELT.into()));
        }
        let scratch = self
            .scratch
            .clone()
            .ok_or_else(|| invalid("a tmux server needs the agent temp folder"))?;
        if self
            .sockets
            .iter()
            .chain(&self.served)
            .any(|known| known.parent() == Some(scratch.as_path()))
        {
            return Err(invalid("the sandbox already has a tmux server"));
        }
        if !clean_absolute(socket)
            || socket.parent() != Some(scratch.as_path())
            || socket.as_os_str().len() > SOCKET_PATH_BYTES_MAX
        {
            return Err(invalid("the tmux socket must sit in the agent temp folder"));
        }
        clear_of_secrets(socket, &self.secret_paths())?;
        if !clean_absolute(tmux) || !tmux.is_file() || canonical(tmux)? != tmux {
            return Err(invalid("the tmux program must be a canonical file"));
        }
        if !self.tool_files.iter().any(|file| file == tmux) {
            if self.tool_files.len() >= TOOL_FILES_MAX {
                return Err(invalid("too many toolchain files"));
            }
            self.tool_files.push(tmux.to_path_buf());
        }
        self = self.allow_exec(tmux)?;
        assert!(self.tool_files.iter().any(|file| file == tmux));
        self = if served_only {
            self.serve_unix_socket(socket)?
        } else {
            self.allow_unix_socket(socket)?
        };
        assert!(self.writes(socket.parent().unwrap_or(socket)));
        Ok(self)
    }

    /// Marks the project folder as one only the engine's own process works in under this sandbox:
    /// a folder it made for one job, a plugin server's check, a context-mode install, or a project
    /// owner git runs in, which must write `.git` and which plain owner git writes in full anyway.
    /// On Windows it gets no project guards, its top writable (DEC-193); Seatbelt and Landlock are
    /// unchanged. Input: a sandbox confined to no worktree, clone or setup. Output: it, marked.
    pub fn as_job_folder(mut self) -> Self {
        assert_eq!(self.confinements(), 0);
        assert!(!self.job_folder);
        self.job_folder = true;
        self
    }

    /// Whether the project folder holds the project guards of [`Self::protected_paths`]:
    /// true but for a folder marked by [`Self::as_job_folder`]. Output: the answer.
    pub fn holds_project_guards(&self) -> bool {
        !self.job_folder
    }

    /// The project folder this sandbox confines writes to.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// The agent's managed home directory.
    pub fn agent_home(&self) -> &Path {
        &self.agent_home
    }

    /// The agent's managed temp directory.
    pub fn agent_tmp(&self) -> &Path {
        &self.agent_tmp
    }

    /// The extra read-only roots, such as harness install directories.
    pub fn read_roots(&self) -> &[PathBuf] {
        &self.read_roots
    }

    /// The network reach this sandbox grants the agent.
    pub fn network(&self) -> &SandboxNetwork {
        &self.network
    }

    /// Sets how this agent's sockets are held on Linux when its network is limited. Input:
    /// the owner's setting. Output: the sandbox.
    pub fn limit_sockets(mut self, sockets: LinuxSockets) -> Self {
        self.linux_sockets = sockets;
        self
    }

    /// How this agent's sockets are held on Linux when its network is limited.
    pub fn linux_sockets(&self) -> LinuxSockets {
        self.linux_sockets
    }

    /// The loopback ports a Linux that nests agents carries for this one over a bridge, the
    /// agent then having a network namespace of its own: the sockets setting is `TcpOnly`
    /// and the limited network names loopback ports only, each [`BRIDGE_PORT_MIN`] or
    /// higher. Output: the ports in rising order, or none when the agent keeps the shared
    /// network.
    pub fn bridged_ports(&self) -> Option<Vec<u16>> {
        let SandboxNetwork::Only(remotes) = &self.network else {
            return None;
        };
        if self.linux_sockets != LinuxSockets::TcpOnly || remotes.is_empty() {
            return None;
        }
        let mut ports = Vec::with_capacity(remotes.len());
        for remote in remotes {
            match remote {
                SandboxRemote::Loopback(port) if *port >= BRIDGE_PORT_MIN => ports.push(*port),
                _ => return None,
            }
        }
        ports.sort_unstable();
        ports.dedup();
        assert!(!ports.is_empty() && ports.len() <= REMOTES_MAX);
        assert!(!self.resolves_over_tcp());
        Some(ports)
    }

    /// The network a Linux that nests agents gives this one of its own, leaving only
    /// through its ports on any host: the sockets setting is `TcpOnly`, the limited network
    /// names at least one port on any host and every other remote is a loopback port
    /// [`BRIDGE_PORT_MIN`] or higher, bridged as [`ProjectSandbox::bridged_ports`] does.
    /// Output: the ports, or none when the agent keeps the shared network.
    pub fn routed(&self) -> Option<Routed> {
        let SandboxNetwork::Only(remotes) = &self.network else {
            return None;
        };
        if self.linux_sockets != LinuxSockets::TcpOnly || remotes.len() > REMOTES_MAX {
            return None;
        }
        let mut any = Vec::with_capacity(remotes.len());
        let mut bridged = Vec::with_capacity(remotes.len());
        for remote in remotes {
            match remote {
                SandboxRemote::AnyHost(port) => any.push(*port),
                SandboxRemote::Loopback(port) if *port >= BRIDGE_PORT_MIN => bridged.push(*port),
                SandboxRemote::Loopback(_) => return None,
            }
        }
        if any.is_empty() {
            return None;
        }
        any.sort_unstable();
        any.dedup();
        bridged.sort_unstable();
        bridged.dedup();
        assert!(!any.contains(&0));
        assert!(any.len() + bridged.len() <= REMOTES_MAX);
        assert!(self.bridged_ports().is_none());
        assert!(self.resolves_over_tcp());
        Some(Routed { any, bridged })
    }

    /// The port that alone keeps this agent on this computer's network on a Linux that
    /// nests agents: the sockets setting is `TcpOnly` and the limited network names
    /// loopback ports only, but one lies below [`BRIDGE_PORT_MIN`]. Output: the lowest such
    /// port, or none when the agent is bridged or shares the network for another reason.
    pub fn unbridged_port(&self) -> Option<u16> {
        let SandboxNetwork::Only(remotes) = &self.network else {
            return None;
        };
        if self.linux_sockets != LinuxSockets::TcpOnly {
            return None;
        }
        let mut lowest: Option<u16> = None;
        for remote in remotes {
            let SandboxRemote::Loopback(port) = remote else {
                return None;
            };
            if *port < BRIDGE_PORT_MIN && lowest.is_none_or(|low| *port < low) {
                lowest = Some(*port);
            }
        }
        assert!(lowest.is_none() || self.bridged_ports().is_none());
        lowest
    }

    /// True when, on Linux, this agent looks host names up over TCP: the setting is
    /// `TcpOnly` and its limited network names a remote on any host. It then may reach TCP
    /// port 53 and gets `RES_OPTIONS=use-vc`; loopback-only and empty networks get neither.
    pub fn resolves_over_tcp(&self) -> bool {
        self.linux_sockets == LinuxSockets::TcpOnly
            && matches!(&self.network, SandboxNetwork::Only(remotes)
                if remotes.iter().any(|remote| matches!(remote, SandboxRemote::AnyHost(_))))
    }

    /// Why the Linux sockets setting refuses to start `program` in this sandbox:
    /// [`LINUX_LIMITED_NETWORK_OFF`] for any limited network under `Refuse`, and
    /// [`LINUX_RESOLVER_NEEDS_UDP`] under `TcpOnly` when the agent resolves names and the
    /// program, or the interpreter a script names, is not a glibc one, and
    /// [`LINUX_HARNESS_UNCHECKED`] when that interpreter cannot be checked ([`elf::lookup`]).
    /// The setting never changes silently. Input: the harness program. Output: the refusal
    /// code, or None.
    pub fn linux_refusal(&self, program: &Path) -> Option<&'static str> {
        match (&self.network, self.linux_sockets) {
            (SandboxNetwork::Open, _) => None,
            (SandboxNetwork::Only(_), LinuxSockets::Refuse) => Some(LINUX_LIMITED_NETWORK_OFF),
            _ if self.resolves_over_tcp() => match elf::lookup(program) {
                elf::Lookup::OverTcp => None,
                elf::Lookup::NotOverTcp => Some(LINUX_RESOLVER_NEEDS_UDP),
                elf::Lookup::Unjudged => Some(LINUX_HARNESS_UNCHECKED),
            },
            _ => None,
        }
    }

    /// Paths no agent may write although they sit in a writable root: both `context/`
    /// folders, the project instructions file every later agent and the master get as the
    /// owner's text (SA vid 11), the git hooks, config and its lock, `.git/modules` and, for
    /// an agent confined to a worktree, its admin folder's `modules`, where git keeps
    /// submodule repositories.
    /// Every platform boundary keeps these read-only, except that Windows guards the config
    /// but not its lock (`windows::GIT_CONFIG_LOCK`).
    pub fn protected_paths(&self) -> Vec<PathBuf> {
        let git = self.project_root.join(".git");
        let mut paths = vec![
            self.project_root.join("context"),
            self.project_root.join(".crazyagent").join("context"),
            self.project_root
                .join(".crazyagent")
                .join(self.hooks.project_file),
            git.join("hooks"),
            git.join("config"),
            git.join("config.lock"),
            git.join("modules"),
        ];
        if let Some(checkout) = &self.checkout {
            paths.push(checkout.admin.join("modules"));
        }
        paths
    }

    /// How many confinements the sandbox holds: its own worktree, its own clone or the
    /// teammates' setup, at most one. Output: the count.
    fn confinements(&self) -> usize {
        usize::from(self.checkout.is_some())
            + usize::from(self.clone.is_some())
            + usize::from(self.setup.is_some())
    }

    /// Every folder this sandbox lets the agent write: the project folder, or for a
    /// confined agent its worktree, that worktree's git admin folder and the git object
    /// store, or for a clone-confined agent its clone alone, then its own home and temp
    /// dirs, its short temp folder, then its extra worktrees, then for a native lead's
    /// teammates the team folders they share in the lead's home, then its tool cache, the
    /// owner's temp folder when granted and each shared writable folder.
    pub fn writable_roots(&self) -> impl Iterator<Item = &PathBuf> {
        assert!(self.confinements() <= 1);
        let project: Vec<&PathBuf> = match (&self.checkout, &self.clone, &self.setup) {
            (Some(checkout), _, _) => vec![&checkout.worktree, &checkout.admin, &checkout.objects],
            (None, Some(clone), _) => vec![clone],
            (None, None, Some(setup)) => vec![&setup.folder, &setup.records],
            (None, None, None) => vec![&self.project_root],
        };
        project
            .into_iter()
            .chain([&self.agent_home, &self.agent_tmp])
            .chain(self.scratch.iter())
            .chain(self.worktrees.iter())
            .chain(self.shared.iter())
            .chain(self.tool_cache.iter().map(|(root, _)| root))
            .chain(self.owner_tmp.iter())
            .chain(self.shared_writes.iter())
    }

    /// The read-only toolchain folders, Seatbelt only, with credential names carved out.
    pub fn tool_roots(&self) -> &[PathBuf] {
        &self.tool_roots
    }

    /// Whether the agent could write `path` or move it away: true when the path is one of
    /// [`Self::writable_roots`] or inside one. Input: an absolute canonical path. Output:
    /// the answer.
    pub fn writes(&self, path: &Path) -> bool {
        self.writable_roots().any(|root| path.starts_with(root))
    }

    /// The Seatbelt profile text for this agent. Reading another process's arguments through
    /// sysctl is refused only with both the named sysctl deny and the explicit `process-info*`
    /// deny. What stays open: every read of a process of the same sandbox, the list of process
    /// ids, which carries only ids, and for `nohup`, by its own path, the one read of a process
    /// outside that it needs to detach from the console. Output: the text.
    pub fn profile(&self) -> String {
        let mut text = String::from("(version 1)\n(deny default)\n");
        text.push_str("(allow process-exec)\n(allow process-fork)\n");
        text.push_str(&app_exec_denies(
            self.writable_roots(),
            self.execs.iter().filter(|program| !self.writes(program)),
        ));
        text.push_str("(allow signal (target same-sandbox))\n");
        text.push_str("(allow sysctl-read)\n");
        text.push_str("(deny sysctl-read (sysctl-name-prefix \"kern.procargs\"))\n");
        text.push_str("(deny process-info*)\n");
        text.push_str("(allow process-info* (target same-sandbox))\n");
        text.push_str("(allow process-info-listpids)\n");
        text.push_str("(allow process-info-pidinfo (process-path \"/usr/bin/nohup\"))\n");
        text.push_str("(allow iokit-open (iokit-registry-entry-class \"RootDomainUserClient\"))\n");
        text.push_str("(allow ipc-posix-sem)\n");
        text.push_str(
            "(allow ipc-posix-shm-read* (ipc-posix-name \"apple.shm.notification_center\"))\n",
        );
        text.push_str("(allow pseudo-tty)\n");
        text.push_str("(allow mach-lookup");
        for service in MACH_SERVICES {
            text.push_str(&format!("\n  (global-name \"{service}\")"));
        }
        text.push_str(")\n");
        text.push_str("(allow file-read* file-map-executable");
        for root in SYSTEM_READ_ROOTS {
            text.push_str(&format!("\n  (subpath \"{root}\")"));
        }
        text.push_str(")\n(allow file-read*");
        for literal in METADATA_LITERALS {
            text.push_str(&format!("\n  (literal \"{literal}\")"));
        }
        text.push_str(")\n");
        text.push_str("(allow file-read-metadata");
        for root in self
            .writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
            .chain(self.tool_files.iter())
        {
            text.push_str(&format!("\n  (path-ancestors \"{}\")", root.display()));
        }
        text.push_str(")\n(allow file-read* file-map-executable");
        for root in self
            .writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
        {
            text.push_str(&format!("\n  (subpath \"{}\")", root.display()));
        }
        for file in &self.tool_files {
            text.push_str(&format!("\n  (literal \"{}\")", file.display()));
        }
        text.push_str(")\n(allow file-write*");
        for root in self.writable_roots() {
            text.push_str(&format!("\n  (subpath \"{}\")", root.display()));
        }
        text.push_str(")\n");
        assert!(self.setup.is_none() || self.lead_branch.is_some());
        if let (Some(_), Some(branch)) = (&self.setup, &self.lead_branch) {
            text.push_str(&format!(
                "(allow file-write*\n  (regex #\"^{}/\\.git/(logs/)?refs/heads/({}|{}--[a-z0-9-]+(\\.lock)?)$\"))\n",
                seatbelt_pattern(&self.project_root),
                regex_escape(self.hooks.linked_prefix.trim_end_matches('/')),
                regex_escape(branch)
            ));
        }
        assert!(self.links.len() <= LINKS_MAX);
        for link in self.links.iter().filter(|link| clean_absolute(link)) {
            text.push_str(&format!(
                "(allow file-read* (literal \"{}\"))\n",
                link.display()
            ));
        }
        text.push_str(&self.project_secret_rules());
        text.push_str("(deny file-read* file-write*");
        for secret in self.secret_paths() {
            if clean_absolute(&secret) {
                text.push_str(&format!("\n  (subpath \"{}\")", secret.display()));
            }
        }
        let credentials = CREDENTIAL_NAMES
            .iter()
            .map(|name| regex_escape(name))
            .collect::<Vec<_>>()
            .join("|");
        for root in &self.tool_roots {
            text.push_str(&format!(
                "\n  {}",
                at_or_below(&format!("{}/(.*/)?({credentials})", seatbelt_pattern(root)))
            ));
        }
        text.push_str(")\n(deny mach-lookup");
        for service in KEYCHAIN_SERVICES {
            text.push_str(&format!("\n  (global-name \"{service}\")"));
        }
        text.push_str(")\n");
        if let Some((home, folder)) = self
            .carved_home
            .as_ref()
            .filter(|(home, folder)| clean_absolute(home) && home.starts_with(folder))
        {
            assert!(self.engine_secrets.iter().any(|secret| secret == folder));
            text.push_str(&format!(
                "(allow file-read* file-write*\n  (subpath \"{}\"))\n(allow file-read-metadata",
                home.display()
            ));
            for above in home
                .ancestors()
                .skip(1)
                .take_while(|above| above.starts_with(folder))
            {
                text.push_str(&format!("\n  (literal \"{}\")", above.display()));
            }
            text.push_str(")\n");
        }
        if !self.hidden.is_empty() {
            text.push_str("(deny file-read* file-write* process-exec file-link file-clone");
            for folder in self.hidden.iter().filter(|folder| clean_absolute(folder)) {
                text.push_str(&format!("\n  (subpath \"{}\")", folder.display()));
            }
            text.push_str(")\n");
        }
        if !self.kept_out.is_empty() {
            text.push_str(KEPT_OUT_DENY);
            for path in self.kept_out.iter().filter(|path| clean_absolute(path)) {
                text.push_str(&format!("\n  (subpath \"{}\")", path.display()));
            }
            text.push_str(")\n");
        }
        text.push_str(&self.closed_tops_rule());
        for file in self.revealed.iter().filter(|file| clean_absolute(file)) {
            text.push_str(&format!(
                "(allow file-read* (literal \"{}\"))\n",
                file.display()
            ));
        }
        text.push_str(&self.project_denies());
        if let Some(scratch) = &self.scratch {
            text.push_str(&format!(
                "(deny {FIXED_WRITES}\n  (literal \"{}\"))\n",
                scratch.display()
            ));
        }
        if let Some(tmp) = &self.owner_tmp {
            let tmp = tmp.display();
            text.push_str(&format!(
                "(deny {FIXED_WRITES}\n  (literal \"{tmp}\"))\n(deny file-write*\n  (require-all\n    (subpath \"{tmp}\")\n    (vnode-type SOCKET)))\n"
            ));
        }
        text.push_str(&self.git_link_denies());
        text.push_str(&self.submodule_denies());
        text.push_str(&self.nested_git_denies());
        text.push_str(&self.ancestry_denies());
        if let Some(checkout) = &self.checkout {
            text.push_str(&checkout.object_denies());
            let [head, _] = ADMIN_WRITES.map(any_case);
            text.push_str(&format!(
                "(deny file-write*\n  (regex #\"^{}/{head}$\"))\n",
                seatbelt_pattern(&checkout.admin)
            ));
        }
        if let Some(clone) = &self.clone {
            text.push_str(&clone_store_denies(clone));
        }
        text.push_str(
            "(allow file-read*\n  (literal \"/dev\")\n  (literal \"/dev/random\")\n  (literal \"/dev/urandom\")\n  (literal \"/dev/stdin\")\n  (literal \"/dev/stdout\")\n  (literal \"/dev/stderr\"))\n(allow file-read* file-write-data file-ioctl\n  (literal \"/dev/null\")\n  (literal \"/dev/zero\")\n  (literal \"/dev/tty\")\n  (literal \"/dev/ptmx\")\n  (literal \"/dev/dtracehelper\")\n  (subpath \"/dev/fd\"))\n(allow file-read-metadata file-ioctl\n  (regex #\"^/dev/ttys[0-9]+$\"))\n(allow file-read* file-write-data\n  (require-all\n    (regex #\"^/dev/ttys[0-9]+$\")\n    (extension \"com.apple.sandbox.pty\")))\n",
        );
        for socket in &self.sockets {
            let path = socket.display();
            let folder = socket.parent().unwrap_or(socket).display();
            text.push_str(&format!(
                "(allow file-read* file-write*\n  (literal \"{path}\")\n  (literal \"{path}.lock\"))\n(allow file-read-metadata (literal \"{folder}\"))\n(allow network-bind (local unix-socket (path-literal \"{path}\")))\n(allow network-outbound (remote unix-socket (path-literal \"{path}\")))\n"
            ));
        }
        for socket in &self.served {
            let path = socket.display();
            let folder = socket.parent().unwrap_or(socket).display();
            text.push_str(&format!(
                "(allow file-read* file-write*\n  (literal \"{path}\")\n  (literal \"{path}.lock\"))\n(allow file-read-metadata (literal \"{folder}\"))\n(allow network-bind (local unix-socket (path-literal \"{path}\")))\n(deny file-write-unlink (literal \"{path}\"))\n"
            ));
        }
        for socket in self.reached.iter().chain(&self.granted) {
            let path = socket.display();
            let folder = socket.parent().unwrap_or(socket).display();
            text.push_str(&format!(
                "(allow file-read* (literal \"{path}\"))\n(allow file-read-metadata (literal \"{folder}\"))\n(allow network-outbound (remote unix-socket (path-literal \"{path}\")))\n(deny file-write* (literal \"{path}\"))\n"
            ));
        }
        for socket in &self.shut {
            let path = socket.display();
            text.push_str(&format!(
                "(deny file-write*\n  (literal \"{path}\")\n  (literal \"{path}.lock\"))\n"
            ));
        }
        for folder in &self.read_only {
            text.push_str(&format!(
                "(deny file-write* (subpath \"{}\"))\n",
                folder.display()
            ));
        }
        let launch = launch_denies(self.hooks, &self.agent_home);
        text.push_str(&launch);
        let config = if self.owner_config {
            String::new()
        } else {
            agent_config_denies(self.hooks, &self.agent_home)
        };
        text.push_str(&config);
        for root in self.writable_roots() {
            text.push_str(&root_pin(root));
        }
        for folder in &self.fixed {
            text.push_str(&format!(
                "(deny {FIXED_WRITES} (literal \"{}\"))\n",
                folder.display()
            ));
        }
        for folder in &self.shared {
            text.push_str(&shared_denies(folder));
        }
        let teammates = self.teammates_denies();
        text.push_str(&teammates);
        let answers = self.answers_denies();
        text.push_str(&answers);
        let agent_files = self
            .agent_files
            .map(|rule| rule.seatbelt(&self.project_root))
            .unwrap_or_default();
        text.push_str(&agent_files);
        let ignored = hfs_ignored_deny();
        text.push_str(&ignored);
        assert_eq!(
            text.matches("unix-socket").count(),
            2 * self.sockets.len() + self.served.len() + self.reached.len() + self.granted.len()
        );
        let mut last: Vec<String> = self
            .read_only
            .iter()
            .map(|folder| format!("(deny file-write* (subpath \"{}\"))", folder.display()))
            .collect();
        last.push(launch);
        if !config.is_empty() {
            last.push(config);
        }
        last.extend(self.writable_roots().map(|root| root_pin(root)));
        last.extend(
            self.fixed
                .iter()
                .map(|folder| format!("(deny {FIXED_WRITES} (literal \"{}\"))", folder.display())),
        );
        last.extend(self.shared.iter().map(|folder| shared_denies(folder)));
        if !teammates.is_empty() {
            last.push(teammates);
        }
        if !answers.is_empty() {
            last.push(answers);
        }
        if !agent_files.is_empty() {
            last.push(agent_files);
        }
        last.push(ignored);
        assert!(
            denies_follow_write_allows(&text, &last),
            "a read-only, root, fixed, shared or teammates' write deny must follow every write allow, since the last match wins"
        );
        let denied_last = |operations: &str, socket: &PathBuf| {
            text.rfind(&format!(
                "(deny {operations} (literal \"{}\"))",
                socket.display()
            )) > text.rfind("(allow file-write*")
        };
        assert!(
            self.served
                .iter()
                .all(|socket| denied_last("file-write-unlink", socket))
                && self
                    .reached
                    .iter()
                    .chain(&self.granted)
                    .all(|socket| denied_last("file-write*", socket)),
            "a served socket's unlink deny and a reached socket's write deny must follow every write allow, since the last match wins"
        );
        let network = text.len();
        match &self.network {
            SandboxNetwork::Open => {
                text.push_str(
                    "(allow network-outbound (remote ip \"*:*\"))\n(allow network-inbound (local ip \"*:*\"))\n(allow network-bind (local ip \"*:*\"))\n(allow network-outbound (literal \"/private/var/run/mDNSResponder\"))\n",
                );
                let owner = OWNER_PORTS
                    .iter()
                    .chain(&self.kept_ports)
                    .copied()
                    .chain(PROXY_PORTS)
                    .chain(RELAY_PORTS)
                    .map(|port| format!(" (local ip \"*:{port}\")"))
                    .collect::<String>();
                text.push_str(&format!(
                    "(deny network-bind{owner})\n(deny network-inbound{owner})\n"
                ));
                text.push_str(
                    "(allow system-socket (require-all (socket-domain AF_SYSTEM) (socket-protocol 2)))\n",
                );
            }
            SandboxNetwork::Only(remotes) => {
                for remote in remotes {
                    let target = match remote {
                        SandboxRemote::Loopback(port) => format!("localhost:{port}"),
                        SandboxRemote::AnyHost(port) => format!("*:{port}"),
                    };
                    text.push_str(&format!(
                        "(allow network-outbound (remote ip \"{target}\"))\n"
                    ));
                }
                if remotes
                    .iter()
                    .any(|remote| matches!(remote, SandboxRemote::AnyHost(_)))
                {
                    text.push_str(
                        "(allow network-outbound (literal \"/private/var/run/mDNSResponder\"))\n",
                    );
                }
            }
        }
        assert!(text.starts_with("(version 1)\n(deny default)\n"));
        assert!(
            text.find("(deny process-exec") > text.find("(allow process-exec)"),
            "the app bundle exec deny must follow the exec allow, since the last match wins"
        );
        assert!(!text.contains("(subpath \"/dev\")"));
        assert!(!text[network..].contains("network*") && !text[network..].contains("unix-socket"));
        assert!(
            MACH_SERVICES
                .iter()
                .all(|service| !KEYCHAIN_SERVICES.contains(service))
        );
        text
    }

    /// The Seatbelt rule that keeps the files the engine and the owner's git read in the
    /// project folder out of agent hands: both `context/` folders, the git hooks and the
    /// rebase, sequencer, remotes and branches state, each with everything below it, and the
    /// entries `.git`, `.crazyagent`, the project instructions file `.crazyagent/instructions.md`
    /// (SA vid 11), the git config and its lock, the index and its lock, `commondir`,
    /// `config.worktree`, `objects` and `objects/info`, none of which an agent may write or
    /// hard link under another name it could write through (SA vid 11 r2). Each name below the
    /// project folder matches in any letter case, so the rule does not depend on how Seatbelt
    /// folds case on the case-insensitive volume, where git finds a name however it is typed
    /// (RV-DS241-15). `.git/info`, `.git/shallow`, the alternates files and `.git/modules`
    /// are kept the same way by [`Self::ancestry_denies`] and [`Self::submodule_denies`].
    /// Input: the policy. Output: the rule text.
    fn project_denies(&self) -> String {
        let project = seatbelt_pattern(&self.project_root);
        let git = any_case(".git");
        let state = any_case(self.hooks.crazyagent_directory);
        let context = any_case("context");
        let objects = format!("{git}/{}", any_case("objects"));
        let mut below = vec![context.clone(), format!("{state}/{context}")];
        below.extend(GIT_STATE_TREES.map(|name| format!("{git}/{}", any_case(name))));
        let mut entries = vec![
            git.clone(),
            format!("{state}/{}", any_case(self.hooks.project_file)),
            state,
            format!("{objects}/{}", any_case("info")),
            objects,
        ];
        entries.extend(GIT_STATE_FILES.map(|name| format!("{git}/{}", any_case(name))));
        let mut text = String::from("(deny file-write* file-link");
        for path in &below {
            text.push_str(&format!(
                "\n  {}",
                at_or_below(&format!("{project}/{path}"))
            ));
        }
        for path in &entries {
            text.push_str(&format!("\n  (regex #\"^{project}/{path}$\")"));
        }
        text.push_str(")\n");
        assert_eq!(below.len() + entries.len(), 19);
        assert_eq!(text.matches("(regex #\"^/").count(), 19);
        text
    }

    /// The Seatbelt rules that keep the owner's git reading the metadata the engine made.
    /// No agent may move the project folder, the git worktrees folders or a worktree, which
    /// would carry protected files out of their denied paths and back. A worktree's `.git`
    /// file and its `commondir` and `gitdir` files may be created once, as `git worktree add`
    /// does, only by a profile that adds worktrees ([`Self::adding_worktrees`] or a lead's
    /// teammates' setup), since the owner's `git worktree repair` would point a real
    /// checkout at an admin folder planted with them; they are never rewritten, removed,
    /// re-permissioned, flagged, given an ACL or a time, or hard linked under a name an agent
    /// could change them through, so the owner's git keeps reading them (RV-DS241-185, rv-sa241
    /// NV-1, LINUX-TEAM-TIME-CALLS: a profile adding none denies every write, since Apple's
    /// `utimensat` sets times past `file-write-times`), and no `config.worktree` may be
    /// written. No link may be made anywhere in the git folder, in any letter case: git's
    /// `worktree prune`, which the owner's next commit runs as maintenance, empties whatever
    /// folder a link at `.git/worktrees` or `.git/worktrees/<name>` names
    /// (SECAUDIT-WORKTREE-ADMIN-PLANT). Each
    /// name below the project root, and each extra worktree's `.git` link, matches in any
    /// letter case, as in `clone_store_denies`, since the case-insensitive disk finds it
    /// however it is typed (DEC-123 rv-secreview L2, RV-DS241-15).
    /// Input: the policy. Output: the rule text.
    fn git_link_denies(&self) -> String {
        let project = self.project_root.display();
        let pattern = seatbelt_pattern(&self.project_root);
        let git = any_case(".git");
        let worktrees = any_case("worktrees");
        let state = any_case(self.hooks.crazyagent_directory);
        let admin = format!("{pattern}/{git}/{worktrees}/[^/]+");
        let checkouts = format!("{pattern}(/{state})?/{worktrees}/[^/]+");
        let mut moves = format!(
            "(deny file-write-unlink\n  (literal \"{project}\")\n  (regex #\"^{pattern}/{git}/{worktrees}$\")\n  (regex #\"^{pattern}(/{state})?/{worktrees}$\")\n  (regex #\"^{admin}$\")\n  (regex #\"^{checkouts}$\")"
        );
        let denied = if self.adds_worktrees || self.setup.is_some() {
            "file-write-unlink file-write-data file-write-mode file-write-flags file-write-acl file-write-times file-link"
        } else {
            "file-write* file-link"
        };
        let [commondir, gitdir] = ADMIN_LINKS.map(any_case);
        let mut links = format!(
            "(deny {denied}\n  (regex #\"^{admin}/({commondir}|{gitdir})$\")\n  (regex #\"^{checkouts}/{git}$\")"
        );
        for worktree in &self.worktrees {
            moves.push_str(&format!("\n  (literal \"{}\")", worktree.display()));
            links.push_str(&format!(
                "\n  (regex #\"^{}/{git}$\")",
                seatbelt_pattern(worktree)
            ));
        }
        let [_, worktree_config] = ADMIN_WRITES.map(any_case);
        let text = format!(
            "{moves})\n{links})\n(deny file-write*\n  (regex #\"^{admin}/{worktree_config}$\"))\n(deny file-write-create\n  (require-all (regex #\"^{pattern}/{git}/\") (vnode-type SYMLINK)))\n"
        );
        assert_eq!(
            text.matches("(regex #\"^/").count(),
            8 + self.worktrees.len()
        );
        assert!(text.contains("(vnode-type SYMLINK)"));
        assert!(!text.contains("/.crazyagent/worktrees\""));
        text
    }

    /// The Seatbelt rule that keeps the submodule store out of agent hands. Git keeps a
    /// submodule's own repository, with the config and hooks a later git would run, under
    /// `.git/modules`; no agent may write there, in the common git folder or a worktree
    /// admin folder, so none can plant a submodule whose metadata the engine or the owner
    /// then enters. The `.gitmodules` file and gitlinks stay writable tracked content: the
    /// engine's own git ignores them with `--ignore-submodules` and never recurses. The
    /// name matches in any letter case ([`any_case`], the long s included), since the
    /// project disk may be case insensitive. Input: the policy. Output: the rule text.
    fn submodule_denies(&self) -> String {
        let pattern = seatbelt_pattern(&self.project_root);
        let dot_git = any_case(".git");
        let worktrees = any_case("worktrees");
        let modules = any_case("modules");
        let text = format!(
            "(deny file-write*\n  {})\n",
            at_or_below(&format!(
                "{pattern}/{dot_git}(/{worktrees}/[^/]+)?/{modules}"
            ))
        );
        assert_eq!(text.matches("(regex #\"^/").count(), 1);
        text
    }

    /// The Seatbelt rule that keeps an agent from planting a nested git repository inside a
    /// writable working tree, which the owner's own unsandboxed git (its terminal or the
    /// V2 IDE) would later enter as a submodule and run under the agent's own config (SM1).
    /// No entry named `.git`, in every spelling a case-insensitive volume finds ([`any_case`]),
    /// as a folder, file or link, may be created below a writable working tree, except that
    /// tree's own top-level `.git` and, only in a profile built by [`Self::adding_worktrees`],
    /// the engine's worktree links, given back by their exact name in either case, which any
    /// other process could point at git data of its own (SECAUDIT-WORKTREE-ADMIN-PLANT).
    /// The project's own `.git` and each worktree's own `.git` sit
    /// at a tree's root, not below it, so the `.+/` guard never matches them. A whole folder
    /// already holding a `.git` could once be renamed in as a path-blind move Seatbelt
    /// cannot inspect; that source is removed by also denying `.git` at any depth, top level
    /// included, in every writable root that is not a working tree ([`Self::git_deny_roots`],
    /// the owner's temp folder and shared writable folders included), so no agent can create
    /// a repository anywhere to rename from (SM1-R, RV-DS241-16). The only `.git` entries in agent roots are engine-made links to exactly
    /// `/dev/null`: the uv markers in a tool cache's buckets (T8 C1), which an agent cannot
    /// make, remove or point elsewhere, and which, moved with their bucket into a tree, git
    /// never enters or stages. Input: the policy. Output: the rule text.
    fn nested_git_denies(&self) -> String {
        let dot_git = any_case(".git");
        let own_git = "\\.[gG][iI][tT]";
        let (working, other) = self.git_deny_roots();
        let mut nested = String::new();
        for root in &working {
            nested.push_str(&format!(
                "\n      {}",
                at_or_below(&format!("{}/.+/{dot_git}", seatbelt_pattern(root)))
            ));
        }
        for root in &other {
            nested.push_str(&format!("\n      {}", any_depth_git(root)));
        }
        let pattern = seatbelt_pattern(&self.project_root);
        let checkouts = format!("{pattern}(/\\.crazyagent)?/worktrees/[^/]+");
        let mut exempt = String::new();
        if self.adds_worktrees {
            exempt.push_str(&format!(
                "\n    (require-not (regex #\"^{checkouts}/{own_git}$\"))"
            ));
        }
        if let Some(clone) = &self.clone {
            exempt.push_str(&format!(
                "\n    (require-not (regex #\"^{}/{}/[^/]+/{own_git}$\"))",
                seatbelt_pattern(clone),
                regex_escape(self.hooks.teammates_folder)
            ));
        }
        if let Some(setup) = &self.setup {
            exempt.push_str(&format!(
                "\n    (require-not (regex #\"^{}/[^/]+/{own_git}$\"))",
                seatbelt_pattern(&setup.folder)
            ));
        }
        for worktree in &self.worktrees {
            exempt.push_str(&format!(
                "\n    (require-not (literal \"{}\"))",
                worktree.join(".git").display()
            ));
        }
        let text =
            format!("(deny file-write*\n  (require-all\n    (require-any{nested}){exempt}))\n");
        assert!(
            text.matches("(regex #\"^/").count()
                == working.len()
                    + other.len()
                    + usize::from(self.adds_worktrees)
                    + usize::from(self.clone.is_some())
                    + usize::from(self.setup.is_some())
        );
        assert!(text.ends_with(")\n"));
        text
    }

    /// The Seatbelt rule that keeps an agent from writing what git reads ancestry or objects
    /// from beside the commits, in every letter case the volume folds together (`any_case`,
    /// the Kelvin sign and long s included): a commit-graph file, single or split, and an
    /// `alternates` or `http-alternates` file in the project's object store or the one its
    /// checkout shares (land-19 r1 M1, r2 L1), and the project's `.git/info` (which holds
    /// `grafts`) and `.git/shallow` (land-19 r2 M1). A forged graph or graft could claim an
    /// agent commit is on the landing tip. This is the only rule on those names, and it holds
    /// a case variant made where the name does not exist yet, as after a clone or
    /// `init --template=`, which git then reads through the case-insensitive volume. Engine
    /// git reads no graph or graft; this also keeps the owner's own git from being handed one.
    /// Input: the policy. Output: the rule text.
    fn ancestry_denies(&self) -> String {
        let git = format!(
            "{}/{}",
            seatbelt_pattern(&self.project_root),
            any_case(".git")
        );
        let mut stores = vec![format!("{git}/{}", any_case("objects"))];
        if let Some(checkout) = &self.checkout {
            stores.push(seatbelt_pattern(&checkout.objects));
        }
        let info = any_case("info");
        let mut text = format!(
            "(deny file-write*\n  (regex #\"^{git}/({info}|{})$\")\n  (regex #\"^{git}/{info}/\")",
            any_case("shallow")
        );
        let [alternates, http_alternates] = STORE_ALTERNATES.map(any_case);
        let graph = format!("{}{}?", any_case("commit-graph"), any_case("s"));
        for store in &stores {
            text.push_str(&format!(
                "\n  (regex #\"^{store}/{info}/({graph}|{alternates}|{http_alternates})$\")\n  (regex #\"^{store}/{info}/{graph}/\")"
            ));
        }
        text.push_str(")\n");
        assert_eq!(text.matches("(regex #\"^/").count(), 2 * stores.len() + 2);
        text
    }

    /// Splits the agent's writable roots into the working-tree roots, where the root's own
    /// top-level `.git` is a legitimate repository link the engine adds, and every other
    /// writable root, where no `.git` may ever appear. The working trees are the project
    /// folder, worktree, clone or setup folder the agent works in and its extra worktrees;
    /// every other root comes from [`Self::writable_roots`] itself, so a root granted there
    /// is under the rule at once: an admin folder, object store or setup records, home, temp
    /// or scratch, a team folder a native lead shares with its teammates, the tool cache (T8,
    /// whose only `.git` entries are the engine's `/dev/null` links), the owner's temp folder
    /// and every shared writable folder (RV-DS241-16). A `.git` anywhere below a working
    /// tree, and a `.git` at any depth (top level included) in every other root, is denied so
    /// no agent can create a repository the owner's git enters there or rename one whole
    /// into a worktree (OP-9 r2, SM1-R). Output: the working roots and the other roots.
    fn git_deny_roots(&self) -> (Vec<&Path>, Vec<&Path>) {
        assert!(self.confinements() <= 1);
        let mut working: Vec<&Path> = vec![match (&self.checkout, &self.clone, &self.setup) {
            (Some(checkout), _, _) => checkout.worktree.as_path(),
            (None, Some(clone), _) => clone.as_path(),
            (None, None, Some(setup)) => setup.folder.as_path(),
            (None, None, None) => self.project_root.as_path(),
        }];
        working.extend(self.worktrees.iter().map(PathBuf::as_path));
        let other: Vec<&Path> = self
            .writable_roots()
            .map(PathBuf::as_path)
            .filter(|root| !working.contains(root))
            .collect();
        assert!(
            working
                .iter()
                .all(|root| self.writable_roots().any(|writable| writable == root))
        );
        assert!(other.len() >= 2);
        (working, other)
    }

    /// Writes the profile with mode 0600 at `path`, replacing any earlier one
    /// atomically. The path must lie outside every writable root, so no agent
    /// can rewrite the profile its next launch uses. A profile holding a string Seatbelt
    /// cannot read, or longer than [`PROFILE_BYTES_MAX`], is refused as `Invalid`, never written.
    /// An agent home first gets the folders its profile keeps it from making
    /// ([`make_config_parents`]), unless it lies outside the engine's data root
    /// ([`Self::anchor_home`]).
    #[cfg(unix)]
    pub fn install(&self, path: &Path) -> Result<(), SandboxError> {
        let parent = path
            .parent()
            .filter(|parent| clean_absolute(parent) && parent.is_dir())
            .ok_or_else(|| invalid("profile directory must exist"))?;
        let parent = parent.canonicalize()?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| name.ends_with(".sb") && name.len() <= 128)
            .ok_or_else(|| invalid("profile name must end in .sb"))?;
        let target = parent.join(name);
        if self.writable_roots().any(|root| target.starts_with(root)) {
            return Err(invalid(
                "profile must live outside the sandbox's writable roots",
            ));
        }
        if !self.owner_config && self.engine_home != Some(false) {
            make_config_parents(self.hooks, &self.agent_home, self.engine_home == Some(true))?;
        }
        let profile = self.profile();
        if !sbpl_overlong(&profile).is_empty() {
            return Err(invalid(
                "a path or name in the sandbox profile is longer than Seatbelt reads",
            ));
        }
        if profile.len() > PROFILE_BYTES_MAX {
            return Err(invalid("the sandbox profile is larger than its 64 KiB cap"));
        }
        let staging = parent.join(format!(".{name}.{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&staging)?;
        let written = file
            .write_all(profile.as_bytes())
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&staging, &target));
        if let Err(error) = written {
            let _ = fs::remove_file(&staging);
            return Err(error.into());
        }
        Ok(())
    }

    /// Refuses to write a Seatbelt profile, which Windows cannot load; Windows agents start
    /// through [`InstalledSandbox`] and the AppContainer boundary. Takes the path, unused.
    /// Returns `Unavailable` with [`kernel_sandbox::windows::SEATBELT_MISSING`].
    #[cfg(target_os = "windows")]
    pub fn install(&self, _path: &Path) -> Result<(), SandboxError> {
        Err(SandboxError::Unavailable(
            kernel_sandbox::windows::SEATBELT_MISSING.into(),
        ))
    }

    /// A command that runs `program` under the installed profile with a cleared
    /// environment: HOME and TMPDIR point at the agent's own dirs, and the
    /// process leads its own group so the whole tree can be stopped.
    #[cfg(unix)]
    pub fn command(&self, profile: &Path, program: &Path) -> Command {
        let base = self.base_env(false);
        let mut command = agent_command(
            profile,
            program,
            &borrowed(&base),
            std::iter::empty::<(&OsStr, Option<&OsStr>)>(),
        )
        .expect("the base agent environment holds no owner secret");
        command.current_dir(&self.project_root);
        command
    }

    /// The variables every command this sandbox builds starts with: HOME, TMPDIR (the short
    /// temp folder when granted, else the temp dir), PATH with the owner's rust proxies after
    /// any [`Self::allow_node_for`] folder, the C locale and [`kernel_sandbox::toolchains::agent_env`]. Only a
    /// harness's PATH starts with the agent's own `getconf` and `git`
    /// ([`kernel_sandbox::toolchains::with_agent_git`]). Takes whether it is a harness. Returns the variables.
    fn base_env(&self, harness: bool) -> Vec<(&'static str, OsString)> {
        let homes = kernel_sandbox::toolchains::ToolHomes::at(&self.owner_home);
        let tmp = self
            .owner_tmp
            .as_ref()
            .or(self.scratch.as_ref())
            .unwrap_or(&self.agent_tmp);
        let path = kernel_sandbox::toolchains::agent_path(&homes, AGENT_PATH);
        let path = match self.node_bin.as_deref().and_then(Path::to_str) {
            Some(bin) => format!("{bin}:{path}"),
            None => path,
        };
        let path = if harness {
            kernel_sandbox::toolchains::with_agent_git(self.hooks.toolchain, &self.agent_home, path)
        } else {
            path
        };
        let mut env = vec![
            ("HOME", self.agent_home.clone().into_os_string()),
            ("TMPDIR", tmp.clone().into_os_string()),
            ("PATH", OsString::from(path)),
            ("LANG", OsString::from("C")),
            ("LC_ALL", OsString::from("C")),
        ];
        env.extend(kernel_sandbox::toolchains::agent_env(
            &self.agent_home,
            &homes,
        ));
        if let Some((cache, uv)) = &self.tool_cache {
            if *uv {
                env.push((TOOL_CACHE_ENV[0], cache.join("uv").into_os_string()));
            }
            env.push((TOOL_CACHE_ENV[1], cache.join("pnpm-store").into_os_string()));
        }
        env
    }

    /// Rebuilds a harness launch command (absolute program, arguments,
    /// explicitly set environment, working directory inside the project) so it
    /// runs under the installed profile. HOME and TMPDIR always point at the
    /// agent's own dirs, whatever the harness set.
    pub fn wrap(&self, profile: &Path, harness: &Command) -> Result<Command, SandboxError> {
        let (program, cwd) = self.harness_start(harness)?;
        let base = self.base_env(true);
        let mut command = agent_command(profile, program, &borrowed(&base), harness.get_envs())?;
        command.args(harness.get_args()).current_dir(cwd);
        assert!(starts_at_boundary(command.get_program()));
        Ok(command)
    }

    /// Rebuilds a harness launch command for a Full access agent (DEC-102) as [`Self::wrap`]
    /// does, with the same checks and environment, but started by [`unconfined_command`]
    /// with no Seatbelt profile. Input: the harness command. Output: the command, or
    /// `Invalid` for an unsafe command.
    #[cfg(unix)]
    pub fn wrap_unconfined(&self, harness: &Command) -> Result<Command, SandboxError> {
        let (program, cwd) = self.harness_start(harness)?;
        let base = self.base_env(false);
        let mut command = unconfined_command(program, &borrowed(&base), harness.get_envs())?;
        command.args(harness.get_args()).current_dir(cwd);
        assert_ne!(command.get_program(), SANDBOX_EXEC);
        Ok(command)
    }

    /// Refuses a Full access harness command on Windows, where the level is unavailable.
    /// Input: the harness command. Output: always `Unavailable`.
    #[cfg(not(unix))]
    pub fn wrap_unconfined(&self, harness: &Command) -> Result<Command, SandboxError> {
        let _ = harness;
        Err(SandboxError::Unavailable(FULL_ACCESS_NEEDS_MACOS.into()))
    }

    fn harness_start<'a>(
        &'a self,
        harness: &'a Command,
    ) -> Result<(&'a Path, &'a Path), SandboxError> {
        let program = Path::new(harness.get_program());
        if !clean_absolute(program) {
            return Err(invalid("harness program must be an absolute plain path"));
        }
        let cwd = harness.get_current_dir().unwrap_or(&self.project_root);
        if !clean_absolute(cwd)
            || !std::iter::once(&self.project_root)
                .chain(self.worktrees.iter())
                .any(|root| cwd.starts_with(root))
        {
            return Err(invalid(
                "harness must start inside the project folder or its worktree",
            ));
        }
        Ok((program, cwd))
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
        if HIDDEN_UNHELD && !sandbox.hidden.is_empty() {
            return Err(SandboxError::Unavailable(HIDDEN_NEEDS_SEATBELT.into()));
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if !sandbox.kept_out.is_empty() || sandbox.open_tops.is_some() {
            return Err(SandboxError::Unavailable(HIDDEN_NEEDS_SEATBELT.into()));
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if !sandbox.shared_writes.is_empty() {
            return Err(SandboxError::Unavailable(
                SHARED_WRITES_NEED_SEATBELT.into(),
            ));
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if sandbox
            .agent_files
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

impl Checkout {
    /// The Seatbelt rules that keep a confined agent from adding to or changing the git
    /// objects the owner and the other agents share: no write in `info/` or `pack/`, nor of
    /// either folder entry, in any letter case, no new, moved or removed fan-out folder, no
    /// new object file by any route (create, rename or link) and no rewrite, removal or
    /// replacement of one, no hard link of one under a name it could change it through
    /// (rv-sa241 NV-1), no symlink anywhere in the store, and no mode, flag or ACL change
    /// of anything in it, which would keep the owner's git and every landing from reading an
    /// object (RV-DS241-185). Git reads a loose object before a packed one without checking
    /// its bytes, so any object an agent could add would stand in for the owner's own;
    /// object writes are the engine's, at turn end.
    /// Seatbelt regexes take no `{n}` counts, so the hex digits repeat. Input: the checkout.
    /// Output: the rule text.
    fn object_denies(&self) -> String {
        const HEX: &str = "[0-9a-fA-F]";
        let objects = self.objects.display();
        let pattern = seatbelt_pattern(&self.objects);
        let held = at_or_below(&format!(
            "{pattern}/({}|{})",
            any_case("info"),
            any_case("pack")
        ));
        let text = format!(
            "(deny file-write*\n  {held})\n(deny file-write-unlink\n  (literal \"{objects}\")\n  (regex #\"^{pattern}/{HEX}{HEX}$\"))\n(deny file-write-unlink file-write-data file-link\n  (regex #\"^{pattern}/{HEX}{HEX}/{HEX}+$\"))\n(deny file-write-create\n  (regex #\"^{pattern}/{HEX}{HEX}$\")\n  (regex #\"^{pattern}/{HEX}{HEX}/{HEX}+$\"))\n(deny file-write-create\n  (require-all (subpath \"{objects}\") (vnode-type SYMLINK)))\n(deny file-write-mode file-write-flags file-write-acl\n  (subpath \"{objects}\"))\n"
        );
        assert!(text.matches("(regex #\"^/").count() == 5);
        assert!(text.contains("(vnode-type SYMLINK)"));
        assert!(text.contains("(deny file-write-mode file-write-flags file-write-acl"));
        assert!(text.contains("(deny file-write-unlink file-write-data file-link"));
        text
    }
}

/// Reads one git link file the engine's own `git worktree add` wrote: a plain file of at
/// most 2 KiB holding `prefix`, one absolute or relative path and a newline. Input: the
/// file and its prefix. Output: the path text, or `Invalid` for any other file.
fn git_link(path: &Path, prefix: &str) -> Result<String, SandboxError> {
    use std::io::Read;
    let refused = || invalid("the worktree's git link is not a plain git link file");
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > GIT_LINK_BYTES_MAX {
        return Err(refused());
    }
    let mut text = String::new();
    fs::File::open(path)?
        .take(GIT_LINK_BYTES_MAX + 1)
        .read_to_string(&mut text)
        .map_err(|_| refused())?;
    let target = text
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix('\n'))
        .filter(|target| !target.is_empty() && !target.contains('\n'))
        .ok_or_else(refused)?;
    Ok(target.to_owned())
}

fn borrowed<'a>(env: &'a [(&'static str, OsString)]) -> Vec<(&'static str, &'a OsStr)> {
    env.iter()
        .map(|(name, value)| (*name, value.as_os_str()))
        .collect()
}

/// Makes sure a store a profile keeps read-only is a plain folder the engine made: creates
/// it empty, mode 0700 on unix, or keeps an entry already there only when it is a real
/// folder, never a file or a link. Disk state is input here, so a wrong entry is an error.
/// Input: the store path. Output: Ok, or `InvalidData` for any other entry at the path.
pub(crate) fn ensure_store(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    let created =
        std::os::unix::fs::DirBuilderExt::mode(&mut fs::DirBuilder::new(), 0o700).create(path);
    #[cfg(not(unix))]
    let created = fs::DirBuilder::new().create(path);
    match created {
        Err(problem) if problem.kind() == io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(path)?.is_dir() {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a submodule store must be a plain folder",
                ))
            }
        }
        made => made,
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

/// Whether every deny rule comes after the profile's last write allow, since Seatbelt applies
/// the last matching rule and a deny a later allow overrides no longer holds (CTX-M1 F1).
/// Inputs: the profile text and the deny rules as written in it. Output: false when any
/// deny is missing or comes before a write allow.
pub(crate) fn denies_follow_write_allows(text: &str, denies: &[String]) -> bool {
    let last_write_allow = text
        .rfind("(allow file-write*")
        .max(text.rfind("(allow file-read* file-write*"));
    denies
        .iter()
        .all(|deny| text.rfind(deny.as_str()) > last_write_allow)
}

/// The Seatbelt rules that keep a shared team folder free of links and moved-in folders: no
/// link may be made there and no folder but a lock, so no teammate places a path the lead's
/// own Claude Code writes through (TPH-2 C). Input: the folder. Output: the rules.
fn shared_denies(folder: &Path) -> String {
    let folder = folder.display();
    format!(
        "(deny file-write-create (require-all (subpath \"{folder}\") (vnode-type SYMLINK)))\n(deny file-write-create (require-all (subpath \"{folder}\") (vnode-type DIRECTORY) (require-not (regex #\"{LOCK_FOLDER}\"))))\n"
    )
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

/// The npm package folder a Node script harness sits in: `<prefix>/lib/node_modules/<name>`,
/// or `<prefix>/lib/node_modules/@scope/<name>` for a scoped package, which holds the
/// harness's own dependencies. Inputs: the prefix and the program under it. Output: the
/// folder, or none when the program is not inside one.
fn node_package(prefix: &Path, program: &Path) -> Option<PathBuf> {
    let modules = prefix.join("lib").join("node_modules");
    let mut parts = program.strip_prefix(&modules).ok()?.components();
    let first = parts.next()?.as_os_str();
    let package = if first.to_str()?.starts_with('@') {
        modules.join(first).join(parts.next()?.as_os_str())
    } else {
        modules.join(first)
    };
    (program != package.as_path() && program.starts_with(&package)).then_some(package)
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
                    launched.network
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
                kernel_sandbox::linux::helper::HIDDEN_MOUNTS_NOT_PRIVATE,
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
