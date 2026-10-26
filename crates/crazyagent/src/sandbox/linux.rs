use super::{
    ProjectSandbox, Sandbox, SandboxError, SandboxNetwork, SandboxRemote, agent_env,
    clean_absolute, invalid,
};
use landlock::{
    ABI, Access, AccessFs, AccessNet, NetPort, RestrictionStatus, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetError, RulesetStatus, path_beneath_rules,
};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// The Landlock helper's file name. It ships next to the engine's own executable.
pub const HELPER_NAME: &str = "crazyagent-landlock";

/// Fixed places a distribution installs bubblewrap. It is never looked up through `PATH`.
pub const BWRAP_PATHS: &[&str] = &["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"];

const AGENT_PATH: &str =
    "/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin:/home/linuxbrew/.linuxbrew/bin";
const GRANTS_MAX: usize = 1024;
const HELPER_PATHS_MAX: usize = GRANTS_MAX + 64;
const READ: &str = "--read";
const WRITE: &str = "--write";
const CONNECT: &str = "--connect";
const LIMIT_NETWORK: &str = "--limit-network";
const PROBE: &str = "--probe";
const FS_ABI: ABI = ABI::V3;
#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: i64 = 0x4000_0000;
const NET_ABI: ABI = ABI::V4;

const SYSTEM_READ_ROOTS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/etc",
    "/opt",
    "/run/systemd/resolve",
    "/home/linuxbrew/.linuxbrew",
    "/nix/store",
    "/sys/devices/system/cpu",
    "/sys/fs/cgroup",
];

const LANDLOCK_DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
    "/dev/ptmx",
    "/dev/pts",
    "/dev/shm",
];

/// The rules the Landlock helper applies to itself before it starts the harness: read and
/// execute beneath `read`, full access beneath `write`, and, when `limit_network` is set,
/// outbound TCP only to the `connect` ports. The engine renders it as helper arguments and
/// the helper parses it back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LandlockPolicy {
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
    pub connect: Vec<u16>,
    pub limit_network: bool,
}

impl LandlockPolicy {
    /// The helper arguments for this policy, without the `--` and the harness command.
    pub fn args(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        for path in &self.read {
            args.push(READ.into());
            args.push(path.into());
        }
        for path in &self.write {
            args.push(WRITE.into());
            args.push(path.into());
        }
        if self.limit_network {
            args.push(LIMIT_NETWORK.into());
        }
        for port in &self.connect {
            args.push(CONNECT.into());
            args.push(port.to_string().into());
        }
        args
    }
}

/// What the `crazyagent-landlock` helper was asked to do: probe whether this kernel holds a
/// Landlock ruleset, or apply a policy and start a harness.
#[derive(Debug, PartialEq, Eq)]
pub enum HelperRequest {
    Probe {
        limit_network: bool,
    },
    Run {
        policy: LandlockPolicy,
        program: PathBuf,
        args: Vec<OsString>,
    },
}

impl HelperRequest {
    /// Parses the helper arguments after its own name. Input: flags from
    /// [`LandlockPolicy::args`] then `--`, an absolute program and its arguments, or
    /// `--probe` with an optional `--limit-network`. Output: the request, or the reason the
    /// arguments are refused.
    pub fn parse(args: Vec<OsString>) -> Result<Self, String> {
        let mut policy = LandlockPolicy::default();
        let mut probe = false;
        let mut rest = args.into_iter();
        while let Some(flag) = rest.next() {
            match flag.to_str() {
                Some(PROBE) => probe = true,
                Some(LIMIT_NETWORK) => policy.limit_network = true,
                Some(READ) => policy.read.push(absolute(rest.next())?),
                Some(WRITE) => policy.write.push(absolute(rest.next())?),
                Some(CONNECT) => policy.connect.push(port(rest.next())?),
                Some("--") if !probe => {
                    let program = absolute(rest.next())?;
                    if policy.read.len() + policy.write.len() > HELPER_PATHS_MAX {
                        return Err("too many path grants".into());
                    }
                    return Ok(Self::Run {
                        policy,
                        program,
                        args: rest.collect(),
                    });
                }
                _ => return Err(format!("unexpected argument {}", flag.to_string_lossy())),
            }
        }
        if probe && policy.read.is_empty() && policy.write.is_empty() && policy.connect.is_empty() {
            return Ok(Self::Probe {
                limit_network: policy.limit_network,
            });
        }
        Err("missing the harness command after --".into())
    }
}

fn absolute(value: Option<OsString>) -> Result<PathBuf, String> {
    let path = PathBuf::from(value.ok_or("missing path")?);
    if !path.is_absolute() {
        return Err(format!("path must be absolute: {}", path.display()));
    }
    Ok(path)
}

fn port(value: Option<OsString>) -> Result<u16, String> {
    value
        .as_deref()
        .and_then(OsStr::to_str)
        .and_then(|text| text.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or_else(|| "port must be 1 to 65535".to_string())
}

fn enforce(policy: &LandlockPolicy) -> Result<RestrictionStatus, RulesetError> {
    let mut ruleset = Ruleset::default().handle_access(AccessFs::from_all(FS_ABI))?;
    if policy.limit_network {
        ruleset = ruleset.handle_access(AccessNet::from_all(NET_ABI))?;
    }
    let mut created = ruleset
        .create()?
        .add_rules(path_beneath_rules(
            &policy.read,
            AccessFs::from_read(FS_ABI),
        ))?
        .add_rules(path_beneath_rules(
            &policy.write,
            AccessFs::from_all(FS_ABI),
        ))?;
    for port in &policy.connect {
        created = created.add_rule(NetPort::new(*port, AccessNet::ConnectTcp))?;
    }
    created.restrict_self()
}

/// Installs the seccomp filter every agent process and its children keep: a new Unix-domain
/// socket and a new io_uring ring both fail with `EACCES`. Landlock does not mediate a
/// pathname socket connect, and a ring could create a socket without `socket(2)`;
/// `socketpair(2)` stays allowed since it reaches no path. Output: the installed filter, or
/// the reason this kernel or architecture cannot hold it.
fn refuse_unix_sockets() -> Result<(), String> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    rules.insert(libc::SYS_socket, unix_domain()?);
    rules.insert(libc::SYS_io_uring_setup, Vec::new());
    #[cfg(target_arch = "x86_64")]
    {
        rules.insert(X32_SYSCALL_BIT | libc::SYS_socket, unix_domain()?);
        rules.insert(X32_SYSCALL_BIT | libc::SYS_io_uring_setup, Vec::new());
    }
    let arch: TargetArch = std::env::consts::ARCH
        .try_into()
        .map_err(|error| format!("seccomp: {error}"))?;
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EACCES as u32),
        arch,
    )
    .map_err(|error| format!("seccomp: {error}"))?;
    let program: BpfProgram = filter
        .try_into()
        .map_err(|error| format!("seccomp: {error}"))?;
    assert!(!program.is_empty());
    seccompiler::apply_filter(&program).map_err(|error| format!("seccomp: {error}"))
}

fn unix_domain() -> Result<Vec<SeccompRule>, String> {
    let family = SeccompCondition::new(
        0,
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::Eq,
        libc::AF_UNIX as u64,
    )
    .map_err(|error| format!("seccomp: {error}"))?;
    let rule = SeccompRule::new(vec![family]).map_err(|error| format!("seccomp: {error}"))?;
    Ok(vec![rule])
}

fn accepted(status: &RestrictionStatus, limit_network: bool) -> bool {
    let held = match status.ruleset {
        RulesetStatus::FullyEnforced => true,
        RulesetStatus::PartiallyEnforced => !limit_network,
        RulesetStatus::NotEnforced => false,
    };
    held && status.no_new_privs
}

/// Moves the helper into a new session before it starts the harness, so the agent keeps no
/// controlling terminal of the engine. The helper starts as its own process group leader,
/// which `setsid` refuses, so it first joins its parent's group. Output: the new session,
/// or the reason it could not start.
fn new_session() -> Result<(), String> {
    let parent = rustix::process::getppid().ok_or("the helper has no parent process")?;
    let group = rustix::process::getpgid(Some(parent))
        .map_err(|error| format!("cannot read the engine's process group: {error}"))?;
    rustix::process::setpgid(None, Some(group))
        .map_err(|error| format!("cannot leave the agent's process group: {error}"))?;
    let session = rustix::process::setsid()
        .map_err(|error| format!("cannot start a new session: {error}"))?;
    assert_eq!(rustix::process::getpgid(None).ok(), Some(session));
    Ok(())
}

/// Entry point of the `crazyagent-landlock` helper. Input: its arguments after its own name.
/// A probe applies an empty ruleset and the Unix-socket filter to itself and exits 0 when
/// this kernel holds both; a run applies the policy and the filter to itself, starts a new
/// session and then replaces itself with the harness. Output: the exit code, 2 for refused
/// arguments, 3 when the kernel cannot hold the policy or the session, 126 when the
/// harness cannot start.
pub fn helper_main(args: Vec<OsString>) -> ExitCode {
    let request = match HelperRequest::parse(args) {
        Ok(request) => request,
        Err(reason) => {
            eprintln!("crazyagent-landlock: {reason}");
            return ExitCode::from(2);
        }
    };
    let (policy, launch) = match request {
        HelperRequest::Probe { limit_network } => (
            LandlockPolicy {
                limit_network,
                ..LandlockPolicy::default()
            },
            None,
        ),
        HelperRequest::Run {
            policy,
            program,
            args,
        } => (policy, Some((program, args))),
    };
    match enforce(&policy) {
        Ok(status) if accepted(&status, policy.limit_network) => {}
        Ok(status) => {
            eprintln!(
                "crazyagent-landlock: this kernel cannot hold the boundary ({:?})",
                status.ruleset
            );
            return ExitCode::from(3);
        }
        Err(error) => {
            eprintln!("crazyagent-landlock: {error}");
            return ExitCode::from(3);
        }
    }
    if let Err(reason) = refuse_unix_sockets() {
        eprintln!("crazyagent-landlock: {reason}");
        return ExitCode::from(3);
    }
    let Some((program, args)) = launch else {
        return ExitCode::SUCCESS;
    };
    if let Err(reason) = new_session() {
        eprintln!("crazyagent-landlock: {reason}");
        return ExitCode::from(3);
    }
    let error = Command::new(&program).args(args).exec();
    eprintln!(
        "crazyagent-landlock: cannot start {}: {error}",
        program.display()
    );
    ExitCode::from(126)
}

/// The Linux mechanism holding a project agent's boundary: the `crazyagent-landlock` helper,
/// or bubblewrap where the kernel has no Landlock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinuxBackend {
    Landlock { helper: PathBuf },
    Bubblewrap { bwrap: PathBuf },
}

/// The Linux boundary for one project agent. It keeps the [`ProjectSandbox`] policy on both
/// backends: the project folder is read-only, its existing entries are writable except the
/// protected paths and symbolic links, or for a confined agent only its own worktree with that
/// worktree's git admin folder and the object store, and the agent's home, temp dir and
/// worktrees are writable. Landlock limits outbound TCP to the model endpoint ports;
/// bubblewrap cannot, so a tools-off agent needs Landlock.
#[derive(Debug)]
pub struct LinuxSandbox {
    sandbox: ProjectSandbox,
    backend: LinuxBackend,
}

impl LinuxSandbox {
    /// Binds the policy to one backend. Input: the project policy and the helper or bwrap
    /// executable as an absolute file. Output: the sandbox, `Invalid` for a bad executable,
    /// or `Unavailable` for a network limit on bubblewrap.
    pub fn new(sandbox: ProjectSandbox, backend: LinuxBackend) -> Result<Self, SandboxError> {
        let executable = match &backend {
            LinuxBackend::Landlock { helper } => helper,
            LinuxBackend::Bubblewrap { bwrap } => bwrap,
        };
        if !clean_absolute(executable) || !executable.is_file() {
            return Err(invalid("sandbox executable must be an absolute file"));
        }
        if matches!(backend, LinuxBackend::Bubblewrap { .. })
            && matches!(sandbox.network(), SandboxNetwork::Only(_))
        {
            return Err(SandboxError::Unavailable(
                "limiting an agent to its model endpoints needs Landlock network rules \
                 (Linux 6.7 or newer); bubblewrap cannot limit ports"
                    .into(),
            ));
        }
        Ok(Self { sandbox, backend })
    }

    /// Picks the backend this machine holds: Landlock when the helper exists and its probe
    /// passes for the policy's network needs, otherwise bubblewrap from a fixed path. Input:
    /// the project policy and the helper executable. Output: the sandbox, or `Unavailable`
    /// when neither backend can hold it.
    pub fn detect(sandbox: ProjectSandbox, helper: &Path) -> Result<Self, SandboxError> {
        let mut probe = Command::new(helper);
        probe.arg(PROBE);
        if matches!(sandbox.network(), SandboxNetwork::Only(_)) {
            probe.arg(LIMIT_NETWORK);
        }
        let held = clean_absolute(helper)
            && helper.is_file()
            && crate::spawn_gate::status(
                probe
                    .env_clear()
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .is_ok_and(|status| status.success());
        if held {
            return Self::new(
                sandbox,
                LinuxBackend::Landlock {
                    helper: helper.to_path_buf(),
                },
            );
        }
        match BWRAP_PATHS
            .iter()
            .map(Path::new)
            .find(|path| path.is_file())
        {
            Some(bwrap) => Self::new(
                sandbox,
                LinuxBackend::Bubblewrap {
                    bwrap: bwrap.to_path_buf(),
                },
            ),
            None => Err(SandboxError::Unavailable(
                "this Linux enforces no Landlock and has no bubblewrap; install bubblewrap".into(),
            )),
        }
    }

    /// The backend this sandbox launches through.
    pub fn backend(&self) -> &LinuxBackend {
        &self.backend
    }

    /// The project policy this sandbox holds.
    pub fn sandbox(&self) -> &ProjectSandbox {
        &self.sandbox
    }

    /// The program every wrapped command starts: the Landlock helper or bwrap.
    pub fn launcher(&self) -> &Path {
        match &self.backend {
            LinuxBackend::Landlock { helper } => helper,
            LinuxBackend::Bubblewrap { bwrap } => bwrap,
        }
    }
}

impl Sandbox for LinuxSandbox {
    fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        let program = Path::new(harness.get_program());
        if !clean_absolute(program) {
            return Err(invalid("harness program must be an absolute plain path"));
        }
        let cwd = harness
            .get_current_dir()
            .unwrap_or(&self.sandbox.project_root);
        if !clean_absolute(cwd)
            || !std::iter::once(&self.sandbox.project_root)
                .chain(self.sandbox.worktrees.iter())
                .any(|root| cwd.starts_with(root))
        {
            return Err(invalid(
                "harness must start inside the project folder or its worktree",
            ));
        }
        let grants = Grants::of(&self.sandbox)?;
        let mut command = match &self.backend {
            LinuxBackend::Landlock { helper } => {
                let mut command = Command::new(helper);
                command.args(landlock_policy(&grants, self.sandbox.network()).args());
                command
            }
            LinuxBackend::Bubblewrap { bwrap } => bwrap_command(bwrap, &grants, cwd),
        };
        let env = agent_env(
            &[
                ("HOME", self.sandbox.agent_home().as_os_str()),
                ("TMPDIR", self.sandbox.agent_tmp().as_os_str()),
                ("PATH", OsStr::new(AGENT_PATH)),
                ("LANG", OsStr::new("C")),
                ("LC_ALL", OsStr::new("C")),
            ],
            harness.get_envs(),
        )?;
        command
            .arg("--")
            .arg(program)
            .args(harness.get_args())
            .env_clear()
            .envs(env.iter().map(|(name, value)| (name, value)))
            .current_dir(cwd)
            .process_group(0);
        assert!(command.get_args().any(|arg| arg == "--"));
        Ok(command)
    }
}

struct Grants {
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
}

impl Grants {
    fn of(sandbox: &ProjectSandbox) -> Result<Self, SandboxError> {
        let project = sandbox.project_root();
        let git = project.join(".git");
        let state = project.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
        let protected = sandbox.protected_paths();
        let mut write = Vec::new();
        for root in sandbox.writable_roots() {
            if root != project {
                write.push(root.clone());
                continue;
            }
            writable_children(project, &[&git, &state], &protected, &mut write)?;
            for folder in [&git, &state] {
                if fs::symlink_metadata(folder).is_ok_and(|meta| meta.is_dir()) {
                    writable_children(folder, &[], &protected, &mut write)?;
                }
            }
        }
        let mut read = vec![project.to_path_buf()];
        read.extend(
            sandbox
                .read_roots()
                .iter()
                .filter(|root| *root != project)
                .cloned(),
        );
        if read.len() + write.len() > GRANTS_MAX {
            return Err(invalid("project folder has too many entries to grant"));
        }
        if write.iter().any(|path| {
            protected
                .iter()
                .any(|item| path.starts_with(item) || item.starts_with(path))
        }) {
            return Err(invalid("a writable root sits inside a protected path"));
        }
        let secrets = sandbox.secret_paths();
        assert!(read.iter().chain(&write).all(|grant| {
            secrets
                .iter()
                .all(|secret| !grant.starts_with(secret) && !secret.starts_with(grant))
        }));
        Ok(Self { read, write })
    }
}

fn writable_children(
    dir: &Path,
    skip: &[&PathBuf],
    protected: &[PathBuf],
    write: &mut Vec<PathBuf>,
) -> Result<(), SandboxError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() || skip.contains(&&path) || protected.contains(&path) {
            continue;
        }
        if kind.is_dir() || kind.is_file() {
            write.push(path);
        }
        if write.len() > GRANTS_MAX {
            return Err(invalid("project folder has too many entries to grant"));
        }
    }
    Ok(())
}

fn landlock_policy(grants: &Grants, network: &SandboxNetwork) -> LandlockPolicy {
    let mut read: Vec<PathBuf> = SYSTEM_READ_ROOTS
        .iter()
        .chain(&["/proc"])
        .map(PathBuf::from)
        .collect();
    read.extend(grants.read.iter().cloned());
    let mut write: Vec<PathBuf> = LANDLOCK_DEVICES.iter().map(PathBuf::from).collect();
    write.extend(grants.write.iter().cloned());
    let (limit_network, connect) = match network {
        SandboxNetwork::Open => (false, Vec::new()),
        SandboxNetwork::Only(remotes) => (true, ports(remotes)),
    };
    assert!(
        read.len() + write.len() <= HELPER_PATHS_MAX,
        "the helper accepts every policy the engine writes"
    );
    LandlockPolicy {
        read,
        write,
        connect,
        limit_network,
    }
}

fn ports(remotes: &[SandboxRemote]) -> Vec<u16> {
    let mut ports: Vec<u16> = remotes
        .iter()
        .map(|remote| match remote {
            SandboxRemote::Loopback(port) | SandboxRemote::AnyHost(port) => *port,
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

fn bwrap_command(bwrap: &Path, grants: &Grants, cwd: &Path) -> Command {
    let mut command = Command::new(bwrap);
    command.args([
        "--die-with-parent",
        "--new-session",
        "--unshare-ipc",
        "--unshare-pid",
        "--unshare-uts",
        "--unshare-cgroup-try",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
    ]);
    for &root in SYSTEM_READ_ROOTS {
        command.args(["--ro-bind-try", root, root]);
    }
    for root in &grants.read {
        command.arg("--ro-bind").arg(root).arg(root);
    }
    for root in &grants.write {
        command.arg("--bind").arg(root).arg(root);
    }
    command.arg("--chdir").arg(cwd);
    command
}
