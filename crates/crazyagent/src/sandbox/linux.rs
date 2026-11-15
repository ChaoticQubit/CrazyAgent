use super::model_proxy::{PICKED_PORTS_START, PROXY_PORTS, RELAY_PORTS};
use super::{
    BRIDGE_PORT_MIN, GIT_STATE_FILES, GIT_STATE_TREES, KEPT_PORTS_MAX, LINUX_HARNESS_UNCHECKED,
    LINUX_LIMITED_NETWORK_OFF, LINUX_OPEN_NETWORK_NEEDS_LANDLOCK, LINUX_RESOLVER_NEEDS_UDP,
    LinuxSockets, OWNER_PORTS, ProjectSandbox, Sandbox, SandboxError, SandboxNetwork,
    SandboxRemote, agent_env, clean_absolute, invalid,
};
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
use std::ops::RangeInclusive;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

mod bridge;

pub use bridge::Bridge;

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
/// How long [`host_now`] keeps what it read: a policy view asks on every read, and each
/// answer starts the helper twice.
const HOST_AGE_MAX: Duration = Duration::from_secs(60);
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
/// `nested` tells the helper it runs inside bubblewrap's namespaces ([`NEST_ARGS`]). The
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
/// the kernel scopes signals, or carry a nested agent's bridged ports as its broker.
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
}

impl HelperRequest {
    /// Parses the helper arguments after its own name. Input: flags from
    /// [`LandlockPolicy::args`] then `--`, an absolute program and its arguments, or
    /// `--probe` with an optional `--limit-network`, `--open-sockets` and `--keep-free`
    /// with a port, or `--filter-sockets` alone then `--` and the harness, or `--nest`
    /// alone then `--` and what [`nest_request`] takes, or `--scoped` with nothing else, or
    /// `--broker` with what [`bridge::broker_request`] takes.
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
                Some("--") if !probe => {
                    open_needs_limit(&policy)?;
                    binds_fit(&policy)?;
                    if policy.nested && policy.die_with.is_some() {
                        return Err(format!("{NESTED} takes no {DIE_WITH}"));
                    }
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
        let bare = policy.read.is_empty()
            && policy.write.is_empty()
            && policy.connect.is_empty()
            && policy.die_with.is_none()
            && !policy.nested;
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
/// harness; a nest becomes bubblewrap, which starts a run inside its namespaces ([`nest`]).
/// Bridge flags may lead a nest and the run or probe inside it ([`Bridge::fits`]).
/// Output: the exit code, 2 for refused arguments, 3 when the kernel cannot hold the policy,
/// the session or the signal mask, or the process it dies with is gone, 126 when the
/// harness or bubblewrap cannot start.
pub fn helper_main(args: Vec<OsString>) -> ExitCode {
    let asked = Bridge::leading(args).and_then(|(bridge, args)| {
        let request = HelperRequest::parse(args)?;
        Bridge::fits(bridge.as_ref(), &request)?;
        Ok((bridge, request))
    });
    let (bridge, request) = match asked {
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

/// Holds a policy on this process, then becomes the harness; for a probe it only tells
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
    let error = Command::new(&program).args(args).exec();
    eprintln!(
        "crazyagent-landlock: cannot start {}: {error}",
        program.display()
    );
    ExitCode::from(126)
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

/// Starts bubblewrap for a nested agent, from outside its namespaces. It takes the session
/// as [`own_session`] does, where a pane's first process can still be told apart, then
/// blocks every signal: bubblewrap's outer process and its first process inside inherit the
/// mask, so they end only with the harness or by KILL, and a stop signal to the group is
/// the harness's to handle. Last it gives up new privileges, so a bubblewrap swapped for a
/// set-user-ID one after [`trusted`] looked still runs as this user. Output: 3 when any of
/// the three fails, 126 when bubblewrap cannot start. With a bridge it first starts the
/// broker on this network and writes the bridge flags for the helper inside
/// ([`bridge::open`]), keeping its end of the bridge open until bubblewrap has it.
fn nest(bwrap: &Path, args: &[OsString], bridge: Option<&Bridge>) -> ExitCode {
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
    let error = exec_keeping_mask(bwrap, &args);
    eprintln!(
        "crazyagent-landlock: cannot start {}: {error}",
        bwrap.display()
    );
    ExitCode::from(126)
}

/// Replaces this process with `program`, keeping its signal mask and its environment.
/// `std`'s exec empties the mask first, which would undo [`nest`]. Inputs: the program and
/// its arguments after its own name. Output: only the reason the exec failed.
fn exec_keeping_mask(program: &Path, args: &[OsString]) -> String {
    let words: Result<Vec<CString>, _> = std::iter::once(program.as_os_str())
        .chain(args.iter().map(OsString::as_os_str))
        .map(|word| CString::new(word.as_bytes()))
        .collect();
    let Ok(words) = words else {
        return "an argument holds a NUL byte".into();
    };
    assert!(words.len() > NEST_ARGS.len());
    let Err(errno) = nix::unistd::execv(&words[0], &words);
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
        Ok(Self { sandbox, backend })
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

    /// The program every wrapped command starts: the Landlock helper or bwrap.
    pub fn launcher(&self) -> &Path {
        match &self.backend {
            LinuxBackend::Landlock { helper } | LinuxBackend::Nested { helper, .. } => helper,
            LinuxBackend::Bubblewrap { bwrap, .. } => bwrap,
        }
    }
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

/// Tells whether a bubblewrap is one the helper may nest an agent under: a file that root
/// or this user owns, with neither [`SET_ID_BITS`] bit, so it runs with this user's
/// privileges and was put there by root or by this user. A host whose bubblewrap fails
/// this stays on plain Landlock with its gap lines. Input: its path. Output: true when it
/// may.
fn trusted(bwrap: &Path) -> bool {
    let own = rustix::process::geteuid().as_raw();
    fs::metadata(bwrap).is_ok_and(|file| {
        file.is_file() && file.mode() & SET_ID_BITS == 0 && [0, own].contains(&file.uid())
    })
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

/// Reads what this machine holds for an agent beyond its Landlock rules: whether the
/// kernel scopes signals (the helper's `--scoped`) and whether a launch gives the agent a
/// process namespace: the plain probe passes inside bubblewrap's namespaces ([`nests`]), or
/// fails outside them, which sends every launch through bubblewrap. Input: the helper.
/// Output: the facts, each false when its question cannot be asked.
pub fn read_host(helper: &Path) -> super::LinuxHost {
    let usable = clean_absolute(helper) && helper.is_file();
    let scoped = usable && passes(Command::new(helper).arg(SCOPED));
    let held = usable && passes(Command::new(helper).arg(PROBE));
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file());
    let apart = |bwrap: &Path| !held || nests(bwrap, helper, &[PROBE.into()]);
    let nested = usable && bwrap.is_some_and(apart);
    super::LinuxHost { nested, scoped }
}

/// [`read_host`] for the helper beside this executable, read again once what is kept is
/// older than `age_max`, or with no age only when nothing is kept. The lock is not held
/// while the helper runs, so a reader of what is kept never waits on it. Input: the age.
/// Output: the facts, each false when this executable's path cannot be read.
fn host_within(age_max: Option<Duration>) -> super::LinuxHost {
    static READ: Mutex<Option<(Instant, super::LinuxHost)>> = Mutex::new(None);
    let kept = *READ.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, host)) = kept
        && age_max.is_none_or(|age_max| at.elapsed() < age_max)
    {
        return host;
    }
    let helper = std::env::current_exe().map(|engine| engine.with_file_name(HELPER_NAME));
    let host = read_host(&helper.unwrap_or_default());
    *READ.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), host));
    host
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
                let mut command = Command::new(helper);
                command.args(landlock_policy(&grants, &self.sandbox).args());
                command
            }
            LinuxBackend::Nested { bwrap, helper } => {
                let policy = LandlockPolicy {
                    nested: true,
                    ..landlock_policy(&grants, &self.sandbox)
                };
                let bridged = self.sandbox.bridged_ports().unwrap_or_default();
                nest_command(bwrap, helper, &policy, cwd, &bridged)
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
/// the policy. With bridged ports the bridge flags lead and bubblewrap gives the agent a
/// network of its own. Inputs: bubblewrap, the helper, the policy, marked nested, the
/// working folder and the bridged ports. Output: the command, which the harness follows
/// after `--`.
fn nest_command(
    bwrap: &Path,
    helper: &Path,
    policy: &LandlockPolicy,
    cwd: &Path,
    bridged: &[u16],
) -> Command {
    assert!(policy.nested);
    assert!(policy.die_with.is_none());
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
