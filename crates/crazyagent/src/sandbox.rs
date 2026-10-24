use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "windows")]
pub mod windows;

/// The macOS Seatbelt launcher every project agent process runs under.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

const PATH_BYTES_MAX: usize = 1024;
const READ_ROOTS_MAX: usize = 16;
const WORKTREES_MAX: usize = 8;
const REMOTES_MAX: usize = 8;
const PROFILE_BYTES_MAX: usize = 64 * 1024;
const AGENT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin";

const SYSTEM_READ_ROOTS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/System",
    "/Library/Apple",
    "/Library/Preferences",
    "/Library/Developer/CommandLineTools",
    "/private/etc",
    "/private/var/db",
    "/private/var/select",
    "/dev",
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/opt/homebrew/lib",
    "/opt/homebrew/libexec",
    "/opt/homebrew/opt",
    "/opt/homebrew/Cellar",
    "/opt/homebrew/Caskroom",
    "/opt/homebrew/share",
    "/opt/homebrew/etc",
    "/usr/local/bin",
    "/usr/local/lib",
    "/usr/local/opt",
    "/usr/local/Cellar",
    "/usr/local/share",
    "/usr/local/etc",
];

const METADATA_LITERALS: &[&str] = &[
    "/",
    "/etc",
    "/var",
    "/tmp",
    "/private",
    "/private/var",
    "/opt",
    "/opt/homebrew",
    "/usr/local",
    "/Library",
    "/Library/Developer",
];

const MACH_SERVICES: &[&str] = &[
    "com.apple.system.opendirectoryd.libinfo",
    "com.apple.system.opendirectoryd.membership",
    "com.apple.system.DirectoryService.libinfo_v1",
    "com.apple.PowerManagement.control",
    "com.apple.bsd.dirhelper",
    "com.apple.system.logger",
    "com.apple.logd",
    "com.apple.diagnosticd",
    "com.apple.system.notification_center",
    "com.apple.trustd",
    "com.apple.trustd.agent",
    "com.apple.ocspd",
    "com.apple.networkd",
    "com.apple.SystemConfiguration.DNSConfiguration",
    "com.apple.SystemConfiguration.configd",
];

const KEYCHAIN_SERVICES: &[&str] = &[
    "com.apple.SecurityServer",
    "com.apple.securityd",
    "com.apple.securityd.xpc",
    "com.apple.secd",
    "com.apple.securityd.ckks",
    "com.apple.securityd.sos",
    "com.apple.security.agent",
    "com.apple.security.agent.login",
    "com.apple.security.authhost",
    "com.apple.security.keychainsyncingoveridsproxy",
    "com.apple.security.cloudkeychainproxy3",
    "com.apple.security.keychain-circle-notification",
];

const OWNER_SECRET_PATHS: &[&str] = &[
    "Library/Keychains",
    ".ssh",
    ".gnupg",
    ".netrc",
    ".git-credentials",
    ".config/gh",
    ".config/git/credentials",
    ".aws",
    ".docker",
    ".kube",
    ".codex",
    ".claude",
    ".claude.json",
];

const SYSTEM_SECRET_PATHS: &[&str] = &["/Library/Keychains", "/private/var/db/SystemKey"];

const ENGINE_SECRET_FILES: &[&str] = &[
    "engine.db",
    "engine.db-wal",
    "engine.db-shm",
    "engine.db-journal",
    "owner.sock",
    "github.json",
    "github.toml",
];

const ENGINE_SECRET_FOLDERS: &[&str] = &["account-keys"];

const AGENT_KEY_VARIABLES: &[&str] = &["ANTHROPIC_API_KEY", "CRAZYAGENT_API_KEY"];

const AGENT_ENV_MAX: usize = 128;

const REFUSED_VARIABLES: &[&str] = &[
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "SSH_AUTH_SOCK",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GOOGLE_APPLICATION_CREDENTIALS",
];

const REFUSED_PREFIXES: &[&str] = &[
    "GH_",
    "GITHUB_",
    "GIT_CONFIG_KEY_",
    "GIT_CONFIG_VALUE_",
    "AWS_",
    "AZURE_",
    "CRAZYAGENT_",
];

const REFUSED_SUFFIXES: &[&str] = &[
    "_API_KEY",
    "_TOKEN",
    "_SECRET",
    "_SECRET_KEY",
    "_PASSWORD",
    "_ACCESS_KEY",
    "_CREDENTIALS",
];

/// The git settings every sandboxed agent process gets: no system git config (whose
/// `credential.helper=osxkeychain` would ask the owner's Keychain), no credential helper
/// at all, and no terminal prompt. They are set after the harness's own variables, so a
/// harness cannot replace them.
pub const AGENT_GIT_ENV: [(&str, &str); 5] = [
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_CONFIG_COUNT", "1"),
    ("GIT_CONFIG_KEY_0", "credential.helper"),
    ("GIT_CONFIG_VALUE_0", ""),
];

/// Tells whether an environment variable name may never reach an agent process: GitHub and
/// git credential settings, the owner's SSH agent, cloud and provider keys, tokens,
/// secrets, passwords and the engine's own `CRAZYAGENT_` settings, including the model key
/// names `ANTHROPIC_API_KEY` and `CRAZYAGENT_API_KEY`, which pass only with a placeholder
/// (see [`agent_variable_allowed`]). Input: the variable name. Output: true when refused.
pub fn agent_env_refused(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return true;
    };
    let upper = name.to_ascii_uppercase();
    REFUSED_VARIABLES.contains(&upper.as_str())
        || REFUSED_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        || REFUSED_SUFFIXES
            .iter()
            .any(|suffix| upper.ends_with(suffix))
}

/// Tells whether one variable may reach an agent process with this value: a name
/// [`agent_env_refused`] accepts, one of the fixed [`AGENT_GIT_ENV`] pairs, or a model key
/// name holding a CrazyAgent placeholder (the local placeholder or a key relay token),
/// never a real provider key. Inputs: the name and value. Output: true when allowed.
pub fn agent_variable_allowed(name: &OsStr, value: &OsStr) -> bool {
    if AGENT_GIT_ENV
        .iter()
        .any(|(git, fixed)| name == *git && value == *fixed)
    {
        return true;
    }
    let placeholder = value
        .to_str()
        .is_some_and(crate::model_gateway::relay::is_placeholder);
    if name
        .to_str()
        .is_some_and(|name| AGENT_KEY_VARIABLES.contains(&name))
    {
        return placeholder;
    }
    !agent_env_refused(name)
}

/// Builds the environment of every agent process in launch order: `base`, then the
/// harness's variables (a later value replaces an earlier one and None removes it, but
/// `HOME` and `TMPDIR` always stay as `base` set them), then [`AGENT_GIT_ENV`].
/// Inputs: the base and harness variables. Output: the variables, or `Invalid` when any
/// one fails [`agent_variable_allowed`].
pub fn agent_env<'a>(
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Vec<(OsString, OsString)>, SandboxError> {
    let refused = || invalid("agent environment holds an owner secret");
    let mut env: Vec<(OsString, OsString)> = Vec::new();
    let set = |env: &mut Vec<(OsString, OsString)>, name: &OsStr, value: Option<&OsStr>| {
        let at = env.iter().position(|(known, _)| known == name);
        match (at, value) {
            (Some(at), Some(value)) => env[at].1 = value.to_owned(),
            (Some(at), None) => {
                env.remove(at);
            }
            (None, Some(value)) => env.push((name.to_owned(), value.to_owned())),
            (None, None) => {}
        }
    };
    for (name, value) in base {
        if !agent_variable_allowed(OsStr::new(name), value) {
            return Err(refused());
        }
        set(&mut env, OsStr::new(name), Some(value));
    }
    for (name, value) in harness {
        if value.is_some_and(|value| !agent_variable_allowed(name, value)) {
            return Err(refused());
        }
        if name == "HOME" || name == "TMPDIR" {
            continue;
        }
        set(&mut env, name, value);
    }
    for (name, value) in AGENT_GIT_ENV {
        set(&mut env, OsStr::new(name), Some(OsStr::new(value)));
    }
    if env.len() > AGENT_ENV_MAX {
        return Err(invalid("agent environment holds too many variables"));
    }
    assert!(
        env.iter()
            .all(|(name, value)| agent_variable_allowed(name, value))
    );
    Ok(env)
}

/// Builds the one command every sandboxed headless agent process starts from: `-f
/// <profile> <program>` under `sandbox-exec`, a cleared environment holding exactly
/// [`agent_env`], and its own process group. Inputs: the installed profile, the absolute
/// program, the base variables and the harness's variables (None removes one).
/// Output: the command, or `Invalid` when any variable fails [`agent_variable_allowed`].
pub fn agent_command<'a>(
    profile: &Path,
    program: &Path,
    base: &[(&str, &OsStr)],
    harness: impl IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
) -> Result<Command, SandboxError> {
    let env = agent_env(base, harness)?;
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .arg("-f")
        .arg(profile)
        .arg(program)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)))
        .process_group(0);
    assert_eq!(command.get_program(), SANDBOX_EXEC);
    assert_eq!(command.get_envs().count(), env.len());
    Ok(command)
}

/// Network reach of a sandboxed agent: open when the agent's tools are on,
/// otherwise only the listed model endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxNetwork {
    Open,
    Only(Vec<SandboxRemote>),
}

/// One outbound TCP destination a tools-off agent may still reach. Seatbelt
/// filters by port only, either on loopback or on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxRemote {
    Loopback(u16),
    AnyHost(u16),
}

impl SandboxRemote {
    /// The destination a tools-off agent needs for one model endpoint: the URL's port on
    /// loopback for a local server, otherwise that port on any host. Input: an http or
    /// https URL. Output: the remote, or a refusal for another scheme, host or port.
    pub fn for_endpoint(url: &str) -> Result<Self, SandboxError> {
        let parsed = url::Url::parse(url).map_err(|_| invalid("endpoint URL is invalid"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(invalid("endpoint URL must be http or https"));
        }
        let port = parsed
            .port_or_known_default()
            .filter(|port| *port != 0)
            .ok_or_else(|| invalid("endpoint port is missing"))?;
        let loopback = match parsed.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => return Err(invalid("endpoint host is missing")),
        };
        assert_ne!(port, 0);
        Ok(if loopback {
            Self::Loopback(port)
        } else {
            Self::AnyHost(port)
        })
    }
}

/// Refusal to build or install a project sandbox, with a plain reason.
#[derive(Debug)]
pub enum SandboxError {
    Invalid(String),
    Unavailable(String),
    Io(io::Error),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "sandbox refused: {reason}"),
            Self::Unavailable(reason) => write!(f, "sandbox unavailable: {reason}"),
            Self::Io(error) => write!(f, "sandbox I/O: {error}"),
        }
    }
}

impl std::error::Error for SandboxError {}

impl From<io::Error> for SandboxError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// The Seatbelt boundary for one agent of one project: writes only inside the
/// project folder (never its `context/` or git hooks and config), the agent's
/// worktrees and its own home and temp dirs, reads of system paths plus those
/// roots, and network per [`SandboxNetwork`].
#[derive(Debug, Clone)]
pub struct ProjectSandbox {
    project_root: PathBuf,
    agent_home: PathBuf,
    agent_tmp: PathBuf,
    worktrees: Vec<PathBuf>,
    read_roots: Vec<PathBuf>,
    network: SandboxNetwork,
    owner_home: PathBuf,
    engine_runtime: Option<PathBuf>,
}

impl ProjectSandbox {
    /// Validates canonical, existing, quote-free directories that neither
    /// contain nor sit inside the owner's home, `~/.codex` or `~/.claude`.
    pub fn new(
        project_root: &Path,
        agent_home: &Path,
        agent_tmp: &Path,
        network: SandboxNetwork,
    ) -> Result<Self, SandboxError> {
        let owner = owner_home()?;
        for root in [project_root, agent_home, agent_tmp] {
            granted_root(root, &owner)?;
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
            project_root: project_root.to_path_buf(),
            agent_home: agent_home.to_path_buf(),
            agent_tmp: agent_tmp.to_path_buf(),
            worktrees: Vec::new(),
            read_roots: Vec::new(),
            network,
            owner_home: owner,
            engine_runtime: None,
        })
    }

    /// Names the engine runtime folder whose secrets this agent must never read or write:
    /// the engine database, the owner socket, the GitHub sign-in and settings files and the
    /// saved account keys. The agent's own home and temp dirs may sit inside it.
    /// Input: the existing runtime folder. Output: the guarded sandbox, or a refusal.
    pub fn guard_engine(mut self, runtime: &Path) -> Result<Self, SandboxError> {
        let runtime = runtime.canonicalize()?;
        if !clean_absolute(&runtime) || !runtime.is_dir() {
            return Err(invalid("engine runtime must be an existing plain folder"));
        }
        for secret in self.engine_secrets_in(&runtime) {
            if self.writable_roots().any(|root| root.starts_with(&secret)) {
                return Err(invalid("agent roots must not sit in engine secrets"));
            }
        }
        self.engine_runtime = Some(runtime);
        Ok(self)
    }

    fn engine_secrets_in(&self, runtime: &Path) -> Vec<PathBuf> {
        ENGINE_SECRET_FILES
            .iter()
            .chain(ENGINE_SECRET_FOLDERS)
            .map(|name| runtime.join(name))
            .collect()
    }

    /// Every path this sandbox denies to the agent although an allowed root might cover
    /// it: the owner's Keychains, SSH, GPG, cloud, gh, git, Codex and Claude credentials,
    /// the system Keychains and, once guarded, the engine runtime's secrets.
    pub fn secret_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = OWNER_SECRET_PATHS
            .iter()
            .map(|path| self.owner_home.join(path))
            .chain(SYSTEM_SECRET_PATHS.iter().map(PathBuf::from))
            .collect();
        if let Some(runtime) = &self.engine_runtime {
            paths.extend(self.engine_secrets_in(runtime));
        }
        paths
    }

    /// Adds one writable worktree the agent may also start in, for a checkout the
    /// engine keeps outside the project folder. Input: a canonical existing folder.
    /// Output: the widened sandbox, or a refusal for an unsafe or extra root.
    pub fn allow_worktree(mut self, worktree: &Path) -> Result<Self, SandboxError> {
        if self.worktrees.len() >= WORKTREES_MAX {
            return Err(invalid("too many worktrees"));
        }
        granted_root(worktree, &owner_home()?)?;
        self.worktrees.push(worktree.to_path_buf());
        assert!(self.worktrees.len() <= WORKTREES_MAX);
        Ok(self)
    }

    /// Adds one extra read-only root, such as a harness install directory.
    pub fn allow_read(mut self, root: &Path) -> Result<Self, SandboxError> {
        if self.read_roots.len() >= READ_ROOTS_MAX {
            return Err(invalid("too many read roots"));
        }
        granted_root(root, &owner_home()?)?;
        self.read_roots.push(root.to_path_buf());
        Ok(self)
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

    /// Paths inside the project folder no agent may write although the folder is writable:
    /// the `context/` folder at the project root and inside `.crazyagent/`, the git hooks,
    /// and the git config with its lock file. Every platform boundary keeps these read-only.
    pub fn protected_paths(&self) -> [PathBuf; 5] {
        let git = self.project_root.join(".git");
        [
            self.project_root.join("context"),
            self.project_root.join(".crazyagent").join("context"),
            git.join("hooks"),
            git.join("config"),
            git.join("config.lock"),
        ]
    }

    /// Every folder this sandbox lets the agent write: the project folder, its own
    /// home and temp dirs, then its worktrees.
    pub fn writable_roots(&self) -> impl Iterator<Item = &PathBuf> {
        [&self.project_root, &self.agent_home, &self.agent_tmp]
            .into_iter()
            .chain(self.worktrees.iter())
    }

    /// The Seatbelt profile text for this agent.
    pub fn profile(&self) -> String {
        let mut text = String::from("(version 1)\n(deny default)\n");
        text.push_str("(allow process-exec)\n(allow process-fork)\n");
        text.push_str("(allow signal (target same-sandbox))\n");
        text.push_str("(allow process-info* (target same-sandbox))\n");
        text.push_str("(allow sysctl-read)\n");
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
        for root in self.writable_roots().chain(self.read_roots.iter()) {
            text.push_str(&format!("\n  (path-ancestors \"{}\")", root.display()));
        }
        text.push_str(")\n(allow file-read* file-map-executable");
        for root in self.writable_roots().chain(self.read_roots.iter()) {
            text.push_str(&format!("\n  (subpath \"{}\")", root.display()));
        }
        text.push_str(")\n(allow file-write*");
        for root in self.writable_roots() {
            text.push_str(&format!("\n  (subpath \"{}\")", root.display()));
        }
        text.push_str(")\n");
        text.push_str("(deny file-read* file-write*");
        for secret in self.secret_paths() {
            if clean_absolute(&secret) {
                text.push_str(&format!("\n  (subpath \"{}\")", secret.display()));
            }
        }
        text.push_str(")\n(deny mach-lookup");
        for service in KEYCHAIN_SERVICES {
            text.push_str(&format!("\n  (global-name \"{service}\")"));
        }
        text.push_str(")\n");
        let project = self.project_root.display();
        text.push_str(&format!(
            "(deny file-write*\n  (subpath \"{project}/context\")\n  (subpath \"{project}/.crazyagent/context\")\n  (subpath \"{project}/.git/hooks\")\n  (literal \"{project}/.git/config\")\n  (literal \"{project}/.git/config.lock\"))\n"
        ));
        text.push_str(
            "(allow file-read* file-write-data file-ioctl\n  (literal \"/dev/null\")\n  (literal \"/dev/zero\")\n  (literal \"/dev/tty\")\n  (literal \"/dev/ptmx\")\n  (literal \"/dev/dtracehelper\")\n  (regex #\"^/dev/ttys[0-9]+$\")\n  (subpath \"/dev/fd\"))\n",
        );
        match &self.network {
            SandboxNetwork::Open => {
                text.push_str("(allow network*)\n");
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
            MACH_SERVICES
                .iter()
                .all(|service| !KEYCHAIN_SERVICES.contains(service))
        );
        assert!(text.len() <= PROFILE_BYTES_MAX);
        text
    }

    /// Writes the profile with mode 0600 at `path`, replacing any earlier one
    /// atomically. The path must lie outside every writable root, so no agent
    /// can rewrite the profile its next launch uses.
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
        let staging = parent.join(format!(".{name}.{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&staging)?;
        let written = file
            .write_all(self.profile().as_bytes())
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&staging, &target));
        if let Err(error) = written {
            let _ = fs::remove_file(&staging);
            return Err(error.into());
        }
        Ok(())
    }

    /// A command that runs `program` under the installed profile with a cleared
    /// environment: HOME and TMPDIR point at the agent's own dirs, and the
    /// process leads its own group so the whole tree can be stopped.
    pub fn command(&self, profile: &Path, program: &Path) -> Command {
        let mut command = agent_command(
            profile,
            program,
            &self.base_env(),
            std::iter::empty::<(&OsStr, Option<&OsStr>)>(),
        )
        .expect("the base agent environment holds no owner secret");
        command.current_dir(&self.project_root);
        command
    }

    fn base_env(&self) -> [(&str, &OsStr); 5] {
        [
            ("HOME", self.agent_home.as_os_str()),
            ("TMPDIR", self.agent_tmp.as_os_str()),
            ("PATH", OsStr::new(AGENT_PATH)),
            ("LANG", OsStr::new("C")),
            ("LC_ALL", OsStr::new("C")),
        ]
    }

    /// Rebuilds a harness launch command (absolute program, arguments,
    /// explicitly set environment, working directory inside the project) so it
    /// runs under the installed profile. HOME and TMPDIR always point at the
    /// agent's own dirs, whatever the harness set.
    pub fn wrap(&self, profile: &Path, harness: &Command) -> Result<Command, SandboxError> {
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
        let mut command = agent_command(profile, program, &self.base_env(), harness.get_envs())?;
        command.args(harness.get_args()).current_dir(cwd);
        assert_eq!(command.get_program(), SANDBOX_EXEC);
        Ok(command)
    }
}

/// One operating system's process boundary for a project agent: Seatbelt on macOS, Landlock
/// or bubblewrap on Linux, AppContainer with a job object on Windows. Each keeps the
/// [`ProjectSandbox`] policy: writes only in its writable roots minus the protected paths,
/// reads of system paths and its roots, and network per [`SandboxNetwork`].
pub trait Sandbox: Send + Sync {
    /// Rebuilds a harness launch command (absolute program, arguments, set environment,
    /// working directory inside the project or a worktree) to start inside the boundary.
    /// Output: the wrapped command, `Invalid` for an unsafe command, or `Unavailable` when
    /// this machine cannot hold the boundary.
    fn wrap(&self, harness: &Command) -> Result<Command, SandboxError>;
}

/// A project sandbox whose profile is already installed on disk. Launch paths take
/// this instead of a bare command, so no agent process can start outside its
/// boundary: Seatbelt on macOS, Landlock or bubblewrap on Linux. Windows has none yet,
/// so installing one there is refused.
#[derive(Debug)]
pub struct InstalledSandbox {
    sandbox: ProjectSandbox,
    profile: PathBuf,
    #[cfg(target_os = "linux")]
    linux: linux::LinuxSandbox,
}

impl InstalledSandbox {
    /// Installs the sandbox's profile at `profile` (see [`ProjectSandbox::install`]) and,
    /// on Linux, picks the Landlock helper next to the engine's executable or bubblewrap.
    /// Output: the sandbox bound to that profile, the install refusal, or `Unavailable`
    /// where this system has no boundary.
    pub fn install(sandbox: ProjectSandbox, profile: &Path) -> Result<Self, SandboxError> {
        #[cfg(target_os = "windows")]
        {
            let _ = (sandbox, profile);
            Err(SandboxError::Unavailable(
                windows::APPCONTAINER_MISSING.into(),
            ))
        }
        #[cfg(not(target_os = "windows"))]
        {
            sandbox.install(profile)?;
            assert!(profile.is_file());
            #[cfg(target_os = "linux")]
            let linux = {
                let helper = std::env::current_exe()?.with_file_name(linux::HELPER_NAME);
                linux::LinuxSandbox::detect(sandbox.clone(), &helper)?
            };
            Ok(Self {
                sandbox,
                profile: profile.to_path_buf(),
                #[cfg(target_os = "linux")]
                linux,
            })
        }
    }

    /// The program every wrapped command starts: `sandbox-exec` on macOS, the Landlock
    /// helper or bwrap on Linux.
    pub fn launcher(&self) -> &Path {
        #[cfg(target_os = "linux")]
        {
            self.linux.launcher()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Path::new(SANDBOX_EXEC)
        }
    }

    /// The sandbox this profile was generated from.
    pub fn sandbox(&self) -> &ProjectSandbox {
        &self.sandbox
    }

    /// The installed profile file `sandbox-exec` loads.
    pub fn profile(&self) -> &Path {
        &self.profile
    }

    /// Rebuilds a harness command to run inside this boundary; see [`ProjectSandbox::wrap`]
    /// on macOS and [`linux::LinuxSandbox`] on Linux.
    pub fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        #[cfg(target_os = "linux")]
        let command = Sandbox::wrap(&self.linux, harness)?;
        #[cfg(not(target_os = "linux"))]
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

fn invalid(reason: &str) -> SandboxError {
    SandboxError::Invalid(reason.into())
}

fn owner_home() -> Result<PathBuf, SandboxError> {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid())
        .map_err(|error| SandboxError::Io(io::Error::from(error)))?
        .ok_or_else(|| invalid("owner account is unknown"))?;
    let home = user.dir;
    if !clean_absolute(&home) {
        return Err(invalid("owner home must be absolute"));
    }
    Ok(home.canonicalize().unwrap_or(home))
}

fn granted_root(root: &Path, owner: &Path) -> Result<(), SandboxError> {
    if !clean_absolute(root) || root.components().count() < 3 {
        return Err(invalid("sandbox roots must be absolute, deep and plain"));
    }
    if owner.starts_with(root) {
        return Err(invalid("sandbox roots must not contain the owner home"));
    }
    for private in [".codex", ".claude"] {
        if root.starts_with(owner.join(private)) {
            return Err(invalid("sandbox roots must not be owner harness homes"));
        }
    }
    if !root.is_dir() || root.canonicalize()? != root {
        return Err(invalid(
            "sandbox roots must be existing canonical directories",
        ));
    }
    Ok(())
}

fn clean_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= PATH_BYTES_MAX
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
        && path.to_str().is_some_and(|text| {
            !text
                .chars()
                .any(|c| c.is_control() || c == '"' || c == '\\' || c == '\'')
        })
}
