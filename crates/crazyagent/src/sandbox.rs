use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod test_agent;
pub mod warden;
#[cfg(target_os = "windows")]
pub mod windows;

/// The macOS Seatbelt launcher every project agent process runs under.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Why a sandbox with hidden folders does not start on Linux or Windows: their boundaries
/// grant whole roots and cannot keep a folder inside one out of reach.
pub const HIDDEN_NEEDS_SEATBELT: &str = "hidden folders need the macOS sandbox";

const PATH_BYTES_MAX: usize = 1024;
const READ_ROOTS_MAX: usize = 16;
const WORKTREES_MAX: usize = 8;
const HIDDEN_MAX: usize = 4;
const GIT_LINK_BYTES_MAX: u64 = 2 * PATH_BYTES_MAX as u64;
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

const ENGINE_SECRET_FOLDERS: &[&str] = &["account-keys", crate::terminal::FOLDER];

const RUNTIME_ENTRIES_MAX: usize = 1024;

/// Why a sandbox root is refused when it equals, holds or sits inside a secret path from
/// [`ProjectSandbox::secret_paths`]: an allow-list boundary such as Landlock or bubblewrap
/// cannot carve the secret back out of a granted root.
pub const SECRET_ROOT_REFUSED: &str = "sandbox roots must not hold or sit in a secret path";

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

/// The root certificates every sandboxed agent process trusts for TLS on macOS: the system
/// bundle, named in `SSL_CERT_FILE`. A harness that reads its roots from the keychain, as
/// Codex does through rustls-native-certs, finds none there because every agent sandbox
/// denies the keychain services, and fails every HTTPS request. Elsewhere the harness's
/// own platform store is left as it is.
#[cfg(target_os = "macos")]
pub const AGENT_TLS_ENV: [(&str, &str); 1] = [("SSL_CERT_FILE", "/private/etc/ssl/cert.pem")];

/// No fixed TLS setting outside macOS: the platform's own certificate store is reachable.
#[cfg(not(target_os = "macos"))]
pub const AGENT_TLS_ENV: [(&str, &str); 0] = [];

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
/// `HOME` and `TMPDIR` always stay as `base` set them), then [`AGENT_GIT_ENV`] and
/// [`AGENT_TLS_ENV`].
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
    for (name, value) in AGENT_GIT_ENV.into_iter().chain(AGENT_TLS_ENV) {
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

/// The session shim a headless agent starts through inside `sandbox-exec`, followed by the
/// harness program and its arguments. It joins the engine's process group, starts a new
/// session and replaces itself with the harness, so the agent holds no controlling terminal
/// and its pid stays its process group. When the session cannot start it exits unstarted.
/// Handed a [`warden::WARDEN_FD`], it first forks the run's warden (see [`warden::Warden`])
/// and keeps that descriptor and variable from the harness.
#[cfg(unix)]
pub const SESSION_SHIM: [&str; 5] = [
    "/usr/bin/perl",
    "-mPOSIX",
    "-e",
    r#"setpgrp(0, getpgrp(getppid())) && POSIX::setsid() > 0 or die "crazyagent: no new session: $!\n"; if (defined(my $fd = delete $ENV{CRAZYAGENT_WARDEN_FD})) { my $engine = getppid(); $fd =~ /\A[0-9]{1,6}\z/ && $engine > 1 or die "crazyagent: no warden\n"; defined(my $warden = fork()) or die "crazyagent: no warden: $!\n"; if (!$warden) { POSIX::setsid(); POSIX::close($_) for 0, 1, 2; $SIG{PIPE} = "IGNORE"; open(my $line, "+<&=", $fd) or POSIX::_exit(1); my $armed = !kill(0, $engine) && $!{EPERM}; syswrite($line, $armed ? "A" : "U"); $armed or POSIX::_exit(0); my $byte; 1 while sysread($line, $byte, 1); syswrite($line, "F"); kill(9, -1); POSIX::_exit(1) } POSIX::close($fd) } exec {$ARGV[0]} @ARGV or die "crazyagent: cannot start $ARGV[0]: $!\n""#,
    "--",
];

/// Builds the one command every sandboxed headless agent process starts from: `-f
/// <profile>`, the [`SESSION_SHIM`] and `<program>` under `sandbox-exec`, a cleared
/// environment holding exactly [`agent_env`], and its own process group. Inputs: the
/// installed profile, the absolute program, the base variables and the harness's variables
/// (None removes one). Output: the command, or `Invalid` when a variable fails
/// [`agent_variable_allowed`].
#[cfg(unix)]
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
        .args(SESSION_SHIM)
        .arg(program)
        .env_clear()
        .envs(env.iter().map(|(name, value)| (name, value)))
        .process_group(0);
    assert_eq!(command.get_program(), SANDBOX_EXEC);
    assert_eq!(
        command.get_args().nth(2 + SESSION_SHIM.len()),
        Some(program.as_os_str())
    );
    assert_eq!(command.get_envs().count(), env.len());
    Ok(command)
}

/// Builds the one command every Windows agent process starts from: the boundary helper next
/// to the engine's executable with `--profile <profile> -- <program>`, and a cleared
/// environment holding exactly [`agent_env`]. Inputs: the installed boundary profile, the
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
    hidden: Vec<PathBuf>,
    network: SandboxNetwork,
    owner_home: PathBuf,
    engine_secrets: Vec<PathBuf>,
    checkout: Option<Checkout>,
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
    /// owner's home, do not sit inside `~/.codex` or `~/.claude`, and neither hold nor sit
    /// inside an owner or system secret path. Output: the sandbox, or `Invalid`.
    pub fn new(
        project_root: &Path,
        agent_home: &Path,
        agent_tmp: &Path,
        network: SandboxNetwork,
    ) -> Result<Self, SandboxError> {
        let owner = owner_home()?;
        let secrets: Vec<PathBuf> = owner_secrets(&owner).collect();
        for root in [project_root, agent_home, agent_tmp] {
            granted_root(root, &owner, &secrets)?;
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
            hidden: Vec::new(),
            network,
            owner_home: owner,
            engine_secrets: Vec::new(),
            checkout: None,
        })
    }

    /// Names the engine runtime folder whose secrets this agent must never read or write:
    /// the engine database, the owner socket, the GitHub sign-in and settings files, the
    /// saved account keys, the terminal server folder and every engine socket and socket
    /// folder found in it now. The agent's own home and temp dirs may sit inside it, but no
    /// root granted so far may hold or sit inside one of those secrets.
    /// Input: the existing runtime folder. Output: the guarded sandbox, or a refusal.
    pub fn guard_engine(mut self, runtime: &Path) -> Result<Self, SandboxError> {
        let runtime = canonical(runtime)?;
        if !clean_absolute(&runtime) || !runtime.is_dir() {
            return Err(invalid("engine runtime must be an existing plain folder"));
        }
        let secrets = engine_secrets_in(&runtime)?;
        for root in self.writable_roots().chain(self.read_roots.iter()) {
            clear_of_secrets(root, &secrets)?;
        }
        self.engine_secrets = secrets;
        Ok(self)
    }

    /// Every path this sandbox denies to the agent although an allowed root might cover
    /// it: the owner's Keychains, SSH, GPG, cloud, gh, git, Codex and Claude credentials,
    /// the system Keychains and, once guarded, the engine runtime's secrets.
    pub fn secret_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = owner_secrets(&self.owner_home).collect();
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
        granted_root(worktree, &owner_home()?, &self.secret_paths())?;
        self.worktrees.push(worktree.to_path_buf());
        assert!(self.worktrees.len() <= WORKTREES_MAX);
        Ok(self)
    }

    /// Confines the agent's writes to its own git worktree of the project: the worktree,
    /// its admin folder in `.git/worktrees` and the shared object store, beside its home,
    /// temp dir and extra worktrees. The project folder stays readable. Input: a canonical
    /// worktree in the project's `worktrees/` folder whose `.git` link and admin folder
    /// name each other. Output: the confined sandbox, or `Invalid`.
    pub fn confine_to_worktree(mut self, worktree: &Path) -> Result<Self, SandboxError> {
        if self.checkout.is_some() {
            return Err(invalid("the sandbox is already confined to a worktree"));
        }
        if self.read_roots.len() >= READ_ROOTS_MAX {
            return Err(invalid("too many read roots"));
        }
        let worktrees = crate::projects_home::state_folder(&self.project_root).join("worktrees");
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
        self.read_roots.push(self.project_root.clone());
        self.checkout = Some(Checkout {
            worktree: worktree.to_path_buf(),
            admin,
            objects,
        });
        Ok(self)
    }

    /// Adds one extra read-only root, such as a harness install directory. Input: a
    /// canonical existing folder that neither holds nor sits inside a path from
    /// [`Self::secret_paths`]. Output: the widened sandbox, or a refusal.
    pub fn allow_read(mut self, root: &Path) -> Result<Self, SandboxError> {
        if self.read_roots.len() >= READ_ROOTS_MAX {
            return Err(invalid("too many read roots"));
        }
        granted_root(root, &owner_home()?, &self.secret_paths())?;
        self.read_roots.push(root.to_path_buf());
        Ok(self)
    }

    /// Keeps one folder inside a granted root out of the agent's reach, read and write, as
    /// the secret paths are; the graph step hides the project's `.crazyagent/` from graphify.
    /// Input: a clean absolute path below a root, in a real parent folder, absent or a real
    /// folder. Output: the narrowed sandbox, or `Invalid`. Only Seatbelt holds it, so the
    /// Linux and Windows boundaries refuse a sandbox with hidden folders.
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
    /// and the git config with its lock file. Every platform boundary keeps these read-only,
    /// except that Windows guards the config but not its lock (`windows::GIT_CONFIG_LOCK`).
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

    /// Every folder this sandbox lets the agent write: the project folder, or for a
    /// confined agent its worktree, that worktree's git admin folder and the git object
    /// store, then its own home and temp dirs, then its extra worktrees.
    pub fn writable_roots(&self) -> impl Iterator<Item = &PathBuf> {
        let project: Vec<&PathBuf> = match &self.checkout {
            Some(checkout) => vec![&checkout.worktree, &checkout.admin, &checkout.objects],
            None => vec![&self.project_root],
        };
        project
            .into_iter()
            .chain([&self.agent_home, &self.agent_tmp])
            .chain(self.worktrees.iter())
    }

    /// Whether the agent could write `path` or move it away: true when the path is one of
    /// [`Self::writable_roots`] or inside one. Input: an absolute canonical path. Output:
    /// the answer.
    pub fn writes(&self, path: &Path) -> bool {
        self.writable_roots().any(|root| path.starts_with(root))
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
        for secret in self.secret_paths().into_iter().chain(self.hidden.clone()) {
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
            "(deny file-write*\n  (subpath \"{project}/context\")\n  (subpath \"{project}/.crazyagent/context\")\n  (subpath \"{project}/.git/hooks\")\n  (literal \"{project}/.git/config\")\n  (literal \"{project}/.git/config.lock\")\n  (literal \"{project}/.git\")\n  (literal \"{project}/.git/commondir\")\n  (literal \"{project}/.git/config.worktree\")\n  (subpath \"{project}/.git/info\")\n  (literal \"{project}/.git/shallow\")\n  (literal \"{project}/.git/objects\")\n  (literal \"{project}/.git/objects/info\")\n  (literal \"{project}/.git/objects/info/alternates\")\n  (literal \"{project}/.git/objects/info/http-alternates\")\n  (literal \"{project}/.crazyagent\"))\n"
        ));
        text.push_str(&self.git_link_denies());
        if let Some(checkout) = &self.checkout {
            text.push_str(&checkout.object_denies());
        }
        text.push_str(
            "(allow file-read*\n  (literal \"/dev\")\n  (literal \"/dev/random\")\n  (literal \"/dev/urandom\")\n  (literal \"/dev/stdin\")\n  (literal \"/dev/stdout\")\n  (literal \"/dev/stderr\"))\n(allow file-read* file-write-data file-ioctl\n  (literal \"/dev/null\")\n  (literal \"/dev/zero\")\n  (literal \"/dev/tty\")\n  (literal \"/dev/ptmx\")\n  (literal \"/dev/dtracehelper\")\n  (subpath \"/dev/fd\"))\n(allow file-read-metadata file-ioctl\n  (regex #\"^/dev/ttys[0-9]+$\"))\n(allow file-read* file-write-data\n  (require-all\n    (regex #\"^/dev/ttys[0-9]+$\")\n    (extension \"com.apple.sandbox.pty\")))\n",
        );
        let network = text.len();
        match &self.network {
            SandboxNetwork::Open => {
                text.push_str(
                    "(allow network-outbound (remote ip \"*:*\"))\n(allow network-inbound (local ip \"*:*\"))\n(allow network-bind (local ip \"*:*\"))\n(allow network-outbound (literal \"/private/var/run/mDNSResponder\"))\n",
                );
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
        assert!(!text.contains("(subpath \"/dev\")"));
        assert!(!text[network..].contains("network*") && !text[network..].contains("unix-socket"));
        assert!(
            MACH_SERVICES
                .iter()
                .all(|service| !KEYCHAIN_SERVICES.contains(service))
        );
        assert!(text.len() <= PROFILE_BYTES_MAX);
        text
    }

    /// The Seatbelt rules that keep the owner's git reading the metadata the engine made.
    /// No agent may move the project folder, the git worktrees folders or a worktree, which
    /// would carry protected files out of their denied paths and back. A worktree's `.git`
    /// file and its `commondir` and `gitdir` files may be created once, as `git worktree add`
    /// does, but never rewritten or removed, and no `config.worktree` may be written.
    /// Input: the policy. Output: the rule text.
    fn git_link_denies(&self) -> String {
        let project = self.project_root.display();
        let pattern = seatbelt_pattern(&self.project_root);
        let admin = format!("{pattern}/\\.git/worktrees/[^/]+");
        let checkouts = format!("{pattern}(/\\.crazyagent)?/worktrees/[^/]+");
        let mut moves = format!(
            "(deny file-write-unlink\n  (literal \"{project}\")\n  (literal \"{project}/.git/worktrees\")\n  (literal \"{project}/worktrees\")\n  (literal \"{project}/.crazyagent/worktrees\")\n  (regex #\"^{admin}$\")\n  (regex #\"^{checkouts}$\")"
        );
        let mut links = format!(
            "(deny file-write-unlink file-write-data\n  (regex #\"^{admin}/(commondir|gitdir)$\")\n  (regex #\"^{checkouts}/\\.git$\")"
        );
        for worktree in &self.worktrees {
            moves.push_str(&format!("\n  (literal \"{}\")", worktree.display()));
            links.push_str(&format!(
                "\n  (literal \"{}\")",
                worktree.join(".git").display()
            ));
        }
        let text = format!(
            "{moves})\n{links})\n(deny file-write*\n  (regex #\"^{admin}/config\\.worktree$\"))\n"
        );
        assert!(text.matches("(regex #\"^/").count() == 5);
        text
    }

    /// Writes the profile with mode 0600 at `path`, replacing any earlier one
    /// atomically. The path must lie outside every writable root, so no agent
    /// can rewrite the profile its next launch uses.
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

    /// Refuses to write a Seatbelt profile, which Windows cannot load; Windows agents start
    /// through [`InstalledSandbox`] and the AppContainer boundary. Takes the path, unused.
    /// Returns `Unavailable` with [`windows::SEATBELT_MISSING`].
    #[cfg(target_os = "windows")]
    pub fn install(&self, _path: &Path) -> Result<(), SandboxError> {
        Err(SandboxError::Unavailable(windows::SEATBELT_MISSING.into()))
    }

    /// A command that runs `program` under the installed profile with a cleared
    /// environment: HOME and TMPDIR point at the agent's own dirs, and the
    /// process leads its own group so the whole tree can be stopped.
    #[cfg(unix)]
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
/// boundary: Seatbelt on macOS, Landlock or bubblewrap on Linux, AppContainer with a job
/// object on Windows.
#[derive(Debug)]
pub struct InstalledSandbox {
    sandbox: ProjectSandbox,
    profile: PathBuf,
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
    /// [`windows::WindowsSandbox::install`]). Output: the sandbox bound to that profile, the
    /// install refusal, or `Unavailable` where this system has no boundary.
    pub fn install(sandbox: ProjectSandbox, profile: &Path) -> Result<Self, SandboxError> {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if !sandbox.hidden.is_empty() {
            return Err(SandboxError::Unavailable(HIDDEN_NEEDS_SEATBELT.into()));
        }
        #[cfg(target_os = "windows")]
        {
            let helper = windows::existing_helper(
                &std::env::current_exe()?.with_file_name(windows::HELPER_NAME),
            )?;
            let grants = windows::grants_folder()?;
            let windows =
                windows::WindowsSandbox::install(sandbox.clone(), &helper, profile, &grants)?;
            assert!(profile.is_file());
            Ok(Self {
                sandbox,
                profile: profile.to_path_buf(),
                windows,
            })
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
    /// helper or bwrap on Linux, the boundary helper on Windows.
    pub fn launcher(&self) -> &Path {
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

    /// Rebuilds a harness command to run inside this boundary; see [`ProjectSandbox::wrap`]
    /// on macOS, [`linux::LinuxSandbox`] on Linux and [`windows::WindowsSandbox`] on Windows.
    pub fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
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
    /// The Seatbelt rules that let a confined agent add git objects but never change the
    /// ones the owner and the other agents share: no write in `info/` or `pack/`, no move
    /// of the store or a fan-out folder, and no rewrite, removal, replacement or hard link
    /// of an object file. Git still creates new objects by link or rename. Seatbelt regexes
    /// take no `{n}` counts, so the hex digits repeat. Input: the checkout. Output: the
    /// rule text.
    fn object_denies(&self) -> String {
        const HEX: &str = "[0-9a-fA-F]";
        let objects = self.objects.display();
        let pattern = seatbelt_pattern(&self.objects);
        let text = format!(
            "(deny file-write*\n  (subpath \"{objects}/info\")\n  (subpath \"{objects}/pack\"))\n(deny file-write-unlink\n  (literal \"{objects}\")\n  (regex #\"^{pattern}/{HEX}{HEX}$\"))\n(deny file-write-unlink file-write-data\n  (regex #\"^{pattern}/{HEX}{HEX}/{HEX}+$\"))\n"
        );
        assert!(text.matches("(regex #\"^/").count() == 2);
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

/// Escapes a plain absolute path for a Seatbelt regex, so every character matches itself.
/// Input: a path that passes `clean_absolute`, so it holds no quote or backslash.
/// Output: the pattern text, without anchors.
fn seatbelt_pattern(path: &Path) -> String {
    let text = path.display().to_string();
    assert!(text.starts_with('/') && !text.contains('"') && !text.contains('\\'));
    let mut pattern = String::with_capacity(text.len() * 2);
    for character in text.chars() {
        if ".^$*+?()[]{}|".contains(character) {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern
}

fn invalid(reason: &str) -> SandboxError {
    SandboxError::Invalid(reason.into())
}

/// The owner's home folder from the user profile, canonical when it resolves, which
/// no sandbox root may contain. Takes nothing. Returns the folder, or `Invalid` when the
/// profile names none or a relative one.
#[cfg(target_os = "windows")]
fn owner_home() -> Result<PathBuf, SandboxError> {
    let home = std::env::home_dir().ok_or_else(|| invalid("owner account is unknown"))?;
    if !clean_absolute(&home) {
        return Err(invalid("owner home must be absolute"));
    }
    Ok(canonical(&home).unwrap_or(home))
}

#[cfg(unix)]
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

fn owner_secrets(owner: &Path) -> impl Iterator<Item = PathBuf> {
    OWNER_SECRET_PATHS
        .iter()
        .map(move |path| {
            path.split('/')
                .fold(owner.to_path_buf(), |at, part| at.join(part))
        })
        .chain(SYSTEM_SECRET_PATHS.iter().map(PathBuf::from))
}

/// The engine runtime's secrets: the fixed secret files and folders, plus every Unix socket
/// directly in the runtime and every runtime folder that directly holds one (owner
/// generations, runner roots, the terminal server). Input: the canonical runtime folder.
/// Output: the secret paths, or a refusal for a runtime or folder over the entry bound.
fn engine_secrets_in(runtime: &Path) -> Result<Vec<PathBuf>, SandboxError> {
    let mut secrets: Vec<PathBuf> = ENGINE_SECRET_FILES
        .iter()
        .chain(ENGINE_SECRET_FOLDERS)
        .map(|name| runtime.join(name))
        .collect();
    for entry in bounded_entries(runtime)? {
        if !secrets.contains(&entry) && engine_socket_place(&entry)? {
            secrets.push(entry);
        }
    }
    Ok(secrets)
}

#[cfg(unix)]
fn engine_socket_place(path: &Path) -> Result<bool, SandboxError> {
    use std::os::unix::fs::FileTypeExt;
    let Some(kind) = present_kind(path)? else {
        return Ok(false);
    };
    if kind.is_socket() {
        return Ok(true);
    }
    if !kind.is_dir() {
        return Ok(false);
    }
    let entries = match bounded_entries(path) {
        Err(SandboxError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(false);
        }
        other => other?,
    };
    for entry in entries {
        if present_kind(&entry)?.is_some_and(|kind| kind.is_socket()) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(unix)]
fn present_kind(path: &Path) -> Result<Option<fs::FileType>, SandboxError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata.file_type())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(unix))]
fn engine_socket_place(_path: &Path) -> Result<bool, SandboxError> {
    Ok(false)
}

fn bounded_entries(folder: &Path) -> Result<Vec<PathBuf>, SandboxError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(folder)? {
        if entries.len() == RUNTIME_ENTRIES_MAX {
            return Err(invalid("engine runtime folder has too many entries"));
        }
        entries.push(entry?.path());
    }
    Ok(entries)
}

fn clear_of_secrets(root: &Path, secrets: &[PathBuf]) -> Result<(), SandboxError> {
    if secrets
        .iter()
        .any(|secret| root.starts_with(secret) || secret.starts_with(root))
    {
        return Err(invalid(SECRET_ROOT_REFUSED));
    }
    Ok(())
}

fn granted_root(root: &Path, owner: &Path, secrets: &[PathBuf]) -> Result<(), SandboxError> {
    let depth = root
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    if !clean_absolute(root) || depth < 2 {
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
    clear_of_secrets(root, secrets)?;
    if !root.is_dir() || canonical(root)? != root {
        return Err(invalid(
            "sandbox roots must be existing canonical directories",
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
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

/// Tells whether a Windows path is absolute and plain: a drive prefix such as `C:` (never a
/// UNC, verbatim or device prefix), then folder names joined by `\`, as
/// [`windows_clean_text`] checks. Input: the path. Output: true when plain.
#[cfg(target_os = "windows")]
fn clean_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| match component {
            Component::Prefix(prefix) => matches!(prefix.kind(), std::path::Prefix::Disk(_)),
            Component::RootDir | Component::Normal(_) => true,
            Component::CurDir | Component::ParentDir => false,
        })
        && path.to_str().is_some_and(windows_clean_text)
}

/// Checks Windows path text on any system: a drive letter, `:\`, then nonempty folder names
/// joined by `\`, each neither `.` nor `..` nor ending in a space or dot, without control
/// characters, quotes, `/`, `:` or the characters Windows forbids in names, at most 1024
/// bytes. Input: the text. Output: true when plain.
#[cfg(any(target_os = "windows", test))]
fn windows_clean_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    if text.len() > PATH_BYTES_MAX
        || bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes[2] != b'\\'
    {
        return false;
    }
    text[3..].split('\\').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.ends_with(' ')
            && !part.ends_with('.')
            && !part.chars().any(|c| {
                c.is_control() || matches!(c, '"' | '\'' | '/' | ':' | '<' | '>' | '|' | '?' | '*')
            })
    })
}

/// The canonical form of an existing path. On Windows the verbatim `\\?\` prefix that
/// `canonicalize` adds to a drive path is removed, so the result equals the plain drive path;
/// any other verbatim form stays as it is and fails [`clean_absolute`]. Input: the path.
/// Output: the canonical path, or the I/O error.
pub(crate) fn canonical(path: &Path) -> io::Result<PathBuf> {
    let resolved = path.canonicalize()?;
    #[cfg(target_os = "windows")]
    if let Some(plain) = resolved
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
        && plain.as_bytes().get(1) == Some(&b':')
    {
        return Ok(PathBuf::from(plain));
    }
    Ok(resolved)
}

#[cfg(test)]
mod windows_path_tests {
    use super::windows_clean_text;

    #[test]
    fn a_plain_drive_path_is_clean_on_every_system() {
        for text in [
            r"C:\Users\owner\project",
            r"d:\work\agent home\tmp",
            r"C:\a",
        ] {
            assert!(windows_clean_text(text), "{text}");
        }
    }

    #[test]
    fn unc_verbatim_device_and_relative_paths_are_refused() {
        for text in [
            r"\\server\share\project",
            r"\\?\C:\project",
            r"\\?\UNC\server\share",
            r"\\.\PhysicalDrive0",
            r"C:project",
            r"project\folder",
            r"\project",
            "C:/project",
            r"C:\",
        ] {
            assert!(!windows_clean_text(text), "{text}");
        }
    }

    #[test]
    fn dots_quotes_streams_and_aliases_are_refused() {
        for text in [
            r"C:\project\..\owner",
            r"C:\project\.\context",
            r"C:\project\context.",
            r"C:\project\context ",
            r"C:\project\\context",
            r"C:\project\",
            r"C:\project\file.txt:stream",
            "C:\\project\\it's",
            "C:\\project\\\"quoted\"",
            "C:\\project\\line\nbreak",
            r"C:\project\a/b",
            r"C:\project\*",
            r"C:\project\a?b",
            r"C:\project\a|b",
        ] {
            assert!(!windows_clean_text(text), "{text:?}");
        }
    }

    #[test]
    fn a_path_longer_than_the_limit_is_refused() {
        let long = format!(r"C:\{}", "a".repeat(super::PATH_BYTES_MAX));
        assert!(!windows_clean_text(&long));
        let fits = format!(r"C:\{}", "a".repeat(super::PATH_BYTES_MAX - 3));
        assert!(windows_clean_text(&fits));
    }
}
