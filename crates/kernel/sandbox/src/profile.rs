/// The sandbox runner and the shims every confined agent starts through: the session and
/// pane shims, the pane keeper and its words, and the trait a boundary wraps a command with.
pub mod command;
/// The limits, the Seatbelt lists, the notices and refusal codes, and the variables the
/// engine itself sets in every agent's environment.
pub mod consts;
/// The names that tell an agent's own lineage: the lineage variable and token.
pub mod env;
/// The installed boundary every launch takes: the profile on disk and the platform launch.
pub mod installed;

/// The Seatbelt rule writers: the denies for app launches, a clone's object store and a
/// lead's clone, the root pins and nested git denies, and the packing and bound checks of
/// the regex strings a profile holds.
pub mod seatbelt;

/// The warden that stops a confined agent's whole run when its harness ends, and reports
/// whether it confirmed the sandbox empty.
pub mod warden;

/// The read, exec, tool, scratch, secret, shared folder and team grants a sandbox takes
/// after [`ProjectSandbox::new`], its read-only accessors and its Linux network checks.
/// Each grant checks its input and returns `Invalid` for one the profile cannot hold.
mod allows;

/// The rule writers that turn a sandbox into its Seatbelt profile text, and the install,
/// command and wrap steps that start an agent under that profile. Input: a built sandbox.
/// Output: the profile, the installed file and the confined command.
mod rules;

use crate::access::AccessHooks;
use crate::elf;
use crate::error::{SandboxError, invalid};
use crate::hidden_paths::HIDDEN_MAX;
use crate::host::LinuxHost;
use crate::lines::*;
use crate::net::{SandboxNetwork, SandboxRemote};
use crate::paths::{
    Kept, any_case, at_or_below, canonical, clean_absolute, fold_case, harness_homes, owner_home,
    owner_secrets, plain_top, regex_escape,
};
use crate::ports::{
    BRIDGE_PORT_MIN, KEPT_PORTS_MAX, OWNER_PORTS, PROXY_PORTS, RELAY_PORTS, REMOTES_MAX,
};
use crate::profile::command::*;
use crate::profile::consts::*;
use crate::profile::env::*;
use crate::profile::seatbelt::*;
use crate::settings::LinuxSockets;
use crate::shared_folders::OPEN_TOPS_MAX;
use crate::toolchains::ToolchainHooks;
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fs, io};

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
    /// The folder of Codex's own skills in its home: letters, digits and dots only, since the
    /// profile writes each of its characters into a bracket class.
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
    /// Builds a Windows agent's command through the engine's boundary helper. Inputs: the
    /// installed profile, the absolute program, the base and the harness variables (None
    /// removes one). Output: the command, or the engine's refusal.
    #[cfg(target_os = "windows")]
    pub windows_agent_command: WindowsAgentCommand,
    /// The engine's grants folder a Windows boundary is installed with, made when missing.
    /// Output: the folder, or the refusal.
    #[cfg(target_os = "windows")]
    pub grants_folder: fn() -> Result<PathBuf, SandboxError>,
}

/// How the engine builds a Windows agent's command: from the installed profile, the absolute
/// program, the base and the harness variables (None removes one), to the command or the
/// engine's refusal.
#[cfg(target_os = "windows")]
pub type WindowsAgentCommand = fn(
    &Path,
    &Path,
    &[(&str, &OsStr)],
    Vec<(&OsStr, Option<&OsStr>)>,
) -> Result<Command, SandboxError>;

/// Tells whether a launch holds the full Linux sandbox: a process space of its own (the nested
/// backend), the call supervisor running, and its network limits held, so none of the gap lines
/// would show for it. An open network has no limit to hold; a limited one needs TCP-only sockets
/// and, for a port on any host, a network of its own. Inputs: what the launch got and its sandbox.
/// Output: true when it holds all of it.
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

/// What a launch tells the owner in the agent's messages when a Linux that nests agents
/// still leaves it on this computer's network: for an agent that could have a network of
/// its own ([`ProjectSandbox::routed`]) on a host that does not route, [`LINUX_UNROUTED_LINE`]
/// or, with no loopback port or the supervisor on, [`LINUX_UNROUTED_ANY_HOST_LINE`]; none
/// when it routes; then [`linux_unbridged_line`] for the lowest port below
/// [`BRIDGE_PORT_MIN`], else under None [`LINUX_PORTS_ANY_HOST_LINE`]. Inputs: this Linux,
/// the network choice, the sandbox as launched. Output: the sentences, or none.
pub fn linux_unbridged_notice_on(
    host: Option<LinuxHost>,
    choice: crate::policy::AgentNetwork,
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
    let unsaid = choice == crate::policy::AgentNetwork::None && sandbox.bridged_ports().is_none();
    (reaches && unsaid).then(|| LINUX_PORTS_ANY_HOST_LINE.to_owned())
}

/// The Seatbelt boundary for one agent of one project: writes only inside the
/// project folder (never its `context/` or git hooks and config), the agent's
/// worktrees and its own home and temp dirs, never the launch files, instructions,
/// skills or settings its harness loads from that home, reads of system paths plus
/// those roots, and network per [`SandboxNetwork`].
#[derive(Debug, Clone)]
pub struct ProjectSandbox {
    pub(crate) hooks: &'static ProfileHooks,
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
    /// The walks this build took of the owner home, the harness homes and each secret, and of
    /// each path it granted, from [`Self::new`] until its install, which walks every secret
    /// again and checks each granted path against them; it is never shared with another build.
    kept: Kept,
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
    agent_files: Option<crate::policy::AgentFilesRule>,
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

/// What the engine's git may write, with a linked lead's rights, while it adds the lead's native
/// teammates' linked worktrees: the teammates' folder inside the lead's worktree and the project
/// repository's worktree records.
#[derive(Debug, Clone)]
pub struct TeammatesSetup {
    folder: PathBuf,
    records: PathBuf,
}

impl TeammatesSetup {
    /// The folder the teammates' clones sit in.
    #[cfg(target_os = "linux")]
    pub(crate) fn folder(&self) -> &PathBuf {
        &self.folder
    }
}

/// The one git worktree a confined agent writes in place of the whole project folder,
/// with the two parts of the project repository its commits need: the worktree's own
/// admin folder under `.git/worktrees` and the shared object store.
#[derive(Debug, Clone)]
pub struct Checkout {
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
        let harnesses = (hooks.access.harness_folders)();
        let secrets: Vec<PathBuf> = owner_secrets(&owner, &harnesses).collect();
        let homes = harness_homes(&owner, &harnesses);
        let mut kept = Kept::of(&owner, &homes, &secrets)?;
        for root in [project_root, agent_home, agent_tmp] {
            kept.grant(root, &secrets)?;
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
            kept,
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

    /// Adds the narrow extras the owner allowed the project's agents: each socket grant as a
    /// connect-and-read grant of its exact path in a list of its own, never bind, write or unlink;
    /// each port grant as an outbound loopback port when the network is limited (an open network
    /// reaches it already); and the temp folder grant as the owner's private temp folder, written
    /// as a root and handed over as `TMPDIR`, with every socket node in it kept unwritable. Inputs:
    /// the saved grants and the owner's temp folder as [`crate::grants::owner_temp_folder`] gives
    /// it, none when it cannot be given. Output: the widened sandbox, or `Invalid`; sockets and the
    /// temp folder need Seatbelt.
    pub fn grant_access(
        mut self,
        grants: &[crate::policy::AccessGrant],
        owner_tmp: Option<&Path>,
    ) -> Result<Self, SandboxError> {
        if grants.len() > crate::policy::GRANTS_MAX {
            return Err(invalid("too many access grants"));
        }
        for grant in grants {
            if !grant.plain() {
                return Err(invalid("an access grant is not plain"));
            }
            match grant {
                crate::policy::AccessGrant::Socket { path } => {
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
                        || self.granted.len() >= crate::policy::GRANT_SOCKETS_MAX
                    {
                        return Err(invalid("a socket grant cannot be held"));
                    }
                    self.granted.push(path.clone());
                }
                crate::policy::AccessGrant::Port { port } => {
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
                crate::policy::AccessGrant::TempFolder => {
                    if !cfg!(target_os = "macos") {
                        return Err(invalid("the temp folder grant needs Seatbelt"));
                    }
                    let tmp = owner_tmp
                        .ok_or_else(|| invalid("the owner's temp folder cannot be given"))?;
                    let secrets: Vec<PathBuf> =
                        owner_secrets(&self.owner_home, &(self.hooks.access.harness_folders)())
                            .collect();
                    self.kept.grant(tmp, &secrets)?;
                    let engine = self.engine_secrets.clone();
                    self.kept.clear(tmp, &engine)?;
                    self.owner_tmp = Some(tmp.to_path_buf());
                }
            }
        }
        assert!(self.granted.len() <= crate::policy::GRANT_SOCKETS_MAX);
        self.confinement_kept()?;
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

    /// Lets the agent bind and connect one Unix socket at an exact path, read and write that file
    /// and its `<path>.lock`, and read its folder's metadata, nothing else there: its own Codex
    /// app-server socket in the shared daemon folder. Input: a plain absolute path of letters,
    /// digits, `/`, `.`, `-` and `_` in an existing folder. Output: the widened sandbox, or
    /// `Invalid`; only Seatbelt can hold the grant.
    pub fn allow_unix_socket(mut self, socket: &Path) -> Result<Self, SandboxError> {
        self.socket_room(socket)?;
        if self.sockets.len() >= SOCKETS_MAX {
            return Err(invalid("too many unix sockets"));
        }
        self.sockets.push(socket.to_path_buf());
        assert!(self.sockets.len() <= SOCKETS_MAX && self.socket_grants() <= SOCKET_GRANTS_MAX);
        Ok(self)
    }

    /// Lets the agent serve one Unix socket at an exact path but never connect to it or unlink it:
    /// bind, create and change that file and its `<path>.lock`, and read its folder's metadata. A
    /// native lead's teammates run their tmux server this way, so none of them reaches it or swaps
    /// the node for a listener of its own. Input: as [`Self::allow_unix_socket`]. Output: the
    /// widened sandbox, or `Invalid`.
    pub fn serve_unix_socket(mut self, socket: &Path) -> Result<Self, SandboxError> {
        self.socket_room(socket)?;
        self.served.push(socket.to_path_buf());
        assert!(self.socket_grants() <= SOCKET_GRANTS_MAX);
        Ok(self)
    }

    /// Lets the agent connect to one Unix socket at an exact path and read it, never bind it or
    /// create, change, link or unlink a node there, such as a native lead reaching its teammates'
    /// tmux server and its teammates reaching the engine's caller socket. Input: as
    /// [`Self::allow_unix_socket`]. Output: the widened sandbox, or `Invalid`.
    pub fn reach_unix_socket(mut self, socket: &Path) -> Result<Self, SandboxError> {
        self.socket_room(socket)?;
        self.reached.push(socket.to_path_buf());
        assert!(self.socket_grants() <= SOCKET_GRANTS_MAX);
        Ok(self)
    }

    /// Keeps the agent from writing, unlinking or locking one Unix socket at an exact path inside
    /// its writable folders, with no grant to bind or reach it, such as a native lead's own tmux
    /// socket for its teammates. Input: as [`Self::allow_unix_socket`]. Output: the narrowed
    /// sandbox, or `Invalid`.
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
        for root in self.guarded_paths() {
            self.kept.clear(&root, &secrets)?;
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
        for root in self.guarded_paths() {
            if carved.as_ref().is_some_and(|(home, _)| *home == root) {
                self.kept.clear(&root, &around_home)?;
            } else {
                self.kept.clear(&root, &secrets)?;
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

    /// The paths a guard checks against the engine secrets it adds: every writable root, read
    /// root, tool root and tool file. Output: the paths, copied, in that order.
    fn guarded_paths(&self) -> Vec<PathBuf> {
        self.writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
            .chain(self.tool_files.iter())
            .cloned()
            .collect()
    }

    /// Checks, as the install writes the profile, that each path this build granted is the
    /// one it checked and holds or sits in no secret it was checked against, every path and
    /// secret walked again now, so one made, moved or swapped since is judged as it is then.
    /// Output: unit, or `Invalid` with [`crate::paths::GRANT_CHANGED`] or
    /// [`crate::paths::SECRET_ROOT_REFUSED`].
    pub(crate) fn secrets_still_clear(&self) -> Result<(), SandboxError> {
        self.kept.recheck()
    }

    /// Adds one writable worktree the agent may also start in, for a checkout the
    /// engine keeps outside the project folder. Input: a canonical existing folder that
    /// neither holds nor sits inside a path from [`Self::secret_paths`].
    /// Output: the widened sandbox, or a refusal for an unsafe or extra root.
    pub fn allow_worktree(mut self, worktree: &Path) -> Result<Self, SandboxError> {
        if self.worktrees.len() >= WORKTREES_MAX {
            return Err(invalid("too many worktrees"));
        }
        let secrets = self.secret_paths();
        self.kept.grant(worktree, &secrets)?;
        self.worktrees.push(worktree.to_path_buf());
        assert!(self.worktrees.len() <= WORKTREES_MAX);
        self.confinement_kept()?;
        Ok(self)
    }

    /// Lets git create a checkout's `.git` link in the project's worktrees folders and the `gitdir`
    /// and `commondir` files of its admin folder, which no other profile may plant. Only the
    /// engine's own worktree-add step installs it, and its callers confine and reinstall the
    /// profile before any agent runs. Output: the sandbox with that exemption.
    #[cfg(unix)]
    pub fn adding_worktrees(mut self) -> Self {
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
            self.kept.clear(root, &secrets)?;
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
        self.confinement_kept()?;
        Ok(self)
    }

    /// Confines an Own branch or Full access agent to its own clone of the project: the clone, with
    /// its git folder inside it, is the agent's only writable project path, so it commits on its
    /// own branch with full git while the project folder, its `.git` and the object store the clone
    /// borrows through `objects/info/alternates` stay read-only. Input: a canonical folder in the
    /// project's `worktrees/` folder whose `.git` is a real folder, not a link. Output: the
    /// confined sandbox, or `Invalid`.
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
        let secrets = self.secret_paths();
        self.kept.clear(worktree, &secrets)?;
        assert!(worktree.starts_with(&self.project_root) && worktree != self.project_root);
        self.read_roots.push(self.project_root.clone());
        self.clone = Some(worktree.to_path_buf());
        self.confinement_kept()?;
        Ok(self)
    }

    /// Names the branch a clone lead works on, as the engine records it, so its native teammates
    /// write only the refs of its `<branch>--<name>` teammate branches in the clone, never the
    /// lead's own. Input: an agent branch, as [`crate::agent_git::is_agent_branch`] says. Output:
    /// the sandbox, or `Invalid` before [`Self::confine_to_clone`], a second time or for any other
    /// branch.
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

    /// Confines the engine's own git, run with a linked lead's rights, to adding the lead's native
    /// teammates' linked worktrees: it writes only the teammates' folder of the lead's worktree,
    /// the project repository's `.git/worktrees` records and the refs and reflogs of the lead's own
    /// `<branch>--<name>` teammate branches with their `crazyagent` folders, beside the lead's home
    /// and temp dir; the project stays readable. Makes the teammates' folder as a plain folder.
    /// Inputs: the lead's canonical linked worktree in `worktrees/`, whose `.git` link names a
    /// record in `.git/worktrees`, and its agent branch as the engine records it. Output: the
    /// confined sandbox, `Invalid` or the disk error.
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
            self.kept.clear(root, &secrets)?;
        }
        assert!(
            [folder.as_path(), records.as_path()]
                .iter()
                .all(|root| root.starts_with(&self.project_root) && *root != self.project_root)
        );
        self.read_roots.push(self.project_root.clone());
        self.setup = Some(TeammatesSetup { folder, records });
        self.lead_branch = Some(branch.to_owned());
        self.confinement_kept()?;
        Ok(self)
    }
}

impl Checkout {
    /// The worktree the agent is confined to.
    #[cfg(target_os = "linux")]
    pub(crate) fn worktree(&self) -> &PathBuf {
        &self.worktree
    }

    /// The worktree's admin folder in the project's `.git/worktrees`.
    #[cfg(target_os = "linux")]
    pub(crate) fn admin(&self) -> &PathBuf {
        &self.admin
    }

    /// The object store the worktree's commits land in.
    #[cfg(target_os = "linux")]
    pub(crate) fn objects(&self) -> &PathBuf {
        &self.objects
    }

    /// The Seatbelt rules that keep a confined agent from adding to or changing the git objects the
    /// owner and the other agents share: no write in `info/` or `pack/`, nor of either folder
    /// entry, in any letter case, no new, moved or removed fan-out folder, no new object file by
    /// any route (create, rename or link) and no rewrite, removal or replacement of one, no hard
    /// link of one under a name it could change it through, no symlink anywhere in the store, and
    /// no mode, flag or ACL change of anything in it, which would keep the owner's git and every
    /// landing from reading an object. Git reads a loose object before a packed one without
    /// checking its bytes, so any object an agent could add would stand in for the owner's own;
    /// object writes are the engine's, at turn end. Seatbelt regexes take no `{n}` counts, so the
    /// hex digits repeat. Input: the checkout. Output: the rule text.
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

#[cfg(all(test, target_os = "linux"))]
mod interpreter_tests;
#[cfg(all(test, target_os = "macos"))]
mod tests;
#[cfg(all(test, target_os = "macos"))]
mod walk_tests;
