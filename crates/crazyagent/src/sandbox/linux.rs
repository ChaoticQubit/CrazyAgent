use super::model_proxy::{PICKED_PORTS_START, PROXY_PORTS, RELAY_PORTS};
use super::{
    BRIDGE_PORT_MIN, GIT_STATE_FILES, GIT_STATE_TREES, KEPT_PORTS_MAX, LINUX_HARNESS_UNCHECKED,
    LINUX_LIMITED_NETWORK_OFF, LINUX_OPEN_NETWORK_NEEDS_LANDLOCK, LINUX_RESOLVER_NEEDS_UDP,
    LinuxSockets, LinuxSupervisor, OWNER_PORTS, ProjectSandbox, REMOTES_MAX, Routed, Sandbox,
    SandboxError, SandboxNetwork, SandboxRemote, agent_env, clean_absolute, invalid,
};
use crazyagent_linux_supervisor::rules::{GitOwn, Reach, is_git};
use crazyagent_linux_supervisor::serve::{self, Supervised};
use landlock::{
    ABI, Access, AccessFs, AccessNet, NetPort, RestrictionStatus, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetError, RulesetStatus, Scope, path_beneath_rules,
};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use std::collections::BTreeMap;
use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::ops::RangeInclusive;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

mod bridge;
mod egress;

pub use bridge::Bridge;
pub use egress::{NETWORK_SHARED, SLIRP_PATHS};

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
const OPEN_SOCKETS: &str = "--open-sockets";
const KEEP_FREE: &str = "--keep-free";
const PROBE: &str = "--probe";
const FILTER_SOCKETS: &str = "--filter-sockets";
const DIE_WITH: &str = "--die-with";
const NEST: &str = "--nest";
const NESTED: &str = "--nested";
const SCOPED: &str = "--scoped";
const GUARD_LOOPBACK: &str = "--guard-loopback";
const GUARD_GIT: &str = "--guard-git";
const OWN_TREE: &str = "--own-tree";
const OWN_KIDS: &str = "--own-kids";
const OWN_GIT: &str = "--own-git";
const GUARD_SIGNALS: &str = "--guard-signals";
const PROBE_SUPERVISOR: &str = "--probe-supervisor";
const PROBE_SUPERVISED: &str = "--probe-supervised";
/// Where AppArmor says whether it keeps unprivileged programs out of user namespaces, as on
/// Ubuntu 23.10 and newer: `1` when it does ([`restricts`]).
const APPARMOR_USERNS: &str = "/proc/sys/kernel/apparmor_restrict_unprivileged_userns";
/// Why the helper runs no mode while its environment holds a variable that makes glibc
/// load code ([`loader_variable`]): every mode, and the bubblewrap a nest starts, runs in
/// the helper's own AppArmor profile, which may make user namespaces, so the variable would
/// load a caller's code there (DEC-179 SBX-3 fold, rv-sa241 MED-1, rv-ds241 F2). The engine
/// never sets one.
pub const LOADER_REFUSED: &str = "the helper refuses variables that load code";
/// How long [`host_now`] keeps what it read: a policy view asks on every read, and each
/// answer starts the helper twice.
const HOST_AGE_MAX: Duration = Duration::from_secs(60);
/// The longest the call supervisor probe may run: its one wait is a 2 s connect timeout,
/// reached only when a guard does not hold (rv-sec3 linux-t4b N4).
const SUPERVISOR_PROBE_TIME_MAX: Duration = Duration::from_secs(10);
/// How the name of each private folder a supervisor probe runs in starts ([`private_folder`]).
const PROBE_FOLDER_PREFIX: &str = "supervisor-probe-";
/// The most names [`private_folder`] tries: each is 128 random bits, so only a name someone
/// else already made, or a broken random source, ever takes one.
const PROBE_FOLDER_TRIES_MAX: usize = 4;
/// How often [`passes_within`] looks whether its probe has ended.
const PROBE_POLL: Duration = Duration::from_millis(20);
/// The set-user-ID and set-group-ID mode bits: a bubblewrap with either runs as someone
/// other than the engine's user, so [`trusted`] refuses it.
const SET_ID_BITS: u32 = 0o6000;
/// The bubblewrap arguments a nested agent starts under, before its working folder: user,
/// process, IPC and host-name namespaces of its own and a fresh `/proc`, with the whole
/// root bound through, devices too, so the helper's Landlock rules stay the only file
/// policy. `--die-with-parent` ties the namespace to bubblewrap's outer process, never to
/// the engine. The network stays shared, but for an agent whose ports are bridged:
/// [`nest_command`] then adds `--unshare-net` after these.
pub const NEST_ARGS: [&str; 10] = [
    "--die-with-parent",
    "--unshare-user",
    "--unshare-pid",
    "--unshare-ipc",
    "--unshare-uts",
    "--dev-bind",
    "/",
    "/",
    "--proc",
    "/proc",
];
const RESOLVER_ROOT: &str = "/run/systemd/resolve";
const HELPER_MISSING: &str = "this Linux cannot fully enforce Landlock file rules (Linux 6.2 \
     or newer), and CrazyAgent's sandbox helper, which keeps a bubblewrap agent off Unix \
     sockets, is missing; reinstall CrazyAgent";
/// The most ports one agent is kept off: the owner's fixed ports, the kept local model
/// ports and every model proxy and model relay port.
const KEEP_FREE_MAX: usize = OWNER_PORTS.len()
    + KEPT_PORTS_MAX
    + (*PROXY_PORTS.end() - *PROXY_PORTS.start()) as usize
    + 1
    + (*RELAY_PORTS.end() - *RELAY_PORTS.start()) as usize
    + 1;
/// The socket family of SMC, whose listener holds a TCP port and takes plain TCP clients.
const AF_SMC: u64 = 43;
const PICKED_PORTS_FILE: &str = "/proc/sys/net/ipv4/ip_local_port_range";
const PICKED_PORTS_BYTES_MAX: u64 = 64;
const PICKED_PORTS_UNREADABLE: &str = "CrazyAgent cannot read which ports this Linux hands out \
     by itself (net.ipv4.ip_local_port_range), so it cannot keep the owner's ports free of an \
     agent";
const _: () = {
    let mut index = 0;
    while index < OWNER_PORTS.len() {
        assert!(OWNER_PORTS[index] < PICKED_PORTS_START);
        index += 1;
    }
};
const FS_ABI: ABI = ABI::V3;
#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: i64 = 0x4000_0000;
const NET_ABI: ABI = ABI::V4;
const SOCKET_TYPE_MASK: u64 = 0xf;
const DNS_PORT: u16 = 53;
const DNS_OVER_TCP: &str = "use-vc";
const LIMITED_NETWORK_OFF: &str = "the Linux limited-network sockets setting is 'Do not start \
     limited-network agents'; choose 'TCP only' or 'TCP ports only' in Settings";
const RESOLVER_NEEDS_UDP: &str = "this harness cannot look up names over TCP, which the Linux \
     limited-network sockets setting 'TCP only' needs; choose 'TCP ports only' in Settings, which \
     lets UDP and other sockets reach any host";
const HARNESS_UNCHECKED: &str = "this harness starts through a script CrazyAgent cannot check for \
     the Linux limited-network sockets setting 'TCP only'; start it from a program, or a script \
     whose first line names a program by its full path, or choose 'TCP ports only' in Settings, \
     which lets UDP and other sockets reach any host";
const SCOPE_ABI: ABI = ABI::V6;

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
    RESOLVER_ROOT,
    "/home/linuxbrew/.linuxbrew",
    "/nix/store",
    "/sys/devices/system/cpu",
    "/sys/fs/cgroup",
];

/// The entries of a project's `.git` that the owner's and the engine's git follow to other
/// repository data, which the Seatbelt profile keeps read-only: no agent writes or plants them.
/// `objects` is granted entry by entry instead, without its `info` folder.
const GIT_FOLLOWED: &[&str] = &[
    "commondir",
    "config.worktree",
    "info",
    "objects",
    "shallow",
    "worktrees",
];

const CONFINED_UNAVAILABLE: &str = "an agent confined to its own worktree cannot run on Linux yet: \
     neither Landlock nor bubblewrap can keep the worktree's git links and the existing git \
     objects read-only inside the git folders it must write";

const LANDLOCK_DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
    "/dev/shm",
];

/// The rules the Landlock helper applies to itself before it starts the harness: read and
/// execute beneath `read`, full access beneath `write`, and, when `limit_network` is set,
/// outbound TCP only to the `connect` ports, no TCP bind and no socket but TCP, unless
/// `open_sockets` keeps the other socket kinds open ([`LinuxSockets::TcpPorts`]). A listen
/// with no bind still takes a port Linux picks, on either network, so both name
/// `keep_free`: the helper starts no agent while Linux could hand one of them out by
/// itself, and an open-network agent binds any TCP port but those. `die_with` names the
/// process the helper and the program it becomes are killed with (an engine git step).
/// `nested` tells the helper it runs inside bubblewrap's namespaces ([`NEST_ARGS`]).
/// `guard` names what the helper's call supervisor holds ([`Guard`]). The
/// engine renders it as helper arguments and the helper parses it back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LandlockPolicy {
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
    pub connect: Vec<u16>,
    pub limit_network: bool,
    pub open_sockets: bool,
    pub keep_free: Vec<u16>,
    pub die_with: Option<u32>,
    pub nested: bool,
    pub guard: Guard,
}

/// What the helper's call supervisor holds for an agent on a host with no namespaces to
/// nest it in (DEC-179): `loopback` names the connect ports it reaches on this machine
/// only, `git` its own git folders, the only `.git` entries it may make, and `signals`
/// keeps its signals inside the programs it started. With none of them the helper becomes
/// the harness; with any it stays as the harness's parent and answers its guarded calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Guard {
    pub loopback: Vec<u16>,
    pub git: Option<GitOwn>,
    pub signals: bool,
}

impl Guard {
    /// The helper arguments for this guard, in the order the helper reads them back.
    fn args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::new();
        for port in &self.loopback {
            args.push(GUARD_LOOPBACK.into());
            args.push(port.to_string().into());
        }
        if let Some(git) = &self.git {
            args.push(GUARD_GIT.into());
            let own = [
                (OWN_TREE, &git.trees),
                (OWN_KIDS, &git.kids),
                (OWN_GIT, &git.exact),
            ];
            for (flag, paths) in own {
                for path in paths {
                    args.push(flag.into());
                    args.push(path.into());
                }
            }
        }
        if self.signals {
            args.push(GUARD_SIGNALS.into());
        }
        args
    }

    /// What the supervisor holds for this guard. Input: the policy's connect ports.
    /// Output: the guarded ports reached on this machine only and the others anywhere.
    fn supervised(&self, connect: &[u16]) -> Supervised {
        assert!(self.loopback.iter().all(|port| connect.contains(port)));
        let any = connect
            .iter()
            .copied()
            .filter(|port| !self.loopback.contains(port))
            .collect();
        let reach = Reach {
            any,
            loopback: self.loopback.clone(),
        };
        Supervised {
            reach: Some(reach).filter(|_| !self.loopback.is_empty()),
            git: self.git.clone(),
            signals: self.signals,
        }
    }
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
        if self.open_sockets {
            args.push(OPEN_SOCKETS.into());
        }
        for port in &self.connect {
            args.push(CONNECT.into());
            args.push(port.to_string().into());
        }
        for port in &self.keep_free {
            args.push(KEEP_FREE.into());
            args.push(port.to_string().into());
        }
        if let Some(parent) = self.die_with {
            args.push(DIE_WITH.into());
            args.push(parent.to_string().into());
        }
        if self.nested {
            args.push(NESTED.into());
        }
        args.extend(self.guard.args());
        args
    }
}

/// The Landlock rules of one engine git step on an agent's linked worktree, the Linux form
/// of the macOS step profile (SECAUDIT unconfined-git-add-reads-through-raced-symlink,
/// unconfined-read-tree-u): it reads the system, `/proc` and `read`, and writes the devices
/// and `write`, with no network rule and no kept port, and dies with this engine process, so
/// its git never outlives the lock the engine took for it (RV-DS241-142); an engine that is
/// process 1 takes every process down with it. Inputs: the absolute places the step reads
/// and writes. Output: the policy.
pub(crate) fn git_step_policy(read: &[PathBuf], write: &[PathBuf]) -> LandlockPolicy {
    assert!(read.iter().chain(write).all(|path| path.is_absolute()));
    let policy = LandlockPolicy {
        read: SYSTEM_READ_ROOTS
            .iter()
            .chain(&["/proc"])
            .map(PathBuf::from)
            .chain(read.iter().cloned())
            .collect(),
        write: LANDLOCK_DEVICES
            .iter()
            .map(PathBuf::from)
            .chain(write.iter().cloned())
            .collect(),
        die_with: Some(std::process::id()).filter(|engine| *engine > 1),
        ..LandlockPolicy::default()
    };
    assert!(policy.read.len() + policy.write.len() <= HELPER_PATHS_MAX);
    assert!(!policy.limit_network && policy.keep_free.is_empty());
    policy
}

/// Tells whether this kernel holds the helper's file rules, as a landing git step needs
/// them: the helper's probe with no network flag. Input: the helper. Output: true only when
/// it is an absolute file and its probe exits 0.
pub(crate) fn holds_file_rules(helper: &Path) -> bool {
    clean_absolute(helper)
        && helper.is_file()
        && crate::spawn_gate::status(
            Command::new(helper)
                .arg(PROBE)
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .is_ok_and(|status| status.success())
}

/// What the `crazyagent-landlock` helper was asked to do: probe whether this kernel holds a
/// Landlock ruleset, apply a policy and start a harness, inside bubblewrap install only
/// the socket filter and start a harness, start bubblewrap from a fixed place with
/// [`NEST_ARGS`], which then starts the helper again inside its namespaces, tell whether
/// the kernel scopes signals, carry a nested agent's bridged ports as its broker, build a
/// routed agent's network as its setup child, tell whether this machine runs the call
/// supervisor, or be the check that question starts.
#[derive(Debug, PartialEq, Eq)]
pub enum HelperRequest {
    Probe {
        limit_network: bool,
        open_sockets: bool,
        keep_free: bool,
    },
    Run {
        policy: LandlockPolicy,
        program: PathBuf,
        args: Vec<OsString>,
    },
    Filter {
        program: PathBuf,
        args: Vec<OsString>,
    },
    Nest {
        bwrap: PathBuf,
        args: Vec<OsString>,
    },
    Scoped,
    Broker {
        parent: u32,
        ports: Vec<u16>,
    },
    Egress {
        parent: u32,
        ports: Vec<u16>,
    },
    Supervisor {
        folder: PathBuf,
    },
    Supervised {
        port: u16,
        folder: PathBuf,
    },
}

impl HelperRequest {
    /// Parses the helper arguments after its own name. Input: flags from
    /// [`LandlockPolicy::args`] then `--`, an absolute program and its arguments, or
    /// `--probe` with an optional `--limit-network`, `--open-sockets` and `--keep-free`
    /// with a port, or `--filter-sockets` alone then `--` and the harness, or `--nest`
    /// alone then `--` and what [`nest_request`] takes, or `--scoped` with nothing else, or
    /// `--broker` with what [`bridge::broker_request`] takes, or `--egress` with what
    /// [`egress::request`] takes.
    /// `--open-sockets` needs `--limit-network`. Output: the request, or the reason the
    /// arguments are refused.
    pub fn parse(args: Vec<OsString>) -> Result<Self, String> {
        if args.first().is_some_and(|flag| flag == SCOPED) {
            return (args.len() == 1)
                .then_some(Self::Scoped)
                .ok_or_else(|| format!("{SCOPED} takes no other argument"));
        }
        if args.first().is_some_and(|flag| flag == bridge::BROKER) {
            return bridge::broker_request(args);
        }
        if args.first().is_some_and(|flag| flag == egress::EGRESS) {
            return egress::request(args);
        }
        if let Some(probe) = supervisor_probe(&args) {
            return probe;
        }
        Self::flagged(args)
    }

    /// Parses every request but the scope question, as [`HelperRequest::parse`] describes
    /// them. Input: the helper arguments. Output: the request, or the reason.
    fn flagged(args: Vec<OsString>) -> Result<Self, String> {
        let mut policy = LandlockPolicy::default();
        let mut probe = false;
        let mut filter = false;
        let mut nest = false;
        let mut rest = args.into_iter();
        while let Some(flag) = rest.next() {
            if guard_flag(&mut policy.guard, &flag, &mut rest)? {
                continue;
            }
            match flag.to_str() {
                Some(PROBE) => probe = true,
                Some(FILTER_SOCKETS) => filter = true,
                Some(NEST) => nest = true,
                Some(NESTED) => policy.nested = true,
                Some(LIMIT_NETWORK) => policy.limit_network = true,
                Some(OPEN_SOCKETS) => policy.open_sockets = true,
                Some(READ) => policy.read.push(absolute(rest.next())?),
                Some(WRITE) => policy.write.push(absolute(rest.next())?),
                Some(CONNECT) => policy.connect.push(port(rest.next())?),
                Some(KEEP_FREE) => policy.keep_free.push(port(rest.next())?),
                Some(DIE_WITH) if policy.die_with.is_none() => {
                    policy.die_with = Some(parent(rest.next())?);
                }
                Some("--") if nest => {
                    let alone = !probe && !filter && policy == LandlockPolicy::default();
                    return nest_request(alone, rest);
                }
                Some("--") if !probe && filter => {
                    if policy != LandlockPolicy::default() {
                        return Err(format!("{FILTER_SOCKETS} takes no policy"));
                    }
                    let program = absolute(rest.next())?;
                    return Ok(Self::Filter {
                        program,
                        args: rest.collect(),
                    });
                }
                Some("--") if !probe => return run_request(policy, rest),
                _ => return Err(format!("unexpected argument {}", flag.to_string_lossy())),
            }
        }
        let bare = policy.read.is_empty()
            && policy.write.is_empty()
            && policy.connect.is_empty()
            && policy.die_with.is_none()
            && !policy.nested
            && policy.guard == Guard::default();
        if probe && bare && !filter && !nest {
            open_needs_limit(&policy)?;
            binds_fit(&policy)?;
            return Ok(Self::Probe {
                limit_network: policy.limit_network,
                open_sockets: policy.open_sockets,
                keep_free: !policy.keep_free.is_empty(),
            });
        }
        Err("missing the harness command after --".into())
    }
}

/// Reads what follows a policy's `--`: the harness and its arguments, once the policy
/// holds together. Inputs: the parsed policy and the arguments after `--`. Output: the
/// run, or the reason the policy or the harness is refused.
fn run_request(
    policy: LandlockPolicy,
    mut rest: impl Iterator<Item = OsString>,
) -> Result<HelperRequest, String> {
    open_needs_limit(&policy)?;
    binds_fit(&policy)?;
    guard_fits(&policy)?;
    if policy.nested && policy.die_with.is_some() {
        return Err(format!("{NESTED} takes no {DIE_WITH}"));
    }
    let program = absolute(rest.next())?;
    if policy.read.len() + policy.write.len() > HELPER_PATHS_MAX {
        return Err("too many path grants".into());
    }
    Ok(HelperRequest::Run {
        policy,
        program,
        args: rest.collect(),
    })
}

/// Reads the call supervisor's two probe requests, which stand alone: the question, with
/// the private folder the engine made for it, and the check the helper then starts as its
/// own harness, with the guarded port and that folder. Input: the helper arguments.
/// Output: the request or the reason it is refused, or none when the first argument is
/// neither.
fn supervisor_probe(args: &[OsString]) -> Option<Result<HelperRequest, String>> {
    let checked = |at: &OsString, folder: &OsString| -> Result<HelperRequest, String> {
        let port = port(Some(at.clone()))?;
        let folder = absolute(Some(folder.clone()))?;
        Ok(HelperRequest::Supervised { port, folder })
    };
    match args {
        [first, folder] if first == PROBE_SUPERVISOR => {
            let folder = absolute(Some(folder.clone()));
            Some(folder.map(|folder| HelperRequest::Supervisor { folder }))
        }
        [first, ..] if first == PROBE_SUPERVISOR => {
            Some(Err(format!("{PROBE_SUPERVISOR} takes a folder")))
        }
        [first, at, folder] if first == PROBE_SUPERVISED => Some(checked(at, folder)),
        [first, ..] if first == PROBE_SUPERVISED => {
            Some(Err(format!("{PROBE_SUPERVISED} takes a port and a folder")))
        }
        _ => None,
    }
}

/// Reads one call-supervisor flag into a guard. Inputs: the guard, the flag and the
/// arguments after it. Output: whether the flag was one of them, or the reason its value
/// is refused; an own git place is one only after `--guard-git`, which comes once.
fn guard_flag(
    guard: &mut Guard,
    flag: &OsStr,
    rest: &mut impl Iterator<Item = OsString>,
) -> Result<bool, String> {
    let place = match flag.to_str() {
        Some(GUARD_LOOPBACK) => {
            guard.loopback.push(port(rest.next())?);
            return Ok(true);
        }
        Some(GUARD_SIGNALS) => {
            guard.signals = true;
            return Ok(true);
        }
        Some(GUARD_GIT) if guard.git.is_none() => {
            guard.git = Some(GitOwn::default());
            return Ok(true);
        }
        Some(OWN_TREE) => guard.git.as_mut().map(|git| &mut git.trees),
        Some(OWN_KIDS) => guard.git.as_mut().map(|git| &mut git.kids),
        Some(OWN_GIT) => guard.git.as_mut().map(|git| &mut git.exact),
        _ => return Ok(false),
    };
    let place = place.ok_or_else(|| format!("an own git place needs {GUARD_GIT} before it"))?;
    place.push(absolute(rest.next())?);
    Ok(true)
}

/// Refuses a guard that does not fit its policy. A guarded port is one the agent may
/// connect to, under the network limit with TCP sockets alone, where every connect is one
/// the supervisor can judge; no guard goes with `--die-with`; the own git places are
/// bounded. Input: the parsed policy. Output: Ok, or the reason.
fn guard_fits(policy: &LandlockPolicy) -> Result<(), String> {
    let guard = &policy.guard;
    if *guard == Guard::default() {
        return Ok(());
    }
    if policy.die_with.is_some() {
        return Err(format!("{DIE_WITH} takes no call supervisor"));
    }
    let tcp_only = policy.limit_network && !policy.open_sockets;
    if !guard.loopback.is_empty() && !tcp_only {
        return Err(format!("{GUARD_LOOPBACK} needs {LIMIT_NETWORK} without {OPEN_SOCKETS}"));
    }
    if !guard.loopback.iter().all(|port| policy.connect.contains(port)) {
        return Err(format!("{GUARD_LOOPBACK} names a port with no {CONNECT}"));
    }
    let own = |git: &GitOwn| git.trees.len() + git.kids.len() + git.exact.len();
    if guard.git.as_ref().map_or(0, own) > GRANTS_MAX {
        return Err("too many own git places".into());
    }
    Ok(())
}

/// Reads what follows `--nest --`: bubblewrap at one of its fixed places, then exactly
/// [`NEST_ARGS`], then at least the command bubblewrap starts, so the helper starts nothing
/// else on a caller's word. Inputs: whether `--nest` was the only flag, and those
/// arguments. Output: the request, or the reason they are refused.
fn nest_request(
    alone: bool,
    mut rest: impl Iterator<Item = OsString>,
) -> Result<HelperRequest, String> {
    if !alone {
        return Err(format!("{NEST} takes no other flag"));
    }
    let bwrap = absolute(rest.next())?;
    if !BWRAP_PATHS.iter().any(|path| Path::new(path) == bwrap) {
        return Err(format!("{NEST} starts only bubblewrap from its fixed places"));
    }
    let args: Vec<OsString> = rest.collect();
    let fixed = args
        .iter()
        .zip(NEST_ARGS)
        .all(|(arg, fixed)| arg.as_os_str() == OsStr::new(fixed));
    if !fixed || args.len() <= NEST_ARGS.len() {
        return Err(format!("{NEST} needs bubblewrap's fixed arguments and a command"));
    }
    Ok(HelperRequest::Nest { bwrap, args })
}

fn open_needs_limit(policy: &LandlockPolicy) -> Result<(), String> {
    if policy.open_sockets && !policy.limit_network {
        return Err(format!("{OPEN_SOCKETS} needs {LIMIT_NETWORK}"));
    }
    Ok(())
}

/// Refuses more kept ports than [`KEEP_FREE_MAX`]. Input: the parsed policy. Output: Ok,
/// or the reason.
fn binds_fit(policy: &LandlockPolicy) -> Result<(), String> {
    if policy.keep_free.len() > KEEP_FREE_MAX {
        return Err("too many kept ports".into());
    }
    Ok(())
}

fn absolute(value: Option<OsString>) -> Result<PathBuf, String> {
    let path = PathBuf::from(value.ok_or("missing path")?);
    if !path.is_absolute() {
        return Err(format!("path must be absolute: {}", path.display()));
    }
    Ok(path)
}

/// Reads the process id `--die-with` names: a number above 1, since process 1 is never an
/// engine. Input: the argument. Output: the id, or the reason it is refused.
fn parent(value: Option<OsString>) -> Result<u32, String> {
    value
        .as_deref()
        .and_then(OsStr::to_str)
        .and_then(|text| text.parse::<u32>().ok())
        .filter(|parent| *parent > 1 && i32::try_from(*parent).is_ok())
        .ok_or_else(|| format!("{DIE_WITH} needs a process id above 1"))
}

fn port(value: Option<OsString>) -> Result<u16, String> {
    value
        .as_deref()
        .and_then(OsStr::to_str)
        .and_then(|text| text.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or_else(|| "port must be 1 to 65535".to_string())
}

/// The ports Linux hands out by itself, as `net.ipv4.ip_local_port_range` gives them; IPv6
/// follows the same setting. Input: the file's text. Output: the lowest and the highest
/// port, or none for text that is not two port numbers in rising order.
fn picked_ports(text: &str) -> Option<RangeInclusive<u16>> {
    let mut numbers = text.split_ascii_whitespace().map(str::parse::<u16>);
    let low = numbers.next()?.ok()?;
    let high = numbers.next()?.ok()?;
    (numbers.next().is_none() && low <= high).then_some(low..=high)
}

/// Reads [`picked_ports`] for the network this process is in. No input. Output: the range,
/// or none when the setting cannot be read.
fn read_picked_ports() -> Option<RangeInclusive<u16>> {
    let mut text = String::new();
    fs::File::open(PICKED_PORTS_FILE)
        .ok()?
        .take(PICKED_PORTS_BYTES_MAX)
        .read_to_string(&mut text)
        .ok()?;
    picked_ports(&text)
}

/// Refuses kept ports Linux could hand to a Landlock agent by itself. Landlock leaves the
/// agent in the host's network and checks only the port a bind names: a bind to port 0 on
/// an open network, or a listen on a socket that was never bound on any network, gets a
/// port Linux picks from its range with no check, and no rule can refuse a listen. So every
/// kept port must lie outside that range, or no Landlock agent starts. Inputs: the kept
/// ports and the range, none when it cannot be read. Output: Ok, or the plain reason
/// naming the first kept port inside the range.
fn kept_outside_picked(kept: &[u16], picked: Option<&RangeInclusive<u16>>) -> Result<(), String> {
    let Some(picked) = picked else {
        return Err(PICKED_PORTS_UNREADABLE.into());
    };
    let Some(port) = kept.iter().find(|port| picked.contains(*port)) else {
        return Ok(());
    };
    let (low, high) = (picked.start(), picked.end());
    Err(format!(
        "port {port} is kept free for the owner's apps, a local model or a model proxy, and this \
         Linux hands out ports {low} to {high} by itself, so an agent could end up listening \
         on it; move the local model to a port below {low}, or have an \
         administrator move net.ipv4.ip_local_port_range away from port {port}"
    ))
}

/// The TCP ports an open-network agent may bind. Landlock has no rule that refuses a port,
/// so the helper allows every other one, port 0 among them, which asks Linux for a free
/// port: [`kept_outside_picked`] holds that such a port is never a kept one. Input: the
/// kept ports, none of them 0. Output: every port that is not kept, in rising order.
fn bindable(keep_free: &[u16]) -> Vec<u16> {
    assert!(!keep_free.contains(&0));
    let mut kept = vec![false; usize::from(u16::MAX) + 1];
    for port in keep_free {
        kept[usize::from(*port)] = true;
    }
    let ports: Vec<u16> = (0..=u16::MAX)
        .filter(|port| !kept[usize::from(*port)])
        .collect();
    assert!(ports.iter().all(|port| !keep_free.contains(port)));
    assert!(ports.len() + keep_free.len() > usize::from(u16::MAX));
    ports
}

/// Applies the policy's Landlock rules to this process. A limited network handles every
/// network right and allows only its `connect` ports. An open network with kept ports
/// handles TCP bind alone and, with `rules`, allows every [`bindable`] port; without `rules`,
/// as in a probe, it only proves this kernel handles the right. Inputs: the policy, and
/// whether to add the bind rules. Output: what the kernel enforced, or the ruleset error.
fn enforce(policy: &LandlockPolicy, rules: bool) -> Result<RestrictionStatus, RulesetError> {
    let keeps = !policy.limit_network && !policy.keep_free.is_empty();
    let mut ruleset = Ruleset::default().handle_access(AccessFs::from_all(FS_ABI))?;
    if policy.limit_network {
        ruleset = ruleset.handle_access(AccessNet::from_all(NET_ABI))?;
    } else if keeps {
        ruleset = ruleset.handle_access(AccessNet::BindTcp)?;
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
    if keeps && rules {
        for port in bindable(&policy.keep_free) {
            created = created.add_rule(NetPort::new(port, AccessNet::BindTcp))?;
        }
    }
    created.restrict_self()
}

/// Adds a second Landlock layer that keeps signals and abstract Unix sockets inside the
/// agent's domain, so the agent cannot signal the engine, its tmux server or other agents.
/// Linux before 6.12 has no scopes: there the layer holds nothing and the first layer's
/// boundary stands alone. No input. Output: the status, or the ruleset error.
fn scope_signals() -> Result<RestrictionStatus, RulesetError> {
    Ruleset::default()
        .scope(Scope::from_all(SCOPE_ABI))?
        .create()?
        .restrict_self()
}

/// Installs the seccomp filter every agent process and its children keep: a new Unix-domain
/// socket and a new io_uring ring both fail with `EACCES`, and with `tcp_only` so does every
/// socket but TCP over IPv4 or IPv6 and routing netlink, since Landlock limits TCP only and
/// a datagram, raw or other family socket reaches any host. Landlock does not mediate a
/// pathname socket connect, and a ring could create a socket without `socket(2)`;
/// `socketpair(2)` stays allowed since it reaches no path. With `plain_tcp` the filter also
/// refuses the stream sockets that hold a TCP port without being plain TCP, which
/// Landlock's bind rules do not see on every kernel. Inputs: whether only TCP sockets may open, and whether
/// Landlock keeps TCP ports free of this agent. Output: the installed filter, or the reason
/// this kernel or architecture cannot hold it.
fn refuse_sockets(tcp_only: bool, plain_tcp: bool) -> Result<(), String> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    rules.insert(libc::SYS_socket, refused_sockets(tcp_only, plain_tcp)?);
    rules.insert(libc::SYS_io_uring_setup, Vec::new());
    #[cfg(target_arch = "x86_64")]
    {
        rules.insert(
            X32_SYSCALL_BIT | libc::SYS_socket,
            refused_sockets(tcp_only, plain_tcp)?,
        );
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

/// The `socket(2)` calls the filter refuses, any one rule refusing a call: a Unix-domain
/// socket, and with `tcp_only` a family other than IPv4, IPv6 and netlink, a netlink socket
/// other than routing (glibc's `getaddrinfo` reads interface addresses through it), an IPv4
/// or IPv6 socket of a type other than stream, and one naming a protocol other than TCP.
/// With `plain_tcp` and without `tcp_only`, which covers them already: an SMC socket, and an
/// IPv4 or IPv6 stream socket naming a protocol other than TCP, such as Multipath TCP or
/// SMC, whose listeners take plain TCP clients on a port no bind rule checked. Inputs: whether only TCP sockets may open, and whether Landlock keeps TCP ports
/// free of this agent. Output: the rules, or the reason seccomp cannot express one.
fn refused_sockets(tcp_only: bool, plain_tcp: bool) -> Result<Vec<SeccompRule>, String> {
    let seccomp = |error: seccompiler::BackendError| format!("seccomp: {error}");
    let rule = |conditions: &[(u8, SeccompCmpOp, u64)]| {
        let conditions = conditions
            .iter()
            .map(|(arg, op, value)| {
                SeccompCondition::new(*arg, SeccompCmpArgLen::Dword, op.clone(), *value)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(seccomp)?;
        SeccompRule::new(conditions).map_err(seccomp)
    };
    let mut rules = vec![rule(&[(0, SeccompCmpOp::Eq, libc::AF_UNIX as u64)])?];
    if tcp_only {
        rules.push(rule(&[
            (0, SeccompCmpOp::Ne, libc::AF_INET as u64),
            (0, SeccompCmpOp::Ne, libc::AF_INET6 as u64),
            (0, SeccompCmpOp::Ne, libc::AF_NETLINK as u64),
        ])?);
        rules.push(rule(&[
            (0, SeccompCmpOp::Eq, libc::AF_NETLINK as u64),
            (2, SeccompCmpOp::Ne, libc::NETLINK_ROUTE as u64),
        ])?);
        for family in [libc::AF_INET as u64, libc::AF_INET6 as u64] {
            for kind in (0..=SOCKET_TYPE_MASK).filter(|kind| *kind != libc::SOCK_STREAM as u64) {
                rules.push(rule(&[
                    (0, SeccompCmpOp::Eq, family),
                    (1, SeccompCmpOp::MaskedEq(SOCKET_TYPE_MASK), kind),
                ])?);
            }
            rules.push(rule(&[
                (0, SeccompCmpOp::Eq, family),
                (2, SeccompCmpOp::Ne, 0),
                (2, SeccompCmpOp::Ne, libc::IPPROTO_TCP as u64),
            ])?);
        }
    } else if plain_tcp {
        rules.push(rule(&[(0, SeccompCmpOp::Eq, AF_SMC)])?);
        for family in [libc::AF_INET as u64, libc::AF_INET6 as u64] {
            rules.push(rule(&[
                (0, SeccompCmpOp::Eq, family),
                (
                    1,
                    SeccompCmpOp::MaskedEq(SOCKET_TYPE_MASK),
                    libc::SOCK_STREAM as u64,
                ),
                (2, SeccompCmpOp::Ne, 0),
                (2, SeccompCmpOp::Ne, libc::IPPROTO_TCP as u64),
            ])?);
        }
    }
    assert!(!rules.is_empty());
    Ok(rules)
}

/// Tells whether the ruleset the helper applied holds the boundary. Inputs: the status
/// `restrict_self` reported and whether no-new-privileges is set. Output: true only when
/// every requested right is enforced. A partly enforced ruleset leaves rights such as
/// truncate (Linux before 6.2) unrestricted, so that kernel falls back to bubblewrap.
fn accepted(ruleset: &RulesetStatus, no_new_privs: bool) -> bool {
    matches!(ruleset, RulesetStatus::FullyEnforced) && no_new_privs
}

/// Makes the helper lead a session of its own before it starts the harness, so the agent
/// has no controlling terminal: it cannot push input into the engine's terminal (TIOCSTI)
/// or get that terminal's signals. A helper already leading its session, as the first
/// process of a terminal pane, keeps it. A process group leader, which `setsid` refuses,
/// first rejoins its parent's group. Output: Ok once session, group and pid are equal.
fn own_session() -> Result<(), String> {
    let helper = rustix::process::getpid();
    if rustix::process::getsid(None) == Ok(helper) {
        return Ok(());
    }
    let session = |error: rustix::io::Errno| format!("session: {error}");
    if rustix::process::getpgrp() == helper {
        let parent = rustix::process::getppid().ok_or("session: the engine has exited")?;
        let group = rustix::process::getpgid(Some(parent)).map_err(session)?;
        rustix::process::setpgid(None, Some(group)).map_err(session)?;
    }
    rustix::process::setsid().map_err(session)?;
    assert_eq!(rustix::process::getsid(None), Ok(helper));
    assert_eq!(rustix::process::getpgrp(), helper);
    Ok(())
}

/// Makes the kernel kill this helper, and the program it then becomes, once the thread of
/// `parent` that started it exits, and refuses when `parent` is already not its parent, as
/// after that process exited before the signal was set. The signal survives the exec of a
/// program that is not set-user-ID. Input: the parent's process id. Output: Ok, or the reason.
fn die_with_parent(parent: u32) -> Result<(), String> {
    rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))
        .map_err(|error| format!("parent death signal: {error}"))?;
    let now = rustix::process::Pid::as_raw(rustix::process::getppid());
    if u32::try_from(now).ok() != Some(parent) {
        return Err("the engine has exited".into());
    }
    Ok(())
}

/// Entry point of the `crazyagent-landlock` helper. Input: its arguments after its own name.
/// A probe applies a ruleset with no rule, the scope layer and the socket filter to itself and
/// exits 0 when this kernel holds them; a run applies the policy, the scope layer and the
/// filter to itself, moves into a session of its own and then replaces itself with the
/// harness, or with a guard stays as its parent and ends with its exit code ([`supervise`]);
/// a nest becomes bubblewrap, which starts a run inside its namespaces ([`nest`]).
/// Route and bridge flags may lead a nest, and bridge flags the run or probe inside it
/// ([`read_request`]). No mode runs while the environment holds a variable that loads code
/// ([`LOADER_REFUSED`]).
/// Output: the exit code, 2 for refused arguments or such a variable, 3 when the kernel
/// cannot hold the policy, the session or the signal mask, or the process it dies with is
/// gone, 126 when the harness or bubblewrap cannot start.
pub fn helper_main(args: Vec<OsString>) -> ExitCode {
    let loader = std::env::vars_os()
        .map(|(name, _)| name)
        .find(|name| loader_variable(name));
    if let Some(name) = loader {
        eprintln!(
            "crazyagent-landlock: {LOADER_REFUSED}: {}",
            name.to_string_lossy()
        );
        return ExitCode::from(2);
    }
    let (_routes, bridge, request) = match read_request(args) {
        Ok(asked) => asked,
        Err(reason) => {
            eprintln!("crazyagent-landlock: {reason}");
            return ExitCode::from(2);
        }
    };
    let (policy, launch) = match request {
        HelperRequest::Filter { program, args } => return filtered(&program, args),
        HelperRequest::Nest { bwrap, args } => return nest(&bwrap, &args, bridge.as_ref()),
        HelperRequest::Scoped => return scoped(),
        HelperRequest::Broker { parent, ports } => return bridge::broker(parent, &ports),
        HelperRequest::Egress { parent, ports } => return egress::setup(parent, &ports),
        HelperRequest::Supervisor { folder } => return probe_supervisor(&folder),
        HelperRequest::Supervised { port, folder } => return probe_supervised(port, &folder),
        HelperRequest::Probe {
            limit_network,
            open_sockets,
            keep_free,
        } => (
            LandlockPolicy {
                limit_network,
                open_sockets,
                keep_free: OWNER_PORTS[..usize::from(keep_free)].to_vec(),
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
    hold(&policy, launch, bridge.as_ref())
}

/// Reads what the helper was asked: the route flags first ([`egress::leading`]), then the
/// bridge flags ([`Bridge::leading`]), then the request, each set of flags only where the
/// request takes it ([`Bridge::fits`], [`egress::fits`]). Input: the helper arguments.
/// Output: the route ports, empty for none, the bridge and the request; or the reason.
fn read_request(args: Vec<OsString>) -> Result<(Vec<u16>, Option<Bridge>, HelperRequest), String> {
    let (routes, args) = egress::leading(args)?;
    let (bridge, args) = Bridge::leading(args)?;
    let request = HelperRequest::parse(args)?;
    Bridge::fits(bridge.as_ref(), &request)?;
    egress::fits(&routes, &request)?;
    assert!(routes.len() <= REMOTES_MAX);
    Ok((routes, bridge, request))
}

/// Holds a policy on this process, then becomes the harness, or with a guard stays as its
/// parent ([`supervise`]); for a probe it only tells
/// whether this kernel held it. Under a bridge it first hands the bridged listeners to the
/// broker, before any rule; the ports it would keep free are then ports of a network of
/// its own, so the picked range is not asked. Inputs: the policy, the harness if any, and
/// the bridge. Output: the exit code as [`helper_main`] gives it.
fn hold(
    policy: &LandlockPolicy,
    launch: Option<(PathBuf, Vec<OsString>)>,
    bridge: Option<&Bridge>,
) -> ExitCode {
    if let Some(parent) = policy.die_with
        && let Err(reason) = die_with_parent(parent)
    {
        eprintln!("crazyagent-landlock: {reason}");
        return ExitCode::from(3);
    }
    let readied = match bridge {
        Some(bridge) => bridge.hand_over(),
        None if policy.keep_free.is_empty() => Ok(()),
        None => kept_outside_picked(&policy.keep_free, read_picked_ports().as_ref()),
    };
    if let Err(reason) = readied {
        eprintln!("crazyagent-landlock: {reason}");
        return ExitCode::from(3);
    }
    match enforce(policy, launch.is_some()) {
        Ok(status) if accepted(&status.ruleset, status.no_new_privs) => {}
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
    if let Err(error) = scope_signals() {
        eprintln!("crazyagent-landlock: {error}");
        return ExitCode::from(3);
    }
    let plain_tcp = policy.limit_network || !policy.keep_free.is_empty();
    if let Err(reason) = refuse_sockets(policy.limit_network && !policy.open_sockets, plain_tcp) {
        eprintln!("crazyagent-landlock: {reason}");
        return ExitCode::from(3);
    }
    let Some((program, args)) = launch else {
        return ExitCode::SUCCESS;
    };
    if let Err(reason) = placed(policy.nested) {
        eprintln!("crazyagent-landlock: {reason}");
        return ExitCode::from(3);
    }
    if policy.guard != Guard::default() {
        return supervise(policy, &program, args);
    }
    let error = Command::new(&program).args(args).exec();
    eprintln!(
        "crazyagent-landlock: cannot start {}: {error}",
        program.display()
    );
    ExitCode::from(126)
}

/// Starts the harness under the call supervisor and stays as its parent, answering its
/// guarded calls until it ends. The engine's stop signals reach the harness, which shares
/// this process's group, and are held off here, so the helper ends with the harness.
/// Inputs: the policy with its guard and the harness. Output: the harness's exit code, 128
/// plus the signal that ended it, 126 when it cannot start under the supervisor.
fn supervise(policy: &LandlockPolicy, program: &Path, args: Vec<OsString>) -> ExitCode {
    assert!(policy.guard != Guard::default());
    let mut harness = Command::new(program);
    harness.args(args);
    match serve::run(policy.guard.supervised(&policy.connect), &mut harness) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!(
                "crazyagent-landlock: cannot start {} under the call supervisor: {error}",
                program.display()
            );
            ExitCode::from(126)
        }
    }
}

/// Answers through the exit code whether this machine runs the call supervisor: the
/// helper starts itself as a harness under every guard, with a port only this machine
/// reaches and the empty private folder the engine made ([`private_empty`]), and that
/// harness checks each guard ([`probe_supervised`]). Input: the folder. Output: 0 when
/// every guard held, 1 otherwise or when the folder is not private.
fn probe_supervisor(folder: &Path) -> ExitCode {
    assert!(folder.is_absolute());
    if nix::sys::prctl::set_no_new_privs().is_err() || !private_empty(folder) {
        return ExitCode::from(1);
    }
    let checked = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).and_then(|listener| {
        let port = listener.local_addr()?.port();
        let mut check = Command::new("/proc/self/exe");
        check
            .arg(PROBE_SUPERVISED)
            .arg(port.to_string())
            .arg(folder);
        let all = Guard {
            loopback: vec![port],
            git: Some(GitOwn::default()),
            signals: true,
        };
        serve::run(all.supervised(&[port]), &mut check)
    });
    match checked {
        Ok(0) => ExitCode::SUCCESS,
        _ => ExitCode::from(1),
    }
}

/// Tells whether a folder is the private probe folder the engine made ([`private_folder`]):
/// a real folder, not a link, that this user owns with mode 0700 and that holds nothing
/// (rv-secreview linux-t4c F1). Input: the absolute folder. Output: true only then.
fn private_empty(folder: &Path) -> bool {
    assert!(folder.is_absolute());
    let Ok(held) = fs::symlink_metadata(folder) else {
        return false;
    };
    let own = held.uid() == rustix::process::geteuid().as_raw();
    let private = held.is_dir() && own && held.mode() & 0o7777 == 0o700;
    private && fs::read_dir(folder).is_ok_and(|mut entries| entries.next().is_none())
}

/// The call supervisor's check, run as its harness: the guarded port is reached on this
/// machine and refused on another address, a new file is made and written, a `.git` is
/// refused, and so are a signal to the supervisor, an open of its memory for writing and a
/// take of its descriptor (rv-sec3 linux-t4b F1). Inputs: the guarded port and an empty
/// folder. Output: 0 when all seven hold, else 1.
fn probe_supervised(port: u16, folder: &Path) -> ExitCode {
    let refused = |error: std::io::Error| error.kind() == std::io::ErrorKind::PermissionDenied;
    let elsewhere = SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), port));
    let made = folder.join("made");
    let here = TcpStream::connect((Ipv4Addr::LOCALHOST, port));
    let there = TcpStream::connect_timeout(&elsewhere, Duration::from_secs(2));
    let written = fs::write(&made, b"made").and_then(|()| fs::read(&made));
    let git = fs::create_dir(folder.join(".git"));
    let supervisor = rustix::process::getppid();
    let signal = supervisor.map(rustix::process::test_kill_process);
    let memory = supervisor.map(|pid| {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("/proc/{}/mem", pid.as_raw_nonzero()))
    });
    let taken = supervisor.map(|pid| {
        let handle = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())?;
        rustix::process::pidfd_getfd(&handle, 0, rustix::process::PidfdGetfdFlags::empty())
    });
    let held = [
        here.is_ok(),
        there.is_err_and(refused),
        written.is_ok_and(|read| read == b"made"),
        git.is_err_and(refused),
        signal == Some(Err(rustix::io::Errno::PERM)),
        memory.is_some_and(|opened| opened.is_err_and(refused)),
        taken.is_some_and(|took| took.err() == Some(rustix::io::Errno::PERM)),
    ];
    if held == [true; 7] {
        return ExitCode::SUCCESS;
    }
    ExitCode::from(1)
}

/// Answers the scope question through the exit code: the scope layer applied to this
/// process alone. Output: 0 when the kernel keeps an agent's signals inside its own
/// Landlock domain (Linux 6.12 or newer), 1 when the layer holds nothing or only in part.
fn scoped() -> ExitCode {
    match scope_signals() {
        Ok(status) if accepted(&status.ruleset, status.no_new_privs) => ExitCode::SUCCESS,
        _ => ExitCode::from(1),
    }
}

/// Starts bubblewrap for a nested agent, from outside its namespaces, with an environment
/// [`helper_main`] has already cleared of loader variables. It opens bubblewrap
/// once ([`trusted_file`]) and starts that open file, so the file it checked is the file
/// that runs. It takes the session as [`own_session`] does, where a pane's first process
/// can still be told apart, then blocks every signal: bubblewrap's outer process and its
/// first process inside inherit the mask, so they end only with the harness or by KILL,
/// and a stop signal to the group is the harness's to handle. Last it gives up new
/// privileges. Output: 3 when bubblewrap is refused or any of the three fails, 126 when
/// bubblewrap cannot start. With a bridge it first starts the
/// broker on this network and writes the bridge flags for the helper inside
/// ([`bridge::open`]), keeping its end of the bridge open until bubblewrap has it.
fn nest(bwrap: &Path, args: &[OsString], bridge: Option<&Bridge>) -> ExitCode {
    let Some(program) = trusted_file(bwrap) else {
        eprintln!(
            "crazyagent-landlock: {} is not a bubblewrap to nest under",
            bwrap.display()
        );
        return ExitCode::from(3);
    };
    let ready = own_session()
        .and_then(|()| match bridge {
            Some(bridge) => bridge::open(&bridge.ports, args)
                .map(|(end, args)| (Some(end), args)),
            None => Ok((None, args.to_vec())),
        })
        .and_then(|opened| {
            nix::sys::signal::SigSet::all()
                .thread_block()
                .map_err(|error| format!("signal mask: {error}"))?;
            nix::sys::prctl::set_no_new_privs()
                .map_err(|error| format!("no new privileges: {error}"))?;
            Ok(opened)
        });
    let (_end, args) = match ready {
        Ok(opened) => opened,
        Err(reason) => {
            eprintln!("crazyagent-landlock: {reason}");
            return ExitCode::from(3);
        }
    };
    assert_eq!(nix::sys::prctl::get_no_new_privs(), Ok(true));
    let error = exec_keeping_mask(&program, bwrap, &args);
    eprintln!(
        "crazyagent-landlock: cannot start {}: {error}",
        bwrap.display()
    );
    ExitCode::from(126)
}

/// Tells whether glibc loads code through a variable of this name: every name that begins
/// `LD_`, in that case, `GLIBC_TUNABLES`, and `GCONV_PATH`, which a static glibc still
/// reads for its gconv modules. Input: the name. Output: true when it does, so the helper
/// runs no mode with it ([`LOADER_REFUSED`]).
pub fn loader_variable(name: &OsStr) -> bool {
    let name = name.as_bytes();
    name.starts_with(b"LD_") || name == b"GLIBC_TUNABLES" || name == b"GCONV_PATH"
}

/// Replaces this process with the open `program`, keeping its signal mask and its
/// environment. `std`'s exec empties the mask first, which would undo [`nest`], and an exec
/// by path could start another file than the one [`trusted_file`] checked. Inputs: the open
/// program, the name it starts under and its arguments after that name. Output: only the
/// reason the exec failed.
fn exec_keeping_mask(program: &fs::File, name: &Path, args: &[OsString]) -> String {
    assert!(std::env::vars_os().all(|(name, _)| !loader_variable(&name)));
    let words: Result<Vec<CString>, _> = std::iter::once(name.as_os_str())
        .chain(args.iter().map(OsString::as_os_str))
        .map(|word| CString::new(word.as_bytes()))
        .collect();
    let kept: Result<Vec<CString>, _> = std::env::vars_os()
        .map(|(key, value)| CString::new([key.as_bytes(), &b"="[..], value.as_bytes()].concat()))
        .collect();
    let (Ok(words), Ok(kept)) = (words, kept) else {
        return "an argument or a variable holds a NUL byte".into();
    };
    assert!(words.len() > NEST_ARGS.len());
    let Err(errno) = nix::unistd::fexecve(program.as_raw_fd(), &words, &kept);
    errno.to_string()
}

/// Places the helper for its harness. Inside bubblewrap it only unblocks every signal, as
/// [`nest`] took the session outside and left it the full mask, so the harness takes its
/// stop and terminal signals as a plain Landlock agent does; anywhere else it takes a
/// session of its own. Input: whether it runs nested. Output: Ok, or the reason.
fn placed(nested: bool) -> Result<(), String> {
    if !nested {
        return own_session();
    }
    nix::sys::signal::SigSet::empty()
        .thread_set_mask()
        .map_err(|error| format!("signal mask: {error}"))
}

/// Starts a harness bubblewrap launched, behind the socket filter alone, the one a Landlock
/// agent with a network limit gets ([`refuse_sockets`] with `tcp_only`): no Unix-domain
/// socket, so a closed agent reaches no socket in the folders bound for it, no io_uring
/// ring, and no socket but TCP over IPv4 or IPv6 and routing netlink, so no AF_VSOCK or
/// other family that `--unshare-net` does not isolate reaches outside the closed network
/// (RV-DS241-84 r2). Bubblewrap already holds the files, the network namespace and the
/// session. Input: the harness. Output: 3 when the filter cannot be installed, 126 when the
/// harness cannot start.
fn filtered(program: &Path, args: Vec<OsString>) -> ExitCode {
    if let Err(reason) = refuse_sockets(true, false) {
        eprintln!("crazyagent-landlock: {reason}");
        return ExitCode::from(3);
    }
    let error = Command::new(program).args(args).exec();
    eprintln!(
        "crazyagent-landlock: cannot start {}: {error}",
        program.display()
    );
    ExitCode::from(126)
}

/// The Linux mechanism holding a project agent's boundary: the `crazyagent-landlock` helper,
/// that helper inside bubblewrap's user and process namespaces where this machine makes
/// them (`Nested`), or bubblewrap where the kernel cannot fully enforce its Landlock rules,
/// with the helper started inside it to install the socket filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinuxBackend {
    Landlock { helper: PathBuf },
    Nested { bwrap: PathBuf, helper: PathBuf },
    Bubblewrap { bwrap: PathBuf, helper: PathBuf },
}

/// The Linux boundary for one project agent. It keeps the [`ProjectSandbox`] policy on both
/// backends: the project folder is read-only, its existing entries are writable except the
/// protected paths, symbolic links, the git metadata git follows (`GIT_FOLLOWED`) and the
/// git state the Seatbelt profile denies ([`GIT_STATE_TREES`], [`GIT_STATE_FILES`]), and
/// the agent's home, temp dir and worktrees are writable. A policy confined to one worktree
/// is refused, since neither backend can hold its git denies. Landlock limits outbound TCP to the model endpoint ports
/// and, under the owner's [`LinuxSockets`] setting, the socket filter refuses every other kind
/// of network socket or no limited-network agent starts; bubblewrap cannot limit ports, so a
/// tools-off agent needs Landlock. An open network needs Landlock as well: its bind rules keep
/// the agent's listeners off the owner's fixed ports, the kept local model ports and the
/// model proxy ports (`kept_free`), which bubblewrap cannot do.
#[derive(Debug)]
pub struct LinuxSandbox {
    sandbox: ProjectSandbox,
    backend: LinuxBackend,
    supervision: Option<Supervision>,
    routed: Option<Routed>,
}

/// What the call supervisor holds for a launch on this host beyond the guards every
/// supervised launch carries: `signals` where neither a process namespace nor the
/// kernel's signal scope keeps the agent's signals to itself (DEC-179 T4c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Supervision {
    signals: bool,
}

impl LinuxSandbox {
    /// Binds the policy to one backend. Input: the project policy and the helper, or bwrap
    /// and the helper, each an absolute file. Output: the sandbox, `Invalid` for a bad
    /// executable, or `Unavailable` for a network limit or an open network on bubblewrap,
    /// which keeps no port free ([`LINUX_OPEN_NETWORK_NEEDS_LANDLOCK`]), for a policy confined to one
    /// worktree, whose git denies neither backend can hold, for a limited network the
    /// sockets setting starts no agent on, or on Landlock for a kept port Linux could hand
    /// out by itself ([`kept_outside_picked`]). Bubblewrap starts only a closed agent, in a
    /// network of its own, where no port it takes is a host port.
    pub fn new(sandbox: ProjectSandbox, backend: LinuxBackend) -> Result<Self, SandboxError> {
        admitted(&sandbox)?;
        let (launcher, inside) = match &backend {
            LinuxBackend::Landlock { helper } => (helper, None),
            LinuxBackend::Nested { bwrap, helper } => (helper, Some(bwrap)),
            LinuxBackend::Bubblewrap { bwrap, helper } => (bwrap, Some(helper)),
        };
        if std::iter::once(launcher)
            .chain(inside)
            .any(|executable| !clean_absolute(executable) || !executable.is_file())
        {
            return Err(invalid("sandbox executable must be an absolute file"));
        }
        if let LinuxBackend::Nested { bwrap, .. } = &backend
            && !trusted(bwrap)
        {
            return Err(invalid(
                "bubblewrap must have no set-ID bit and belong to root or this user",
            ));
        }
        if matches!(backend, LinuxBackend::Bubblewrap { .. }) {
            match sandbox.network() {
                SandboxNetwork::Open => {
                    return Err(SandboxError::Unavailable(
                        LINUX_OPEN_NETWORK_NEEDS_LANDLOCK.into(),
                    ));
                }
                SandboxNetwork::Only(remotes) if !remotes.is_empty() => {
                    return Err(SandboxError::Unavailable(
                        "limiting an agent's network to a few ports needs Landlock network \
                         rules (Linux 6.7 or newer); bubblewrap cannot limit ports"
                            .into(),
                    ));
                }
                SandboxNetwork::Only(_) => {}
            }
        } else {
            kept_outside_picked(&kept_free(&sandbox), read_picked_ports().as_ref())
                .map_err(SandboxError::Unavailable)?;
        }
        Ok(Self {
            sandbox,
            backend,
            supervision: None,
            routed: None,
        })
    }

    /// Puts this launch under the call supervisor when the owner's setting is On and the
    /// probe in the engine's data folder holds ([`supervises`]); bubblewrap alone never runs
    /// it. Off keeps the launch as it was; so does a failed probe, which says so on stderr and
    /// marks the kept host unsupervised ([`forget_supervisor`]). Inputs: the choice and the
    /// data folder, none where no probe may run. Output: the sandbox.
    pub fn supervise(mut self, calls: LinuxSupervisor, data: Option<&Path>) -> Self {
        assert!(self.supervision.is_none());
        if calls == LinuxSupervisor::Off {
            return self;
        }
        let root = rustix::process::geteuid().is_root();
        let holds = |helper: &PathBuf, bwrap: Option<&Path>| {
            data.is_some_and(|data| supervises(helper, bwrap, data, root))
        };
        let supervision = match &self.backend {
            LinuxBackend::Landlock { helper } if holds(helper, None) => {
                let scoped = passes(Command::new(helper).arg(SCOPED));
                Some(Supervision { signals: !scoped })
            }
            LinuxBackend::Nested { bwrap, helper } if holds(helper, Some(bwrap.as_path())) => {
                Some(Supervision { signals: false })
            }
            LinuxBackend::Landlock { .. } | LinuxBackend::Nested { .. } => {
                eprintln!("linux_supervisor_not_started: this launch runs unsupervised");
                forget_supervisor(&HOST_READ);
                None
            }
            LinuxBackend::Bubblewrap { .. } => None,
        };
        self.supervision = supervision;
        self
    }

    /// The policy with the guards this launch's supervision carries, none without one,
    /// checked as the helper checks it ([`guard_fits`]). A nested launch never carries the
    /// signal guard. Input: the policy. Output: the policy, or `Invalid` for more own git
    /// folders than the helper takes.
    fn with_guard(&self, policy: LandlockPolicy) -> Result<LandlockPolicy, SandboxError> {
        let Some(supervision) = self.supervision else {
            return Ok(policy);
        };
        let guarded = LandlockPolicy {
            guard: guard(&self.sandbox, supervision),
            ..policy
        };
        assert!(!guarded.nested || !guarded.guard.signals);
        assert!(guarded.guard != Guard::default());
        guard_fits(&guarded).map_err(|reason| invalid(&reason))?;
        Ok(guarded)
    }

    /// Picks the backend this machine holds: Landlock when the helper exists and its probe
    /// passes for the policy's network needs, nested in bubblewrap when the same probe also
    /// passes inside its namespaces ([`nests`]), otherwise bubblewrap from a fixed path with
    /// the helper started inside it, so no bubblewrap agent starts without the helper. An
    /// open network probes the TCP bind rules that keep ports free and never falls back to
    /// bubblewrap. Input: the project policy and the helper executable. Output: the
    /// sandbox, or `Unavailable` when no backend can hold it.
    pub fn detect(sandbox: ProjectSandbox, helper: &Path) -> Result<Self, SandboxError> {
        let bwraps: Vec<&Path> = BWRAP_PATHS.iter().map(Path::new).collect();
        Self::detect_among(sandbox, helper, &bwraps)
    }

    /// Picks the backend as [`LinuxSandbox::detect`] does, with bubblewrap looked for only
    /// at `bwraps`, so a caller with none pins the plain Landlock backend. Inputs: the
    /// project policy, the helper executable and the bubblewrap places, in order. Output:
    /// the sandbox, or `Unavailable` when no backend can hold it.
    pub fn detect_among(
        sandbox: ProjectSandbox,
        helper: &Path,
        bwraps: &[&Path],
    ) -> Result<Self, SandboxError> {
        admitted(&sandbox)?;
        let mut probe = Command::new(helper);
        probe.arg(PROBE);
        let open = matches!(sandbox.network(), SandboxNetwork::Open);
        if open {
            probe.arg(KEEP_FREE).arg(OWNER_PORTS[0].to_string());
        } else {
            probe.arg(LIMIT_NETWORK);
            if sandbox.linux_sockets() == LinuxSockets::TcpPorts {
                probe.arg(OPEN_SOCKETS);
            }
        }
        let flags: Vec<OsString> = probe.get_args().map(OsStr::to_os_string).collect();
        let held = clean_absolute(helper) && helper.is_file() && passes(&mut probe);
        if held {
            let helper = helper.to_path_buf();
            let nest = bwraps
                .iter()
                .find(|bwrap| bwrap.is_file())
                .filter(|bwrap| nests(bwrap, &helper, &flags));
            let backend = match nest {
                Some(bwrap) => LinuxBackend::Nested {
                    bwrap: bwrap.to_path_buf(),
                    helper,
                },
                None => LinuxBackend::Landlock { helper },
            };
            return Self::new(sandbox, backend);
        }
        if open {
            return Err(SandboxError::Unavailable(
                LINUX_OPEN_NETWORK_NEEDS_LANDLOCK.into(),
            ));
        }
        if !clean_absolute(helper) || !helper.is_file() {
            return Err(SandboxError::Unavailable(HELPER_MISSING.into()));
        }
        match bwraps.iter().find(|path| path.is_file()) {
            Some(bwrap) => Self::new(
                sandbox,
                LinuxBackend::Bubblewrap {
                    bwrap: bwrap.to_path_buf(),
                    helper: helper.to_path_buf(),
                },
            ),
            None => Err(SandboxError::Unavailable(
                "this Linux cannot fully enforce Landlock file rules (Linux 6.2 or newer) and \
                 has no bubblewrap; install bubblewrap"
                    .into(),
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

    /// The network of its own this sandbox gives its agent, as detection found this machine
    /// holds it ([`ProjectSandbox::routed`]); none when the agent keeps the shared network.
    pub fn routed(&self) -> Option<&Routed> {
        self.routed.as_ref()
    }

    /// The program every wrapped command starts: the Landlock helper or bwrap.
    pub fn launcher(&self) -> &Path {
        match &self.backend {
            LinuxBackend::Landlock { helper } | LinuxBackend::Nested { helper, .. } => helper,
            LinuxBackend::Bubblewrap { bwrap, .. } => bwrap,
        }
    }
}

/// Runs a probe as [`passes`] does, but ends it once `time_max` has gone by; the harness
/// the call supervisor probe starts dies with it. Inputs: the probe and its longest run.
/// Output: true only when it exited 0 in time.
fn passes_within(probe: &mut Command, time_max: Duration) -> bool {
    let started = Instant::now();
    let quiet = probe
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = crate::spawn_gate::spawn(quiet) else {
        return false;
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < time_max => std::thread::sleep(PROBE_POLL),
            Ok(None) | Err(_) => break,
        }
    }
    if child.kill().is_ok() {
        let reaped = child.wait();
        assert!(reaped.is_ok(), "a killed probe is this process's child to reap");
    }
    false
}

/// Makes a folder only this user may open under a random name in `data`, the engine's data
/// folder, for one supervisor probe, so no stale, concurrent or planted folder takes its
/// name (rv-secreview linux-t4c F1). Input: the absolute data folder. Output: the new empty
/// folder, or none when no name was free or the data folder refused it.
fn private_folder(data: &Path) -> Option<PathBuf> {
    assert!(data.is_absolute());
    for _ in 0..PROBE_FOLDER_TRIES_MAX {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).ok()?;
        let name: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(name.len(), 32);
        let folder = data.join(format!("{PROBE_FOLDER_PREFIX}{name}"));
        match fs::DirBuilder::new().mode(0o700).create(&folder) {
            Ok(()) => return Some(folder),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    None
}

/// Tells whether the call supervisor holds every guard here as a launch starts it: the
/// helper's probe ([`probe_supervisor`]), plain or inside bubblewrap's namespaces, in a new
/// private folder under the engine's data folder ([`private_folder`]), removed after. Never
/// as root (rv-sec3 linux-t4b N1). Inputs: the helper, the bubblewrap launches nest under,
/// the data folder and whether this runs as root. Output: true only when the probe held.
fn supervises(helper: &Path, bwrap: Option<&Path>, data: &Path, root: bool) -> bool {
    if root || !(clean_absolute(helper) && helper.is_file() && clean_absolute(data)) {
        return false;
    }
    let mut probe = Command::new(helper);
    match bwrap {
        None => {}
        Some(bwrap) if clean_absolute(bwrap) && trusted(bwrap) => {
            probe.arg(NEST).arg("--").arg(bwrap);
            probe.args(NEST_ARGS).arg("--").arg(helper);
        }
        Some(_) => return false,
    }
    let Some(folder) = private_folder(data) else {
        return false;
    };
    assert!(folder.starts_with(data));
    probe.arg(PROBE_SUPERVISOR).arg(&folder);
    let held = private_empty(&folder) && passes_within(&mut probe, SUPERVISOR_PROBE_TIME_MAX);
    if let Err(error) = fs::remove_dir_all(&folder) {
        eprintln!(
            "linux_supervisor_probe_folder_kept {}: {error}",
            folder.display()
        );
    }
    held
}

/// Runs a probe with no environment and no input or output. Input: the probe command.
/// Output: true only when it ran and exited 0.
fn passes(probe: &mut Command) -> bool {
    crate::spawn_gate::status(
        probe
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .is_ok_and(|status| status.success())
}

/// Opens a bubblewrap the helper may nest an agent under and checks the file it opened, not
/// the path again: a file that root or this user owns, with neither [`SET_ID_BITS`] bit, so
/// it runs with this user's privileges and was put there by root or by this user. It opens
/// without reading, so a bubblewrap that can only be run still opens. Input: its path.
/// Output: the open file, which [`nest`] starts, or none.
fn trusted_file(bwrap: &Path) -> Option<fs::File> {
    let own = rustix::process::geteuid().as_raw();
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open(bwrap)
        .ok()?;
    let held = file.metadata().ok()?;
    let plain = held.is_file() && held.mode() & SET_ID_BITS == 0;
    (plain && [0, own].contains(&held.uid())).then_some(file)
}

/// Tells whether a bubblewrap is one the helper may nest an agent under ([`trusted_file`]).
/// A host whose bubblewrap fails this stays on plain Landlock with its gap lines. Input:
/// its path. Output: true when it may.
fn trusted(bwrap: &Path) -> bool {
    trusted_file(bwrap).is_some()
}

/// Tells whether this machine starts the helper inside bubblewrap's namespaces and holds
/// the same probe there, by running the widest nest a launch asks for: the helper,
/// bubblewrap with [`NEST_ARGS`] and a network of its own with one port bridged, then the
/// helper's probe. A bubblewrap that only exists proves nothing: Ubuntu 24.04 refuses it
/// the user namespace. A bubblewrap [`trusted`] refuses is never started. Inputs:
/// bubblewrap, the helper and the probe's arguments. Output: true only when the nested
/// probe exits 0.
fn nests(bwrap: &Path, helper: &Path, flags: &[OsString]) -> bool {
    assert!(flags.first().is_some_and(|flag| flag == PROBE));
    clean_absolute(bwrap)
        && trusted(bwrap)
        && passes(
            Command::new(helper)
                .arg(bridge::BRIDGE)
                .arg(BRIDGE_PORT_MIN.to_string())
                .arg(NEST)
                .arg("--")
                .arg(bwrap)
                .args(NEST_ARGS)
                .arg(bridge::UNSHARE_NET)
                .arg("--")
                .arg(helper)
                .args(flags),
        )
}

/// Tells whether AppArmor keeps unprivileged programs out of user namespaces, from what
/// [`APPARMOR_USERNS`] reads: only `1`, with or without its newline, does. Input: the text,
/// none where the file cannot be read (no AppArmor, or an older one). Output: true when it
/// restricts them.
fn restricts(setting: Option<&str>) -> bool {
    setting.is_some_and(|text| text.strip_suffix('\n').unwrap_or(text) == "1")
}

/// Reads what this machine holds for an agent beyond its Landlock rules: whether the
/// kernel scopes signals (the helper's `--scoped`), whether a launch gives the agent a
/// process namespace ([`nests`], or the plain probe fails), whether the call supervisor
/// holds ([`supervises`]), whether this runs as root and whether AppArmor's user namespace
/// restriction, which never covers root, is why nothing nests ([`restricts`]). Inputs: the
/// helper and the engine's data folder, none where no probe may run. Output: the facts.
pub fn read_host(helper: &Path, data: Option<&Path>) -> super::LinuxHost {
    let usable = clean_absolute(helper) && helper.is_file();
    let scoped = usable && passes(Command::new(helper).arg(SCOPED));
    let held = usable && passes(Command::new(helper).arg(PROBE));
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file());
    let apart = |bwrap: &Path| !held || nests(bwrap, helper, &[PROBE.into()]);
    let nested = usable && bwrap.is_some_and(apart);
    let root = rustix::process::geteuid().is_root();
    let nest = bwrap.filter(|_| nested);
    let supervised = held && data.is_some_and(|data| supervises(helper, nest, data, root));
    let setting = fs::read_to_string(APPARMOR_USERNS).ok();
    let armored = !nested && !root && restricts(setting.as_deref());
    assert!(!(root && supervised));
    assert!(!(armored && nested));
    assert!(!(armored && root));
    super::LinuxHost {
        nested,
        scoped,
        supervised,
        root,
        armored,
        routed: false,
    }
}

/// Tells whether a project may hold a git repository other than its own top-level `.git`:
/// an entry of any kind that is `.git` to some filesystem below the top, or one at the top
/// not spelled `.git`. A folder it cannot read, or more entries than it reads, answers yes.
/// Links are not followed. Input: the project folder. Output: true when one may be there.
pub(crate) fn holds_other_git(project: &Path) -> bool {
    let mut folders = vec![project.to_path_buf()];
    let mut seen: usize = 0;
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            return true;
        };
        for entry in entries {
            seen += 1;
            let Ok(entry) = entry else {
                return true;
            };
            if seen > GIT_SEARCH_ENTRIES_MAX {
                return true;
            }
            let name = entry.file_name();
            if is_git(&name) {
                let own = folder == project && name.as_os_str() == OsStr::new(".git");
                if !own {
                    return true;
                }
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                return true;
            };
            if kind.is_dir() {
                folders.push(entry.path());
            }
            assert!(folders.len() <= seen);
        }
    }
    assert!(seen <= GIT_SEARCH_ENTRIES_MAX);
    false
}

/// The most entries [`holds_other_git`] reads in a project before it answers yes: a read
/// runs inside the engine's read of the project's Agent access page.
const GIT_SEARCH_ENTRIES_MAX: usize = 4096;

/// What is kept of this machine and when it was read.
type KeptHost = Mutex<Option<(Instant, super::LinuxHost)>>;

/// What [`host_within`] last read of this machine, which a launch whose own probe failed
/// marks unsupervised ([`forget_supervisor`]).
static HOST_READ: KeptHost = Mutex::new(None);

/// [`read_host`] for the helper beside this executable and the engine's data folder, read
/// again once what is kept is older than `age_max`, or with no age only when nothing is
/// kept. The lock is not held while the helper runs, so a reader of what is kept never
/// waits on it. Input: the age. Output: the facts, each false when it cannot be asked.
fn host_within(age_max: Option<Duration>) -> super::LinuxHost {
    let kept = *HOST_READ.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, host)) = kept
        && age_max.is_none_or(|age_max| at.elapsed() < age_max)
    {
        return host;
    }
    let helper = std::env::current_exe().map(|engine| engine.with_file_name(HELPER_NAME));
    let host = read_host(&helper.unwrap_or_default(), crate::tools::engine_root());
    *HOST_READ.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), host));
    host
}

/// Marks what is kept of this machine as running no call supervisor, after a launch with
/// the setting On found it does not hold (rv-secreview linux-t4c F2), so the Agent access
/// page shows the wider lines until the next read. Nothing kept stays nothing. Input: what
/// is kept.
fn forget_supervisor(kept: &KeptHost) {
    let mut held = kept.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, host)) = held.as_mut() {
        host.supervised = false;
    }
    assert!(held.as_ref().is_none_or(|(_, host)| !host.supervised));
}

/// What this machine holds, read again once what is kept is older than [`HOST_AGE_MAX`]:
/// it may start the helper and bubblewrap, so it is not for a database transaction. No
/// input. Output: the facts as [`read_host`] gives them.
pub(crate) fn host_now() -> super::LinuxHost {
    host_within(Some(HOST_AGE_MAX))
}

/// What this machine holds as it was last read, however long ago, read now only when it
/// never was: after [`host_now`] it starts nothing. No input. Output: the facts as
/// [`read_host`] gives them.
pub(crate) fn host_kept() -> super::LinuxHost {
    host_within(None)
}

/// Refuses a policy neither Linux backend starts: one confined to its own worktree, or a
/// limited network under [`LinuxSockets::Refuse`]. Input: the policy. Output: Ok, or the
/// plain `Unavailable` reason.
fn admitted(sandbox: &ProjectSandbox) -> Result<(), SandboxError> {
    if sandbox.checkout.is_some() || sandbox.clone.is_some() {
        return Err(SandboxError::Unavailable(CONFINED_UNAVAILABLE.into()));
    }
    if matches!(sandbox.network(), SandboxNetwork::Only(_))
        && sandbox.linux_sockets() == LinuxSockets::Refuse
    {
        return Err(SandboxError::Unavailable(LIMITED_NETWORK_OFF.into()));
    }
    Ok(())
}

impl Sandbox for LinuxSandbox {
    fn wrap(&self, harness: &Command) -> Result<Command, SandboxError> {
        let program = Path::new(harness.get_program());
        if !clean_absolute(program) {
            return Err(invalid("harness program must be an absolute plain path"));
        }
        match self.sandbox.linux_refusal(program) {
            None => {}
            Some(LINUX_LIMITED_NETWORK_OFF) => {
                return Err(SandboxError::Unavailable(LIMITED_NETWORK_OFF.into()));
            }
            Some(LINUX_HARNESS_UNCHECKED) => {
                return Err(SandboxError::Unavailable(HARNESS_UNCHECKED.into()));
            }
            Some(code) => {
                assert_eq!(code, LINUX_RESOLVER_NEEDS_UDP);
                return Err(SandboxError::Unavailable(RESOLVER_NEEDS_UDP.into()));
            }
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
                let policy = self.with_guard(landlock_policy(&grants, &self.sandbox))?;
                let mut command = Command::new(helper);
                command.args(policy.args());
                command
            }
            LinuxBackend::Nested { bwrap, helper } => {
                let policy = self.with_guard(LandlockPolicy {
                    nested: true,
                    ..landlock_policy(&grants, &self.sandbox)
                })?;
                let bridged = self.sandbox.bridged_ports().unwrap_or_default();
                nest_command(bwrap, helper, &policy, cwd, &bridged, &[])
            }
            LinuxBackend::Bubblewrap { bwrap, helper } => {
                bwrap_command(bwrap, helper, &grants, cwd, self.sandbox.network())
            }
        };
        let mut base = vec![
            ("HOME", self.sandbox.agent_home().as_os_str()),
            ("TMPDIR", self.sandbox.agent_tmp().as_os_str()),
            ("PATH", OsStr::new(AGENT_PATH)),
            ("LANG", OsStr::new("C")),
            ("LC_ALL", OsStr::new("C")),
        ];
        if self.sandbox.resolves_over_tcp() {
            base.push(("RES_OPTIONS", OsStr::new(DNS_OVER_TCP)));
        }
        let env = agent_env(&base, harness.get_envs())?;
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
        let objects = git.join("objects");
        let objects_info = objects.join("info");
        let followed: Vec<PathBuf> = GIT_FOLLOWED
            .iter()
            .chain(&GIT_STATE_TREES)
            .chain(&GIT_STATE_FILES)
            .map(|name| git.join(name))
            .collect();
        let followed: Vec<&PathBuf> = followed.iter().collect();
        let folder = |path: &Path| fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir());
        let mut write = Vec::new();
        for root in sandbox.writable_roots() {
            if root != project {
                write.push(root.clone());
                continue;
            }
            writable_children(project, &[&git, &state], &protected, &mut write)?;
            if folder(&git) {
                writable_children(&git, &followed, &protected, &mut write)?;
                if folder(&objects) {
                    writable_children(&objects, &[&objects_info], &protected, &mut write)?;
                }
            }
            if folder(&state) {
                writable_children(&state, &[], &protected, &mut write)?;
            }
        }
        assert!(
            write
                .iter()
                .all(|path| !followed.contains(&path) && *path != objects_info)
        );
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
        let carved = sandbox.carved_home.as_ref();
        assert!(read.iter().chain(&write).all(|grant| {
            secrets.iter().all(|secret| {
                (carved == Some(grant) && grant.parent() == Some(secret.as_path()))
                    || (!grant.starts_with(secret) && !secret.starts_with(grant))
            })
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

/// The guards a supervised launch carries (DEC-179 T4c): the loopback ports of a limited
/// network under [`LinuxSockets::TcpOnly`], each then reached on this computer only, but a
/// port the agent may also reach on any host; the agent's own git folders
/// ([`git_own`]); and signals where [`Supervision`] says nothing else holds them. Inputs:
/// the policy and what the supervisor holds here. Output: the guard.
fn guard(sandbox: &ProjectSandbox, supervision: Supervision) -> Guard {
    let loopback = match sandbox.network() {
        SandboxNetwork::Only(remotes) if sandbox.linux_sockets() == LinuxSockets::TcpOnly => {
            let anywhere = |port: u16| {
                remotes.contains(&SandboxRemote::AnyHost(port))
                    || (sandbox.resolves_over_tcp() && port == DNS_PORT)
            };
            ports(remotes)
                .into_iter()
                .filter(|port| {
                    remotes.contains(&SandboxRemote::Loopback(*port)) && !anywhere(*port)
                })
                .collect()
        }
        SandboxNetwork::Only(_) | SandboxNetwork::Open => Vec::new(),
    };
    let held = Guard {
        loopback,
        git: Some(git_own(sandbox)),
        signals: supervision.signals,
    };
    assert!(!held.loopback.contains(&0));
    assert!(held.loopback.windows(2).all(|pair| pair[0] < pair[1]));
    held
}

/// The folders an agent may make a `.git` entry in, as the macOS boundary's nested git
/// rule allows them: the top of each working tree it works in, and each folder directly
/// inside the engine's worktree folders (when it adds worktrees) or inside a setup folder.
/// Input: the policy. Output: the agent's own git folders.
fn git_own(sandbox: &ProjectSandbox) -> GitOwn {
    assert!(sandbox.checkout.is_none());
    assert!(sandbox.clone.is_none());
    let (working, _) = sandbox.git_deny_roots();
    let mut kids = Vec::new();
    if sandbox.adds_worktrees {
        let project = sandbox.project_root();
        let state = project.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
        kids.extend([project.join("worktrees"), state.join("worktrees")]);
    }
    if let Some(setup) = &sandbox.setup {
        kids.push(setup.folder.clone());
    }
    let own = GitOwn {
        trees: working.into_iter().map(Path::to_path_buf).collect(),
        kids,
        exact: Vec::new(),
    };
    assert!(!own.trees.is_empty());
    own
}

fn landlock_policy(grants: &Grants, sandbox: &ProjectSandbox) -> LandlockPolicy {
    let mut read: Vec<PathBuf> = SYSTEM_READ_ROOTS
        .iter()
        .chain(&["/proc"])
        .map(PathBuf::from)
        .collect();
    read.extend(grants.read.iter().cloned());
    let mut write: Vec<PathBuf> = LANDLOCK_DEVICES.iter().map(PathBuf::from).collect();
    write.extend(grants.write.iter().cloned());
    let (limit_network, mut connect) = match sandbox.network() {
        SandboxNetwork::Open => (false, Vec::new()),
        SandboxNetwork::Only(remotes) => (true, ports(remotes)),
    };
    if sandbox.resolves_over_tcp() {
        connect.push(DNS_PORT);
        connect.sort_unstable();
        connect.dedup();
    }
    let open_sockets = limit_network && sandbox.linux_sockets() == LinuxSockets::TcpPorts;
    let keep_free = kept_free(sandbox);
    assert!(
        read.len() + write.len() <= HELPER_PATHS_MAX,
        "the helper accepts every policy the engine writes"
    );
    assert!(
        !keep_free.is_empty(),
        "every agent is held to the ports Linux picks"
    );
    LandlockPolicy {
        read,
        write,
        connect,
        limit_network,
        open_sockets,
        keep_free,
        die_with: None,
        nested: false,
        guard: Guard::default(),
    }
}

/// The TCP ports no agent listens on: the owner's fixed ports, the kept local model ports
/// and every model proxy and model relay port, as the macOS boundary keeps them. Input: the policy. Output:
/// those ports, rising and each once.
fn kept_free(sandbox: &ProjectSandbox) -> Vec<u16> {
    let mut ports: Vec<u16> = OWNER_PORTS
        .iter()
        .chain(&sandbox.kept_ports)
        .copied()
        .chain(PROXY_PORTS)
        .chain(RELAY_PORTS)
        .collect();
    ports.sort_unstable();
    ports.dedup();
    assert!(OWNER_PORTS.iter().all(|port| ports.contains(port)));
    assert!(!ports.contains(&0));
    assert!(ports.len() <= KEEP_FREE_MAX);
    ports
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

/// The command for a nested agent up to its harness: the helper asked to start bubblewrap
/// ([`NEST`]) with [`NEST_ARGS`] in the working folder, and inside it the helper again with
/// the policy. With route or bridged ports the route flags lead, then the bridge flags,
/// and bubblewrap gives the agent a network of its own. Inputs: bubblewrap, the helper,
/// the policy, marked nested, the working folder, the bridged ports and the route ports.
/// Output: the command, which the harness follows after `--`.
fn nest_command(
    bwrap: &Path,
    helper: &Path,
    policy: &LandlockPolicy,
    cwd: &Path,
    bridged: &[u16],
    routes: &[u16],
) -> Command {
    assert!(policy.nested);
    assert!(policy.die_with.is_none());
    assert!(routes.len() <= REMOTES_MAX);
    let mut command = Command::new(helper);
    for port in bridged {
        command.arg(bridge::BRIDGE).arg(port.to_string());
    }
    command.arg(NEST).arg("--").arg(bwrap).args(NEST_ARGS);
    if !bridged.is_empty() {
        command.arg(bridge::UNSHARE_NET);
    }
    command
        .arg("--chdir")
        .arg(cwd)
        .arg("--")
        .arg(helper)
        .args(policy.args());
    assert_eq!(command.get_args().filter(|arg| *arg == NEST).count(), 1);
    command
}

/// The bubblewrap command for a closed agent up to its harness: its own namespaces, the
/// system folders read-only but the name resolver's ([`RESOLVER_ROOT`]), the grants, then
/// the helper bound read-only and started to install the TCP-only socket filter
/// ([`FILTER_SOCKETS`]).
/// Inputs: bwrap, the helper, the grants, the working folder and the network. Output: the
/// command, which the harness follows after `--`.
fn bwrap_command(
    bwrap: &Path,
    helper: &Path,
    grants: &Grants,
    cwd: &Path,
    network: &SandboxNetwork,
) -> Command {
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
    if matches!(network, SandboxNetwork::Only(remotes) if remotes.is_empty()) {
        command.arg("--unshare-net");
    }
    let system: Vec<&str> = SYSTEM_READ_ROOTS
        .iter()
        .copied()
        .filter(|root| *root != RESOLVER_ROOT)
        .collect();
    assert_eq!(system.len() + 1, SYSTEM_READ_ROOTS.len());
    for root in system {
        command.args(["--ro-bind-try", root, root]);
    }
    for root in &grants.read {
        command.arg("--ro-bind").arg(root).arg(root);
    }
    for root in &grants.write {
        command.arg("--bind").arg(root).arg(root);
    }
    command.arg("--ro-bind").arg(helper).arg(helper);
    command.arg("--chdir").arg(cwd);
    command.arg("--").arg(helper).arg(FILTER_SOCKETS);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn helper() -> PathBuf {
        Path::new("/opt/crazyagent").join(HELPER_NAME)
    }

    #[test]
    fn only_fully_enforced_rules_with_no_new_privs_hold_the_boundary() {
        assert!(accepted(&RulesetStatus::FullyEnforced, true));
        assert!(!accepted(&RulesetStatus::PartiallyEnforced, true));
        assert!(!accepted(&RulesetStatus::NotEnforced, true));
        assert!(!accepted(&RulesetStatus::FullyEnforced, false));
        assert!(!accepted(&RulesetStatus::PartiallyEnforced, false));
    }

    #[test]
    fn bubblewrap_starts_the_agent_in_a_new_session() {
        let grants = Grants {
            read: Vec::new(),
            write: Vec::new(),
        };
        let command = bwrap_command(
            Path::new(BWRAP_PATHS[0]),
            &helper(),
            &grants,
            Path::new("/tmp"),
            &SandboxNetwork::Open,
        );
        assert!(command.get_args().any(|arg| arg == "--new-session"));
    }

    #[test]
    fn bubblewrap_cuts_off_the_network_only_when_nothing_is_allowed() {
        let grants = Grants {
            read: Vec::new(),
            write: Vec::new(),
        };
        let unshares = |network: SandboxNetwork| {
            bwrap_command(
                Path::new(BWRAP_PATHS[0]),
                &helper(),
                &grants,
                Path::new("/tmp"),
                &network,
            )
            .get_args()
            .any(|arg| arg == "--unshare-net")
        };
        assert!(unshares(SandboxNetwork::Only(Vec::new())));
        assert!(!unshares(SandboxNetwork::Open));
    }

    fn project(network: SandboxNetwork) -> (tempfile::TempDir, ProjectSandbox) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let folders = ["project", "home", "tmp"].map(|name| root.join(name));
        for folder in &folders {
            fs::create_dir(folder).unwrap();
        }
        let sandbox = ProjectSandbox::new(&folders[0], &folders[1], &folders[2], network).unwrap();
        (temp, sandbox)
    }

    #[test]
    fn an_open_network_agent_binds_every_port_but_the_kept_ones() {
        let keep_free = [1455, 11434, 41300];
        let ports = bindable(&keep_free);
        assert_eq!(ports.len(), usize::from(u16::MAX) + 1 - keep_free.len());
        assert_eq!(ports[..3], [0, 1, 2]);
        assert_eq!(ports.last(), Some(&u16::MAX));
        for port in keep_free {
            assert!(!ports.contains(&port), "{port}");
            assert!(ports.contains(&(port - 1)), "{port}");
            assert!(ports.contains(&(port + 1)), "{port}");
        }
        assert_eq!(bindable(&[]).len(), usize::from(u16::MAX) + 1);
    }

    #[test]
    fn an_open_network_policy_keeps_the_owner_model_and_proxy_ports_free() {
        let (_temp, sandbox) = project(SandboxNetwork::Open);
        let sandbox = sandbox.keep_ports(&[8080, 11434, 1234]).unwrap();
        let grants = Grants {
            read: Vec::new(),
            write: Vec::new(),
        };
        let policy = landlock_policy(&grants, &sandbox);
        assert!(!policy.limit_network);
        let mut expected = vec![1234, 1455, 8080, 11434];
        expected.extend(PROXY_PORTS);
        expected.extend(crate::sandbox::model_proxy::RELAY_PORTS);
        assert_eq!(policy.keep_free, expected);
        assert_eq!(policy.keep_free.len(), 4 + 256);
        assert_eq!(
            HelperRequest::parse(
                policy
                    .args()
                    .into_iter()
                    .chain(["--".into(), "/bin/true".into()])
                    .collect()
            ),
            Ok(HelperRequest::Run {
                policy,
                program: PathBuf::from("/bin/true"),
                args: Vec::new(),
            })
        );
    }

    #[test]
    fn the_ports_linux_hands_out_are_read_as_two_rising_numbers_or_not_at_all() {
        assert_eq!(picked_ports("32768\t60999\n"), Some(32768..=60999));
        assert_eq!(picked_ports("1024 65535"), Some(1024..=65535));
        assert_eq!(picked_ports("40000 40000"), Some(40000..=40000));
        for text in [
            "",
            "32768",
            "60999 32768",
            "1 2 3",
            "low high",
            "32768 70000",
            "-1 5",
        ] {
            assert_eq!(picked_ports(text), None, "{text:?}");
        }
    }

    #[test]
    fn an_open_network_is_refused_when_linux_could_hand_out_a_kept_port() {
        let (_temp, sandbox) = project(SandboxNetwork::Open);
        let usual = PICKED_PORTS_START..=60_999;
        let kept = kept_free(&sandbox.clone().keep_ports(&[8080, 18_080]).unwrap());
        assert_eq!(
            kept_outside_picked(&kept, Some(&usual)),
            Ok(()),
            "the owner ports and the whole proxy block lie below the ports Linux hands out"
        );
        let inside = kept_free(&sandbox.clone().keep_ports(&[40_000]).unwrap());
        let reason = kept_outside_picked(&inside, Some(&usual)).unwrap_err();
        assert!(reason.starts_with("port 40000 is kept free"), "{reason}");
        assert!(reason.contains("ports 32768 to 60999"), "{reason}");
        let moved = kept_outside_picked(&kept, Some(&(1024..=65_535))).unwrap_err();
        assert!(moved.starts_with("port 1455 is kept free"), "{moved}");
        assert_eq!(
            kept_outside_picked(&kept, None),
            Err(PICKED_PORTS_UNREADABLE.to_owned()),
            "a range that cannot be read starts no open-network agent"
        );
        assert!(kept_outside_picked(&[], None).is_err());
    }

    #[test]
    fn a_kept_port_inside_this_machines_picked_ports_refuses_every_landlock_launch_only() {
        let picked = read_picked_ports().expect("Linux test machines show their port range");
        let inside = (*picked.start()).max(1);
        let helper = std::env::current_exe().unwrap();
        let landlock = || LinuxBackend::Landlock {
            helper: helper.clone(),
        };
        let (_closed, sandbox) = project(SandboxNetwork::Only(Vec::new()));
        let closed = sandbox.keep_ports(&[inside]).unwrap();
        let bubblewrap = LinuxBackend::Bubblewrap {
            bwrap: helper.clone(),
            helper: helper.clone(),
        };
        assert!(
            LinuxSandbox::new(closed, bubblewrap).is_ok(),
            "bubblewrap starts a closed agent in a network of its own"
        );
        for network in [
            SandboxNetwork::Open,
            SandboxNetwork::Only(Vec::new()),
            SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
        ] {
            let (_kept, sandbox) = project(network.clone());
            let sandbox = sandbox.keep_ports(&[inside]).unwrap();
            match LinuxSandbox::new(sandbox, landlock()) {
                Err(SandboxError::Unavailable(reason)) => {
                    assert!(reason.contains(&format!("port {inside} ")), "{reason}");
                }
                other => panic!(
                    "a kept port Linux could pick must refuse the launch on {network:?}: {other:?}"
                ),
            }
        }
    }

    #[test]
    fn a_limited_network_policy_names_the_kept_ports_for_the_helper_to_check() {
        let (_temp, sandbox) = project(SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]));
        let sandbox = sandbox.keep_ports(&[8080]).unwrap();
        let grants = Grants {
            read: Vec::new(),
            write: Vec::new(),
        };
        let policy = landlock_policy(&grants, &sandbox);
        assert!(policy.limit_network);
        let mut expected = vec![1455, 8080, 11434];
        expected.extend(PROXY_PORTS);
        expected.extend(crate::sandbox::model_proxy::RELAY_PORTS);
        assert_eq!(policy.keep_free, expected);
        assert_eq!(
            HelperRequest::parse(
                policy
                    .args()
                    .into_iter()
                    .chain(["--".into(), "/bin/true".into()])
                    .collect()
            ),
            Ok(HelperRequest::Run {
                policy,
                program: PathBuf::from("/bin/true"),
                args: Vec::new(),
            })
        );
    }

    #[test]
    fn the_helper_refuses_kept_ports_that_do_not_fit_its_policy() {
        let parse = |args: &[&str]| HelperRequest::parse(args.iter().map(OsString::from).collect());
        assert_eq!(
            parse(&[PROBE, KEEP_FREE, "1455"]),
            Ok(HelperRequest::Probe {
                limit_network: false,
                open_sockets: false,
                keep_free: true,
            })
        );
        assert_eq!(
            parse(&[PROBE]),
            Ok(HelperRequest::Probe {
                limit_network: false,
                open_sockets: false,
                keep_free: false,
            })
        );
        assert_eq!(
            parse(&[PROBE, LIMIT_NETWORK, KEEP_FREE, "1455"]),
            Ok(HelperRequest::Probe {
                limit_network: true,
                open_sockets: false,
                keep_free: true,
            })
        );
        assert_eq!(
            parse(&[LIMIT_NETWORK, KEEP_FREE, "1455", "--", "/bin/true"]),
            Ok(HelperRequest::Run {
                policy: LandlockPolicy {
                    limit_network: true,
                    keep_free: vec![1455],
                    ..LandlockPolicy::default()
                },
                program: PathBuf::from("/bin/true"),
                args: Vec::new(),
            })
        );
        for refused in [
            &[KEEP_FREE, "0", "--", "/bin/true"][..],
            &[KEEP_FREE, "--", "/bin/true"],
            &[PROBE, KEEP_FREE],
        ] {
            assert!(parse(refused).is_err(), "{refused:?}");
        }
        let many: Vec<OsString> = (1..=KEEP_FREE_MAX + 1)
            .flat_map(|port| [OsString::from(KEEP_FREE), OsString::from(port.to_string())])
            .chain(["--".into(), "/bin/true".into()])
            .collect();
        assert_eq!(
            HelperRequest::parse(many),
            Err("too many kept ports".into())
        );
    }

    #[test]
    fn bubblewrap_starts_no_open_network_agent_since_it_keeps_no_port_free() {
        let bwrap = std::env::current_exe().unwrap();
        let backend = || LinuxBackend::Bubblewrap {
            bwrap: bwrap.clone(),
            helper: bwrap.clone(),
        };
        let (_open, sandbox) = project(SandboxNetwork::Open);
        match LinuxSandbox::new(sandbox, backend()) {
            Err(SandboxError::Unavailable(reason)) => {
                assert_eq!(reason, LINUX_OPEN_NETWORK_NEEDS_LANDLOCK);
            }
            other => panic!("an open network on bubblewrap must be refused: {other:?}"),
        }
        let (_closed, sandbox) = project(SandboxNetwork::Only(Vec::new()));
        assert!(LinuxSandbox::new(sandbox, backend()).is_ok());
        let (_absent, sandbox) = project(SandboxNetwork::Open);
        match LinuxSandbox::detect(sandbox, Path::new("/nonexistent/crazyagent-landlock")) {
            Err(SandboxError::Unavailable(reason)) => {
                assert_eq!(reason, LINUX_OPEN_NETWORK_NEEDS_LANDLOCK);
            }
            other => panic!("an open network without the helper must be refused: {other:?}"),
        }
    }

    #[test]
    fn guarded_tcp_ports_refuse_the_stream_sockets_that_are_not_plain_tcp() {
        assert_eq!(refused_sockets(false, false).unwrap().len(), 1);
        assert_eq!(refused_sockets(false, true).unwrap().len(), 4);
        assert_eq!(
            refused_sockets(true, true).unwrap().len(),
            refused_sockets(true, false).unwrap().len()
        );
    }

    #[test]
    fn a_closed_bubblewrap_agent_gets_no_name_resolver_and_starts_behind_the_socket_filter() {
        let grants = Grants {
            read: Vec::new(),
            write: Vec::new(),
        };
        let command = bwrap_command(
            Path::new(BWRAP_PATHS[0]),
            &helper(),
            &grants,
            Path::new("/tmp"),
            &SandboxNetwork::Only(Vec::new()),
        );
        let args: Vec<&OsStr> = command.get_args().collect();
        assert!(
            !args.contains(&OsStr::new("/run/systemd/resolve")),
            "{args:?}"
        );
        let [split, helper, filter] = args[args.len() - 3..] else {
            panic!("{args:?}");
        };
        assert_eq!(split, "--", "{args:?}");
        assert_eq!(
            Path::new(helper).file_name(),
            Some(OsStr::new(HELPER_NAME)),
            "{args:?}"
        );
        assert_eq!(filter, "--filter-sockets", "{args:?}");
        let bound = args
            .windows(3)
            .any(|three| three[0] == "--ro-bind" && three[1] == helper && three[2] == helper);
        assert!(bound, "the helper is bound into the namespace: {args:?}");
    }

    #[test]
    fn the_socket_filter_request_takes_no_policy_and_names_the_harness() {
        let parse =
            |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
        assert_eq!(
            parse(&[FILTER_SOCKETS, "--", "/bin/true", "-x"]),
            Ok(HelperRequest::Filter {
                program: PathBuf::from("/bin/true"),
                args: vec![OsString::from("-x")],
            })
        );
        assert!(parse(&[FILTER_SOCKETS, READ, "/usr", "--", "/bin/true"]).is_err());
        assert!(parse(&[FILTER_SOCKETS, "--", "bin/true"]).is_err());
        assert!(parse(&[PROBE, FILTER_SOCKETS]).is_err());
    }

    #[test]
    fn the_scope_question_stands_alone() {
        let parse =
            |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
        assert_eq!(parse(&[SCOPED]), Ok(HelperRequest::Scoped));
        assert!(parse(&[SCOPED, PROBE]).is_err());
        assert!(parse(&[PROBE, SCOPED]).is_err());
        assert!(parse(&[SCOPED, "--", "/bin/true"]).is_err());
        assert!(parse(&[READ, "/usr", SCOPED, "--", "/bin/true"]).is_err());
    }

    #[test]
    fn a_nested_agent_starts_through_the_helper_then_bubblewrap_then_the_helper_again() {
        let (_temp, sandbox) = project(SandboxNetwork::Open);
        let cwd = sandbox.project_root().to_path_buf();
        let helper = std::env::current_exe().unwrap();
        let nested = LinuxSandbox::new(
            sandbox,
            LinuxBackend::Nested {
                bwrap: helper.clone(),
                helper: helper.clone(),
            },
        )
        .unwrap();
        let mut harness = Command::new("/bin/sh");
        harness.args(["-c", "true"]).current_dir(&cwd);
        let command = nested.wrap(&harness).unwrap();
        assert_eq!(command.get_program(), helper.as_os_str());
        let args: Vec<&OsStr> = command.get_args().collect();
        let mut head = vec![OsStr::new(NEST), OsStr::new("--"), helper.as_os_str()];
        head.extend(NEST_ARGS.map(OsStr::new));
        head.extend([
            OsStr::new("--chdir"),
            cwd.as_os_str(),
            OsStr::new("--"),
            helper.as_os_str(),
        ]);
        assert_eq!(args.get(..head.len()), Some(&head[..]), "{args:?}");
        let [policy @ .., split, program, flag, script] = &args[head.len()..] else {
            panic!("{args:?}");
        };
        assert_eq!(*split, "--", "{args:?}");
        assert_eq!(*program, "/bin/sh", "{args:?}");
        assert_eq!(*flag, "-c", "{args:?}");
        assert_eq!(*script, "true", "{args:?}");
        let grants = Grants::of(nested.sandbox()).unwrap();
        let mut expected = landlock_policy(&grants, nested.sandbox());
        expected.nested = true;
        let written: Vec<OsString> = policy.iter().map(|arg| arg.to_os_string()).collect();
        assert_eq!(written, expected.args(), "{args:?}");
        assert!(written.contains(&OsString::from(NESTED)), "{args:?}");
        for absent in ["--new-session", "--unshare-net"] {
            assert!(!args.contains(&OsStr::new(absent)), "{absent}: {args:?}");
        }
        assert_eq!(args.iter().filter(|arg| **arg == NEST).count(), 1);
    }

    #[test]
    fn only_a_loopback_only_tcp_only_agent_with_ports_from_1024_up_is_bridged() {
        use SandboxRemote::{AnyHost, Loopback};
        let bridged = |remotes: &[SandboxRemote], sockets: LinuxSockets| {
            let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
            sandbox.limit_sockets(sockets).bridged_ports()
        };
        let default = LinuxSockets::TcpOnly;
        assert_eq!(
            bridged(&[Loopback(11434), Loopback(8080)], default),
            Some(vec![8080, 11434])
        );
        assert_eq!(bridged(&[Loopback(1024)], default), Some(vec![1024]));
        assert_eq!(bridged(&[Loopback(8080), AnyHost(443)], default), None);
        assert_eq!(bridged(&[Loopback(8080), Loopback(1023)], default), None);
        assert_eq!(bridged(&[], default), None);
        assert_eq!(bridged(&[Loopback(8080)], LinuxSockets::TcpPorts), None);
        let (_temp, open) = project(SandboxNetwork::Open);
        assert_eq!(open.bridged_ports(), None);
    }

    #[test]
    fn only_a_tcp_only_agent_with_a_port_on_any_host_and_loopback_ports_from_1024_up_is_routed()
    {
        use SandboxRemote::{AnyHost, Loopback};
        let routed = |remotes: &[SandboxRemote], sockets: LinuxSockets| {
            let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
            let sandbox = sandbox.limit_sockets(sockets);
            let routed = sandbox.routed();
            assert!(routed.is_none() || sandbox.bridged_ports().is_none());
            routed.map(|routed| (routed.any, routed.bridged))
        };
        let default = LinuxSockets::TcpOnly;
        assert_eq!(routed(&[AnyHost(443)], default), Some((vec![443], vec![])));
        assert_eq!(
            routed(&[Loopback(18080), AnyHost(8443), AnyHost(443)], default),
            Some((vec![443, 8443], vec![18080]))
        );
        assert_eq!(
            routed(&[AnyHost(443), Loopback(1024)], default),
            Some((vec![443], vec![1024]))
        );
        assert_eq!(routed(&[AnyHost(443), Loopback(1023)], default), None);
        assert_eq!(routed(&[AnyHost(443), Loopback(53)], default), None);
        assert_eq!(routed(&[Loopback(8080)], default), None, "the bridge's case");
        assert_eq!(routed(&[], default), None);
        assert_eq!(routed(&[AnyHost(443)], LinuxSockets::TcpPorts), None);
        assert_eq!(routed(&[AnyHost(443)], LinuxSockets::Refuse), None);
        let (_temp, open) = project(SandboxNetwork::Open);
        assert_eq!(open.routed(), None);
    }

    #[test]
    fn a_routed_agent_starts_with_its_route_flags_first_and_a_network_of_its_own() {
        use bridge::{BRIDGE, UNSHARE_NET};
        let (_temp, sandbox) = project(SandboxNetwork::Only(vec![
            SandboxRemote::AnyHost(443),
            SandboxRemote::Loopback(18080),
        ]));
        let cwd = sandbox.project_root().to_path_buf();
        let grants = Grants::of(&sandbox).unwrap();
        let policy = LandlockPolicy {
            nested: true,
            ..landlock_policy(&grants, &sandbox)
        };
        let bwrap = Path::new(BWRAP_PATHS[0]);
        let helper = helper();
        let words = |bridged: &[u16], routes: &[u16]| -> Vec<OsString> {
            nest_command(bwrap, &helper, &policy, &cwd, bridged, routes)
                .get_args()
                .map(OsStr::to_os_string)
                .collect()
        };
        let expected = |head: &[&str]| -> Vec<OsString> {
            let mut expected: Vec<OsString> = head.iter().map(OsString::from).collect();
            expected.extend([NEST, "--"].map(OsString::from));
            expected.push(bwrap.into());
            expected.extend(NEST_ARGS.map(OsString::from));
            expected.extend([UNSHARE_NET, "--chdir"].map(OsString::from));
            expected.push(cwd.clone().into());
            expected.push("--".into());
            expected.push(helper.clone().into());
            expected.extend(policy.args());
            expected
        };
        let both = [egress::ROUTE, "443", BRIDGE, "18080"];
        assert_eq!(words(&[18080], &[443]), expected(&both));
        let routes = [egress::ROUTE, "443", egress::ROUTE, "8443"];
        assert_eq!(words(&[], &[443, 8443]), expected(&routes));
        for routed in [words(&[18080], &[443]), words(&[], &[443])] {
            for flag in egress::DESCRIPTOR_FLAGS {
                assert!(!routed.iter().any(|word| word == flag), "{flag}: {routed:?}");
            }
        }
    }

    #[test]
    fn route_flags_lead_only_a_nest_that_unshares_the_network_and_no_caller_names_its_descriptors()
    {
        use bridge::{BRIDGE, UNSHARE_NET};
        let nest = |head: &[&str], options: &[&str]| -> Vec<OsString> {
            let mut args: Vec<OsString> = head.iter().map(OsString::from).collect();
            args.extend([NEST, "--", BWRAP_PATHS[0]].map(OsString::from));
            args.extend(NEST_ARGS.map(OsString::from));
            args.extend(options.iter().map(OsString::from));
            args.push("--".into());
            args.push(helper().into());
            args.push(PROBE.into());
            args
        };
        let read = |args: Vec<OsString>| {
            read_request(args).map(|(routes, bridge, _)| (routes, bridge.map(|held| held.ports)))
        };
        let route = [egress::ROUTE, "443"];
        let bridged = [BRIDGE, "18080"];
        assert_eq!(read(nest(&route, &[UNSHARE_NET])), Ok((vec![443], None)));
        let both = [egress::ROUTE, "443", egress::ROUTE, "8443", BRIDGE, "18080"];
        assert_eq!(
            read(nest(&both, &[UNSHARE_NET])),
            Ok((vec![443, 8443], Some(vec![18080])))
        );
        assert_eq!(read(nest(&[], &[])), Ok((Vec::new(), None)));
        assert!(read(nest(&route, &[])).is_err(), "a route needs --unshare-net");
        assert!(read(nest(&[], &[UNSHARE_NET])).is_err(), "--unshare-net needs a port");
        let mut probe: Vec<OsString> = route.iter().map(OsString::from).collect();
        probe.push(PROBE.into());
        assert!(read(probe).is_err(), "a route leads a nest only");
        for options in [
            &[UNSHARE_NET, "--info-fd", "5"][..],
            &[UNSHARE_NET, "--block-fd", "6"],
            &[UNSHARE_NET, "--ro-bind-data", "7", "/etc/resolv.conf"],
            &["--info-fd", "5", UNSHARE_NET],
        ] {
            assert!(read(nest(&route, options)).is_err(), "{options:?}");
            assert!(read(nest(&bridged, options)).is_err(), "{options:?}");
        }
    }

    #[test]
    fn a_slirp4netns_or_bubblewrap_that_others_may_write_to_is_never_run() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let moded = |mode: u32| {
            let path = root.join(format!("slirp4netns-{mode:o}"));
            fs::write(&path, b"").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            path
        };
        assert!(trusted(&moded(0o755)));
        assert!(trusted(&moded(0o700)));
        for refused in [0o757, 0o775, 0o777, 0o722] {
            assert!(!trusted(&moded(refused)), "{refused:o}");
        }
    }

    #[test]
    fn a_set_user_or_set_group_id_bubblewrap_is_never_nested_under() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let moded = |name: &str, mode: u32| {
            let path = root.join(name);
            fs::write(&path, b"").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            path
        };
        let plain = moded("plain", 0o755);
        let marked = [moded("setuid", 0o4755), moded("setgid", 0o2755)];
        let agreeing = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|folder| folder.join("true"))
            .find(|path| path.is_file())
            .unwrap();
        let probe = [OsString::from(PROBE)];
        assert!(nests(&plain, &agreeing, &probe), "the control");
        assert!(trusted(&plain));
        assert!(trusted(Path::new("/bin/sh")), "a program root owns");
        assert!(!trusted(&root.join("absent")));
        assert!(!trusted(&root), "a folder");
        for bwrap in &marked {
            assert!(!trusted(bwrap), "{}", bwrap.display());
            let (_temp, sandbox) = project(SandboxNetwork::Open);
            let backend = LinuxBackend::Nested {
                bwrap: bwrap.clone(),
                helper: plain.clone(),
            };
            let refused = LinuxSandbox::new(sandbox, backend).is_err();
            assert!(refused, "{}", bwrap.display());
            assert!(!nests(bwrap, &agreeing, &probe), "{}", bwrap.display());
        }
    }

    #[test]
    fn an_agent_kept_on_this_network_by_a_low_port_names_the_lowest_one() {
        use SandboxRemote::{AnyHost, Loopback};
        let low = |remotes: &[SandboxRemote], sockets: LinuxSockets| {
            let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
            let sandbox = sandbox.limit_sockets(sockets);
            assert!(sandbox.unbridged_port().is_none() || sandbox.bridged_ports().is_none());
            sandbox.unbridged_port()
        };
        let default = LinuxSockets::TcpOnly;
        assert_eq!(
            low(&[Loopback(8080), Loopback(443), Loopback(80)], default),
            Some(80)
        );
        assert_eq!(low(&[Loopback(1023)], default), Some(1023));
        assert_eq!(low(&[Loopback(1024)], default), None, "bridged");
        assert_eq!(
            low(&[Loopback(80), AnyHost(443)], default),
            None,
            "a host port"
        );
        assert_eq!(low(&[], default), None);
        assert_eq!(
            low(&[Loopback(80)], LinuxSockets::TcpPorts),
            None,
            "the setting"
        );
        let (_temp, open) = project(SandboxNetwork::Open);
        assert_eq!(open.unbridged_port(), None);
    }

    #[test]
    fn a_project_may_hold_another_git_repository_below_its_top_or_where_none_is_ruled_out() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(project.join(".git/hooks")).unwrap();
        fs::write(project.join(".git/config"), "").unwrap();
        fs::create_dir_all(project.join("src/deep")).unwrap();
        fs::write(project.join("src/deep/.gitignore"), "").unwrap();
        fs::write(project.join("a.git"), "").unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir_all(outside.join(".git")).unwrap();
        symlink(&outside, project.join("linked")).unwrap();
        assert!(!holds_other_git(&project), "its own .git, and a link is not followed");
        let found = |make: &dyn Fn(&Path), undo: &dyn Fn(&Path)| {
            make(&project);
            let held = holds_other_git(&project);
            undo(&project);
            assert!(!holds_other_git(&project));
            held
        };
        for name in ["vendor/lib/.git", "vendor/.GIT", "vendor/.git.", ".Git"] {
            let folder = |project: &Path| fs::create_dir_all(project.join(name)).unwrap();
            let gone = |project: &Path| fs::remove_dir(project.join(name)).unwrap();
            assert!(found(&folder, &gone), "{name}");
        }
        fs::create_dir_all(project.join("vendor/lib")).unwrap();
        let link = |project: &Path| {
            fs::write(project.join("vendor/lib/.git"), "gitdir: x").unwrap();
        };
        let unlink = |project: &Path| fs::remove_file(project.join("vendor/lib/.git")).unwrap();
        assert!(found(&link, &unlink), "a .git file is rewritten without a create");
        let pointer = |project: &Path| symlink(&outside, project.join("vendor/.git")).unwrap();
        let unpoint = |project: &Path| fs::remove_file(project.join("vendor/.git")).unwrap();
        assert!(found(&pointer, &unpoint), "a link named .git counts");
        let mode = |project: &Path, bits| {
            let permissions = fs::Permissions::from_mode(bits);
            fs::set_permissions(project.join("src/deep"), permissions).unwrap();
        };
        let locked = |project: &Path| mode(project, 0o000);
        let unlocked = |project: &Path| mode(project, 0o755);
        if !rustix::process::geteuid().is_root() {
            assert!(found(&locked, &unlocked), "a folder it cannot read");
        }
        let many = |project: &Path| {
            fs::create_dir(project.join("many")).unwrap();
            for index in 0..GIT_SEARCH_ENTRIES_MAX {
                fs::write(project.join("many").join(index.to_string()), "").unwrap();
            }
        };
        let few = |project: &Path| fs::remove_dir_all(project.join("many")).unwrap();
        assert!(found(&many, &few), "more entries than it reads");
        assert!(holds_other_git(&temp.path().join("missing")), "no folder to read");
    }

    #[test]
    fn a_launch_left_on_this_network_by_a_low_port_says_so_with_the_port() {
        use super::super::policy::AgentNetwork;
        use super::super::{LinuxHost, linux_unbridged_line, linux_unbridged_notice_on};
        use SandboxRemote::{AnyHost, Loopback};
        let host = |nested| {
            Some(LinuxHost {
                nested,
                scoped: false,
                supervised: false,
                root: false,
                armored: false,
                routed: false,
            })
        };
        let launched = |remotes: &[SandboxRemote]| {
            let (_temp, sandbox) = project(SandboxNetwork::Only(remotes.to_vec()));
            sandbox
        };
        let said = |nested, sandbox: &ProjectSandbox| {
            linux_unbridged_notice_on(host(nested), AgentNetwork::None, sandbox)
        };
        let low = launched(&[Loopback(11434), Loopback(80)]);
        assert_eq!(said(true, &low), Some(linux_unbridged_line(80)));
        assert!(said(true, &low).unwrap().contains("port 80 is below 1024"));
        assert_eq!(said(false, &low), None, "no nest");
        assert_eq!(linux_unbridged_notice_on(None, AgentNetwork::None, &low), None);
        assert_eq!(said(true, &launched(&[Loopback(11434)])), None, "bridged");
        let (_temp, open) = project(SandboxNetwork::Open);
        assert_eq!(said(true, &open), None);
        let plain = Some(super::super::LINUX_PORTS_ANY_HOST_LINE.to_owned());
        let hosted = launched(&[AnyHost(443), Loopback(8080)]);
        assert_eq!(said(true, &hosted), plain, "a model on a host, tools off");
        assert_eq!(said(false, &hosted), None, "the page says it without a nest");
        let ports = launched(&[Loopback(8080)]).limit_sockets(LinuxSockets::TcpPorts);
        assert_eq!(said(true, &ports), plain);
        assert_eq!(said(true, &launched(&[AnyHost(443)])), None, "no port here");
        assert_eq!(said(true, &launched(&[])), None);
        for choice in [AgentNetwork::Https, AgentNetwork::Open] {
            let said = |sandbox| linux_unbridged_notice_on(host(true), choice, sandbox);
            assert_eq!(said(&hosted), None, "{choice:?}: the page says it");
            assert_eq!(said(&ports), None, "{choice:?}: the page says it");
            assert_eq!(said(&low), Some(linux_unbridged_line(80)), "{choice:?}");
        }
    }

    #[test]
    fn the_bubblewrap_a_nest_starts_is_checked_as_the_file_it_opened() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let moded = |name: &str, mode: u32| {
            let path = root.join(name);
            fs::write(&path, b"").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            path
        };
        let plain = moded("plain", 0o755);
        let unread = moded("unread", 0o711);
        let setuid = moded("setuid", 0o4755);
        let linked = root.join("linked");
        std::os::unix::fs::symlink(&setuid, &linked).unwrap();
        for held in [&plain, &unread] {
            let file = trusted_file(held).unwrap_or_else(|| panic!("{}", held.display()));
            let opened = file.metadata().unwrap();
            let named = fs::metadata(held).unwrap();
            assert_eq!((opened.dev(), opened.ino()), (named.dev(), named.ino()));
        }
        for refused in [&setuid, &moded("setgid", 0o2755), &linked, &root] {
            assert!(trusted_file(refused).is_none(), "{}", refused.display());
            assert!(!trusted(refused), "{}", refused.display());
        }
        assert!(trusted_file(&root.join("absent")).is_none());
        let file = trusted_file(&plain).unwrap();
        fs::set_permissions(&plain, fs::Permissions::from_mode(0o4755)).unwrap();
        let mode = file.metadata().unwrap().mode();
        assert_ne!(mode & SET_ID_BITS, 0, "the open file is the one a swap changes");
    }

    fn nested_words(network: SandboxNetwork) -> (PathBuf, PathBuf, Vec<OsString>) {
        let (_temp, sandbox) = project(network);
        let cwd = sandbox.project_root().to_path_buf();
        let helper = std::env::current_exe().unwrap();
        let backend = LinuxBackend::Nested {
            bwrap: helper.clone(),
            helper: helper.clone(),
        };
        let nested = LinuxSandbox::new(sandbox, backend).unwrap();
        let mut harness = Command::new("/bin/sh");
        harness.args(["-c", "true"]).current_dir(&cwd);
        let command = nested.wrap(&harness).unwrap();
        let words = command.get_args().map(OsStr::to_os_string).collect();
        (helper, cwd, words)
    }

    #[test]
    fn a_loopback_only_nested_agent_starts_with_a_network_of_its_own_and_its_ports_bridged() {
        use bridge::{BRIDGE, BRIDGE_FD, UNSHARE_NET};
        let loopback = [SandboxRemote::Loopback(11434), SandboxRemote::Loopback(8080)];
        let (helper, cwd, words) = nested_words(SandboxNetwork::Only(loopback.to_vec()));
        let mut head: Vec<OsString> = [BRIDGE, "8080", BRIDGE, "11434", NEST, "--"]
            .map(OsString::from)
            .to_vec();
        head.push(helper.clone().into());
        head.extend(NEST_ARGS.map(OsString::from));
        head.extend([UNSHARE_NET, "--chdir"].map(OsString::from));
        head.push(cwd.into());
        head.push("--".into());
        head.push(helper.into());
        assert_eq!(words.get(..head.len()), Some(&head[..]), "{words:?}");
        let inside = &words[head.len()..];
        let written = |word: &OsString| word == BRIDGE || word == BRIDGE_FD;
        assert!(!inside.iter().any(written), "the nest writes them: {words:?}");
        let (bridge, rest) = Bridge::leading(words.clone()).unwrap();
        let asked = Bridge {
            fd: None,
            ports: vec![8080, 11434],
        };
        assert_eq!(bridge, Some(asked));
        assert_eq!(rest.first(), Some(&OsString::from(NEST)));
        let low = [SandboxRemote::Loopback(8080), SandboxRemote::Loopback(443)];
        for network in [
            SandboxNetwork::Only(low.to_vec()),
            SandboxNetwork::Only(Vec::new()),
            SandboxNetwork::Open,
        ] {
            let (_, _, words) = nested_words(network.clone());
            assert_eq!(words.first(), Some(&OsString::from(NEST)), "{network:?}");
            assert!(!words.iter().any(|word| word == UNSHARE_NET), "{network:?}");
        }
    }

    #[test]
    fn a_nested_agent_may_have_an_open_network_and_keeps_the_kept_port_check() {
        let picked = read_picked_ports().expect("Linux test machines show their port range");
        let inside = (*picked.start()).max(1);
        let helper = std::env::current_exe().unwrap();
        let nested = || LinuxBackend::Nested {
            bwrap: helper.clone(),
            helper: helper.clone(),
        };
        let (_open, sandbox) = project(SandboxNetwork::Open);
        assert!(
            LinuxSandbox::new(sandbox, nested()).is_ok(),
            "a nested agent is a Landlock agent, which keeps ports free on an open network"
        );
        let (_kept, sandbox) = project(SandboxNetwork::Open);
        match LinuxSandbox::new(sandbox.keep_ports(&[inside]).unwrap(), nested()) {
            Err(SandboxError::Unavailable(reason)) => {
                assert!(reason.contains(&format!("port {inside} ")), "{reason}");
            }
            other => panic!("a kept port Linux could pick must refuse the nest: {other:?}"),
        }
    }

    #[test]
    fn the_helper_starts_only_a_fixed_bubblewrap_for_a_nest_and_takes_no_policy_with_it() {
        let parse =
            |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
        let nest = |bwrap: &str, tail: &[&str]| {
            let mut words = vec![NEST, "--", bwrap];
            words.extend(NEST_ARGS);
            words.extend(tail);
            parse(&words)
        };
        let tail = [
            "--chdir",
            "/work",
            "--",
            "/opt/helper",
            NESTED,
            "--",
            "/bin/true",
        ];
        let passed: Vec<OsString> = NEST_ARGS.iter().chain(&tail).map(OsString::from).collect();
        for bwrap in BWRAP_PATHS {
            assert_eq!(
                nest(bwrap, &tail),
                Ok(HelperRequest::Nest {
                    bwrap: PathBuf::from(bwrap),
                    args: passed.clone(),
                })
            );
        }
        assert!(nest("/tmp/bwrap", &tail).is_err());
        assert!(nest("usr/bin/bwrap", &tail).is_err());
        assert!(
            nest(BWRAP_PATHS[0], &[]).is_err(),
            "nothing to start inside"
        );
        let own = [NEST, "--", BWRAP_PATHS[0], "--bind", "/", "/", "--", "/x"];
        assert!(parse(&own).is_err(), "only the fixed arguments");
        assert!(parse(&[NEST, READ, "/usr", "--", BWRAP_PATHS[0]]).is_err());
        assert!(parse(&[NEST, PROBE]).is_err());
        assert!(parse(&[PROBE, NEST]).is_err());
        assert!(parse(&[NEST]).is_err());
    }

    #[test]
    fn a_nested_policy_reads_back_from_its_own_arguments() {
        let policy = LandlockPolicy {
            read: vec![PathBuf::from("/usr")],
            nested: true,
            ..LandlockPolicy::default()
        };
        let mut words = policy.args();
        assert_eq!(words.last(), Some(&OsString::from(NESTED)));
        words.extend(["--".into(), "/bin/true".into()]);
        assert_eq!(
            HelperRequest::parse(words),
            Ok(HelperRequest::Run {
                policy,
                program: PathBuf::from("/bin/true"),
                args: Vec::new(),
            })
        );
        let parse =
            |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
        assert!(parse(&[NESTED, DIE_WITH, "4242", "--", "/bin/true"]).is_err());
        assert!(parse(&[PROBE, NESTED]).is_err());
        assert!(parse(&[FILTER_SOCKETS, NESTED, "--", "/bin/true"]).is_err());
    }

    fn guarded() -> LandlockPolicy {
        LandlockPolicy {
            read: vec![PathBuf::from("/usr")],
            connect: vec![443, 8080],
            limit_network: true,
            guard: Guard {
                loopback: vec![8080],
                git: Some(GitOwn {
                    trees: vec![PathBuf::from("/p")],
                    kids: vec![PathBuf::from("/p/worktrees")],
                    exact: vec![PathBuf::from("/p/modules/.git")],
                }),
                signals: true,
            },
            ..LandlockPolicy::default()
        }
    }

    #[test]
    fn a_guarded_policy_reads_back_from_its_own_arguments() {
        let policy = guarded();
        let mut words = policy.args();
        let guard: Vec<OsString> = [
            GUARD_LOOPBACK,
            "8080",
            GUARD_GIT,
            OWN_TREE,
            "/p",
            OWN_KIDS,
            "/p/worktrees",
            OWN_GIT,
            "/p/modules/.git",
            GUARD_SIGNALS,
        ]
        .iter()
        .map(OsString::from)
        .collect();
        assert!(words.ends_with(&guard), "{words:?}");
        words.extend(["--".into(), "/bin/true".into()]);
        assert_eq!(
            HelperRequest::parse(words),
            Ok(HelperRequest::Run {
                policy,
                program: PathBuf::from("/bin/true"),
                args: Vec::new(),
            })
        );
        assert!(LandlockPolicy::default().guard.args().is_empty());
    }

    #[test]
    fn each_guard_stands_alone_and_a_guard_that_does_not_fit_its_policy_is_refused() {
        let parse =
            |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
        let held = |words: &[&str]| match parse(words) {
            Ok(HelperRequest::Run { policy, .. }) => Some(policy.guard),
            _ => None,
        };
        let signals = Guard {
            signals: true,
            ..Guard::default()
        };
        let git = Guard {
            git: Some(GitOwn::default()),
            ..Guard::default()
        };
        assert_eq!(held(&[GUARD_SIGNALS, "--", "/bin/true"]), Some(signals));
        assert_eq!(held(&[GUARD_GIT, "--", "/bin/true"]), Some(git));
        assert_eq!(held(&["--", "/bin/true"]), Some(Guard::default()));
        for refused in [
            &[OWN_TREE, "/p", "--", "/bin/true"][..],
            &[OWN_KIDS, "/p", GUARD_GIT, "--", "/bin/true"],
            &[GUARD_GIT, OWN_GIT, "p/.git", "--", "/bin/true"],
            &[GUARD_GIT, GUARD_GIT, "--", "/bin/true"],
            &[GUARD_LOOPBACK, "8080", "--", "/bin/true"],
            &[LIMIT_NETWORK, GUARD_LOOPBACK, "8080", "--", "/bin/true"],
            &[GUARD_SIGNALS, DIE_WITH, "4242", "--", "/bin/true"],
            &[PROBE, GUARD_SIGNALS],
            &[PROBE, GUARD_GIT],
            &[FILTER_SOCKETS, GUARD_SIGNALS, "--", "/bin/true"],
        ] {
            assert!(parse(refused).is_err(), "{refused:?}");
        }
        let limited = [LIMIT_NETWORK, CONNECT, "8080", GUARD_LOOPBACK];
        let ported = |more: &[&str]| [&limited[..], more, &["--", "/bin/true"][..]].concat();
        for refused in [&["0"][..], &["8081"], &["8080", OPEN_SOCKETS]] {
            assert!(parse(&ported(refused)).is_err(), "{refused:?}");
        }
        let fits = held(&ported(&["8080"]));
        assert_eq!(fits.map(|guard| guard.loopback), Some(vec![8080]));
    }

    #[test]
    fn a_guard_keeps_its_ports_on_this_machine_and_leaves_the_others_to_every_host() {
        let policy = guarded();
        let held = policy.guard.supervised(&policy.connect);
        let reach = held.reach.as_ref().expect("a guarded port is a guarded connect");
        assert_eq!(reach.any, [443]);
        assert_eq!(reach.loopback, [8080]);
        assert_eq!(held.git, policy.guard.git);
        assert!(held.signals);
        let alone = Guard {
            signals: true,
            ..Guard::default()
        };
        let held = alone.supervised(&policy.connect);
        assert_eq!((held.reach, held.git, held.signals), (None, None, true));
    }

    #[test]
    fn the_call_supervisor_probes_stand_alone() {
        let parse =
            |words: &[&str]| HelperRequest::parse(words.iter().map(OsString::from).collect());
        assert_eq!(
            parse(&[PROBE_SUPERVISOR, "/tmp/probe"]),
            Ok(HelperRequest::Supervisor {
                folder: PathBuf::from("/tmp/probe"),
            })
        );
        assert_eq!(
            parse(&[PROBE_SUPERVISED, "8080", "/tmp/probe"]),
            Ok(HelperRequest::Supervised {
                port: 8080,
                folder: PathBuf::from("/tmp/probe"),
            })
        );
        for refused in [
            &[PROBE_SUPERVISOR][..],
            &[PROBE_SUPERVISOR, "probe"],
            &[PROBE_SUPERVISOR, "/tmp/probe", "more"],
            &[PROBE_SUPERVISOR, GUARD_SIGNALS],
            &[PROBE_SUPERVISOR, "--", "/bin/true"],
            &[PROBE, PROBE_SUPERVISOR],
            &[PROBE_SUPERVISED],
            &[PROBE_SUPERVISED, "8080"],
            &[PROBE_SUPERVISED, "0", "/tmp/probe"],
            &[PROBE_SUPERVISED, "8080", "probe"],
            &[PROBE_SUPERVISED, "8080", "/tmp/probe", "more"],
        ] {
            assert!(parse(refused).is_err(), "{refused:?}");
        }
        let probe = HelperRequest::Supervisor {
            folder: PathBuf::from("/tmp/probe"),
        };
        assert_eq!(Bridge::fits(None, &probe), Ok(()));
    }

    /// Each supervisor probe gets a new folder only this user may open, under a name no
    /// stale, concurrent or planted folder takes, and none where the data folder refuses
    /// one (rv-secreview linux-t4c F1).
    #[test]
    fn each_supervisor_probe_gets_a_new_private_folder_under_the_engine_data_folder() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().canonicalize().unwrap();
        let first = private_folder(&data).expect("a folder in the data folder");
        let second = private_folder(&data).expect("a second folder beside it");
        assert_ne!(first, second);
        for folder in [&first, &second] {
            assert_eq!(folder.parent(), Some(data.as_path()));
            let name = folder.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with(PROBE_FOLDER_PREFIX), "{name}");
            assert!(private_empty(folder), "{}", folder.display());
        }
        let file = data.join("file");
        fs::write(&file, "").unwrap();
        assert_eq!(private_folder(&file), None);
        assert_eq!(private_folder(&data.join("missing")), None);
    }

    /// The helper runs its supervisor probe only in the private folder the engine made for
    /// it: a link to it, a folder others may open, one that holds anything, a file or no
    /// folder at all fails it (rv-secreview linux-t4c F1).
    #[test]
    fn the_supervisor_probe_takes_only_an_empty_folder_only_this_user_may_open() {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().canonicalize().unwrap();
        let private = data.join("private");
        fs::DirBuilder::new().mode(0o700).create(&private).unwrap();
        assert!(private_empty(&private));
        let linked = data.join("linked");
        symlink(&private, &linked).unwrap();
        assert!(!private_empty(&linked), "a link to it");
        fs::write(private.join("made"), "").unwrap();
        assert!(!private_empty(&private), "a folder that holds a file");
        fs::remove_file(private.join("made")).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!private_empty(&private), "a folder others may open");
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(private_empty(&private));
        let file = data.join("file");
        fs::write(&file, "").unwrap();
        assert!(!private_empty(&file), "a file");
        assert!(!private_empty(&data.join("missing")), "no folder");
    }

    /// The engine makes the probe's folder, hands it to the helper and removes it whatever
    /// the probe answered, so a failed or killed probe leaves nothing a later one trips on,
    /// two probes at once each get their own, and as root no probe runs (rv-secreview
    /// linux-t4c F1, rv-code4 linux-t4c F1).
    #[test]
    fn a_supervisor_probe_runs_in_its_own_folder_the_engine_removes_after() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let place = temp.path().canonicalize().unwrap();
        let data = place.join("data");
        fs::create_dir(&data).unwrap();
        let seen = place.join("seen");
        let fake = |name: &str, answer: u8| {
            let helper = place.join(name);
            let script = format!(
                "#!/bin/sh\n[ \"$1\" = {PROBE_SUPERVISOR} ] && [ -d \"$2\" ] || exit 9\n\
                 printf '%s\\n' \"$2\" >> '{}'\n: > \"$2/made\"\nexit {answer}\n",
                seen.display()
            );
            fs::write(&helper, script).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
            helper
        };
        let holding = fake("holding", 0);
        let failing = fake("failing", 1);
        assert!(supervises(&holding, None, &data, false));
        assert!(!supervises(&failing, None, &data, false));
        let both = std::thread::scope(|scope| {
            let one = scope.spawn(|| supervises(&holding, None, &data, false));
            let two = scope.spawn(|| supervises(&holding, None, &data, false));
            [one.join().unwrap(), two.join().unwrap()]
        });
        assert_eq!(both, [true, true]);
        let told = fs::read_to_string(&seen).unwrap();
        let folders: std::collections::BTreeSet<&str> = told.lines().collect();
        assert_eq!((told.lines().count(), folders.len()), (4, 4), "{told}");
        for folder in &folders {
            assert!(Path::new(folder).starts_with(&data), "{folder}");
        }
        assert_eq!(fs::read_dir(&data).unwrap().count(), 0, "every folder removed");
        assert!(!supervises(&holding, None, &data, true), "never as root");
        assert_eq!(fs::read_to_string(&seen).unwrap(), told, "as root no probe runs");
        assert_eq!(fs::read_dir(&data).unwrap().count(), 0, "and no folder is made");
    }

    /// AppArmor keeps unprivileged programs out of user namespaces only where its setting
    /// reads 1; any other value, or no file, leaves them free, so the AppArmor line shows
    /// only where it is the reason nothing nests (DEC-179 SBX-3).
    #[test]
    fn only_an_apparmor_setting_of_one_restricts_user_namespaces() {
        assert_eq!(
            APPARMOR_USERNS,
            "/proc/sys/kernel/apparmor_restrict_unprivileged_userns"
        );
        assert!(restricts(Some("1\n")));
        assert!(restricts(Some("1")));
        for free in [Some("0\n"), Some(""), Some("2\n"), Some("11\n"), Some("1 0"), None] {
            assert!(!restricts(free), "{free:?}");
        }
    }

    /// The helper refuses every variable that makes glibc load code, the dynamic loader's
    /// and gconv's, and no other: a name that only starts with `LD`, holds `LD_` later, or
    /// differs in case still passes (DEC-179 SBX-3 fold, rv-sa241 MED-1, rv-ds241 F2).
    #[test]
    fn the_helper_refuses_only_the_variables_that_load_code() {
        for name in [
            "LD_PRELOAD",
            "LD_AUDIT",
            "LD_LIBRARY_PATH",
            "LD_BIND_NOW",
            "GLIBC_TUNABLES",
            "GCONV_PATH",
        ] {
            assert!(loader_variable(OsStr::new(name)), "{name}");
        }
        for name in [
            "LDFLAGS",
            "OLD_PWD",
            "PATH",
            "ld_preload",
            "GLIBC_TUNABLES_X",
            "",
        ] {
            assert!(!loader_variable(OsStr::new(name)), "{name}");
        }
    }

    /// A launch whose own probe fails with the setting On runs without the supervisor and
    /// marks the kept host as running none, so the next Agent access page read shows the
    /// wider lines; nothing kept stays nothing (rv-secreview linux-t4c F2).
    #[test]
    fn a_launch_whose_probe_fails_marks_the_kept_host_unsupervised() {
        let host = LinuxHost {
            nested: true,
            scoped: true,
            supervised: true,
            root: false,
            armored: false,
            routed: false,
        };
        let at = Instant::now();
        let kept: KeptHost = Mutex::new(Some((at, host)));
        forget_supervisor(&kept);
        let marked = LinuxHost {
            supervised: false,
            ..host
        };
        assert_eq!(*kept.lock().unwrap(), Some((at, marked)));
        let nothing: KeptHost = Mutex::new(None);
        forget_supervisor(&nothing);
        assert_eq!(*nothing.lock().unwrap(), None);
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().canonicalize().unwrap();
        let (_project, sandbox) = project(SandboxNetwork::Only(Vec::new()));
        let failing = LinuxBackend::Landlock {
            helper: PathBuf::from("/bin/false"),
        };
        let launch = LinuxSandbox::new(sandbox, failing).unwrap();
        let unsupervised = launch.supervise(LinuxSupervisor::On, Some(data.as_path()));
        assert_eq!(unsupervised.supervision, None);
        assert_eq!(fs::read_dir(&data).unwrap().count(), 0);
    }

    /// A supervised launch guards the loopback ports of a limited network under TCP only,
    /// but a port it may also reach on any host or look names up on, always its own git
    /// folders, and signals only where the host holds nothing else (DEC-179 T4c).
    #[test]
    fn a_supervised_launch_carries_each_guard_its_policy_and_host_need() {
        use SandboxRemote::{AnyHost, Loopback};
        let remotes = vec![Loopback(8080), Loopback(443), AnyHost(443), Loopback(53)];
        let (_temp, sandbox) = project(SandboxNetwork::Only(remotes));
        assert!(sandbox.resolves_over_tcp());
        let own = GitOwn {
            trees: vec![sandbox.project_root().to_path_buf()],
            ..GitOwn::default()
        };
        let alone = Supervision { signals: true };
        let expected = Guard {
            loopback: vec![8080],
            git: Some(own),
            signals: true,
        };
        assert_eq!(guard(&sandbox, alone), expected);
        let nested = Guard {
            signals: false,
            ..expected.clone()
        };
        assert_eq!(guard(&sandbox, Supervision { signals: false }), nested);
        let ports = sandbox.clone().limit_sockets(LinuxSockets::TcpPorts);
        let any_host = Guard {
            loopback: Vec::new(),
            ..expected
        };
        assert_eq!(guard(&ports, alone), any_host);
        let (_open, open) = project(SandboxNetwork::Open);
        let held = guard(&open, alone);
        assert!(held.loopback.is_empty(), "{held:?}");
        assert!(held.git.is_some_and(|git| git.trees == [open.project_root()]));
    }

    /// The git folders a supervised agent owns are the ones the macOS boundary leaves it:
    /// the top of each tree it works in and, for the engine's worktree step, each folder in
    /// the two worktree folders (DEC-179 T4c, RV-DS241-85b).
    #[test]
    fn a_supervised_agent_owns_the_git_of_each_tree_it_works_in_and_of_each_engine_worktree() {
        let (temp, sandbox) = project(SandboxNetwork::Only(Vec::new()));
        let project = sandbox.project_root().to_path_buf();
        let alone = GitOwn {
            trees: vec![project.clone()],
            ..GitOwn::default()
        };
        assert_eq!(git_own(&sandbox), alone);
        let lease = temp.path().canonicalize().unwrap().join("lease");
        fs::create_dir(&lease).unwrap();
        let worked = sandbox.allow_worktree(&lease).unwrap().adding_worktrees();
        let state = project.join(crate::projects_home::CRAZYAGENT_DIRECTORY);
        let expected = GitOwn {
            trees: vec![project.clone(), lease],
            kids: vec![project.join("worktrees"), state.join("worktrees")],
            exact: Vec::new(),
        };
        assert_eq!(git_own(&worked), expected);
    }

    /// The helper gets guard flags only for a launch the call supervisor holds, and a
    /// nested one carries no signal guard: its process namespace holds signals.
    #[test]
    fn only_a_supervised_launch_hands_the_helper_its_guards() {
        let (_temp, sandbox) = project(SandboxNetwork::Open);
        let cwd = sandbox.project_root().to_path_buf();
        let helper = std::env::current_exe().unwrap();
        let backend = LinuxBackend::Nested {
            bwrap: helper.clone(),
            helper,
        };
        let plain = LinuxSandbox::new(sandbox, backend).unwrap();
        let mut harness = Command::new("/bin/sh");
        harness.args(["-c", "true"]).current_dir(&cwd);
        let words = |sandbox: &LinuxSandbox| -> Vec<OsString> {
            sandbox
                .wrap(&harness)
                .unwrap()
                .get_args()
                .map(OsStr::to_os_string)
                .collect()
        };
        assert!(!words(&plain).contains(&GUARD_GIT.into()));
        let supervised = LinuxSandbox {
            supervision: Some(Supervision { signals: false }),
            ..plain
        };
        let held = words(&supervised);
        let git = [GUARD_GIT, OWN_TREE].map(OsString::from);
        let at = held
            .windows(2)
            .position(|pair| pair == git)
            .expect("the git guard");
        assert_eq!(held[at + 2], cwd.as_os_str());
        assert!(!held.contains(&GUARD_SIGNALS.into()));
        assert!(!held.contains(&GUARD_LOOPBACK.into()));
    }

    /// The owner's setting Off, and a host that only has bubblewrap, start no supervisor:
    /// the launch stays as it was before the supervisor (DEC-179 add.2 Q1).
    #[test]
    fn the_supervisor_setting_off_and_bubblewrap_alone_start_no_supervisor() {
        let helper = std::env::current_exe().unwrap();
        let (_temp, sandbox) = project(SandboxNetwork::Only(Vec::new()));
        let landlock = LinuxBackend::Landlock {
            helper: helper.clone(),
        };
        let off = LinuxSandbox::new(sandbox.clone(), landlock).unwrap();
        assert_eq!(off.supervise(LinuxSupervisor::Off, None).supervision, None);
        let bubblewrap = LinuxBackend::Bubblewrap {
            bwrap: helper.clone(),
            helper,
        };
        let alone = LinuxSandbox::new(sandbox, bubblewrap).unwrap();
        assert_eq!(alone.supervise(LinuxSupervisor::On, None).supervision, None);
    }

    #[test]
    fn without_the_helper_bubblewrap_starts_no_agent() {
        let (_closed, sandbox) = project(SandboxNetwork::Only(Vec::new()));
        match LinuxSandbox::detect(sandbox, Path::new("/nonexistent/crazyagent-landlock")) {
            Err(SandboxError::Unavailable(reason)) => {
                assert!(reason.contains("helper"), "{reason}");
            }
            other => panic!("bubblewrap without the socket filter must be refused: {other:?}"),
        }
    }

    #[test]
    fn no_agent_is_granted_the_git_state_the_owners_git_reads_and_runs() {
        let (_temp, sandbox) = project(SandboxNetwork::Only(Vec::new()));
        let git = sandbox.project_root().join(".git");
        let trees = [
            "rebase-merge",
            "rebase-apply",
            "remotes",
            "branches",
            "sequencer",
            "refs",
        ];
        for name in trees {
            fs::create_dir_all(git.join(name)).unwrap();
        }
        for name in ["index", "index.lock", "HEAD"] {
            fs::write(git.join(name), "state\n").unwrap();
        }
        let grants = Grants::of(&sandbox).unwrap();
        for name in [
            "index",
            "index.lock",
            "rebase-merge",
            "rebase-apply",
            "remotes",
            "branches",
            "sequencer",
        ] {
            assert!(
                !grants.write.contains(&git.join(name)),
                "{name} is writable"
            );
        }
        for name in ["HEAD", "refs"] {
            assert!(
                grants.write.contains(&git.join(name)),
                "{name} is not writable"
            );
        }
    }
}
