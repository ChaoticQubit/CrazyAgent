use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

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
    "com.apple.SecurityServer",
    "com.apple.trustd",
    "com.apple.trustd.agent",
    "com.apple.ocspd",
    "com.apple.networkd",
    "com.apple.SystemConfiguration.DNSConfiguration",
    "com.apple.SystemConfiguration.configd",
];

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
    Io(io::Error),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "sandbox refused: {reason}"),
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
        })
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
        let project = self.project_root.display();
        text.push_str(&format!(
            "(deny file-write*\n  (subpath \"{project}/context\")\n  (subpath \"{project}/.git/hooks\")\n  (literal \"{project}/.git/config\")\n  (literal \"{project}/.git/config.lock\"))\n"
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
        let mut command = Command::new(SANDBOX_EXEC);
        command
            .arg("-f")
            .arg(profile)
            .arg(program)
            .env_clear()
            .env("HOME", &self.agent_home)
            .env("TMPDIR", &self.agent_tmp)
            .env("PATH", AGENT_PATH)
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .current_dir(&self.project_root)
            .process_group(0);
        assert_eq!(command.get_program(), SANDBOX_EXEC);
        command
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
        let mut command = self.command(profile, program);
        command.args(harness.get_args()).current_dir(cwd);
        for (key, value) in harness.get_envs() {
            if key == "HOME" || key == "TMPDIR" {
                continue;
            }
            match value {
                Some(value) => command.env(key, value),
                None => command.env_remove(key),
            };
        }
        assert_eq!(command.get_program(), SANDBOX_EXEC);
        Ok(command)
    }
}

/// A project sandbox whose profile is already installed on disk. Launch paths take
/// this instead of a bare command, so no agent process can start outside its
/// Seatbelt boundary.
#[derive(Debug)]
pub struct InstalledSandbox {
    sandbox: ProjectSandbox,
    profile: PathBuf,
}

impl InstalledSandbox {
    /// Installs the sandbox's profile at `profile` (see [`ProjectSandbox::install`]).
    /// Output: the sandbox bound to that profile, or the install refusal.
    pub fn install(sandbox: ProjectSandbox, profile: &Path) -> Result<Self, SandboxError> {
        sandbox.install(profile)?;
        assert!(profile.is_file());
        Ok(Self {
            sandbox,
            profile: profile.to_path_buf(),
        })
    }

    /// The sandbox this profile was generated from.
    pub fn sandbox(&self) -> &ProjectSandbox {
        &self.sandbox
    }

    /// The installed profile file `sandbox-exec` loads.
    pub fn profile(&self) -> &Path {
        &self.profile
    }

    /// Rebuilds a harness command to run under this profile; see [`ProjectSandbox::wrap`].
    pub fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        let command = self.sandbox.wrap(&self.profile, harness)?;
        assert_eq!(command.get_args().nth(1), Some(self.profile.as_os_str()));
        Ok(command)
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
