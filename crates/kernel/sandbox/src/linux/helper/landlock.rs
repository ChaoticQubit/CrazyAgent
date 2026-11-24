use super::*;
use crate::linux::places::{fixed_place, granted_place, held_roots};
use ::landlock::{
    ABI, Access, AccessFs, AccessNet, BitFlags, NetPort, PathBeneath, RestrictionStatus, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetError, Scope, make_bitflags,
};
use crazyagent_linux_supervisor::rules::{GitOwn, Held, Reach};
use crazyagent_linux_supervisor::serve::Supervised;
use rustix::fs::{FileType, Mode, OFlags};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The socket family of SMC, whose listener holds a TCP port and takes plain TCP clients.
const AF_SMC: u64 = 43;

pub(super) const FS_ABI: ABI = ABI::V3;

#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: i64 = 0x4000_0000;

const NET_ABI: ABI = ABI::V4;

const SOCKET_TYPE_MASK: u64 = 0xf;

const SCOPE_ABI: ABI = ABI::V6;

/// The rules the Landlock helper applies to itself before it starts the harness: read and
/// execute beneath `read`, full access beneath `write`, and, when `limit_network` is set,
/// outbound TCP only to the `connect` ports, no TCP bind and no socket but TCP, unless
/// `open_sockets` keeps the other socket kinds open ([`LinuxSockets::TcpPorts`]). A listen
/// with no bind still takes a port Linux picks, on either network, so both name
/// `keep_free`: the helper starts no agent while Linux could hand one of them out by
/// itself, and an open-network agent binds any TCP port but those. `die_with` names the
/// process the helper and the program it becomes are killed with (an engine git step).
/// `nested` tells the helper it runs inside bubblewrap's namespaces ([`NEST_ARGS`]).
/// `guard` names what the helper's call supervisor holds ([`Guard`]). `git_write` names
/// the git folders a confined agent writes with [`git_folder_rights`] only. The
/// engine renders it as helper arguments and the helper parses it back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LandlockPolicy {
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
    pub git_write: Vec<PathBuf>,
    pub connect: Vec<u16>,
    pub limit_network: bool,
    pub open_sockets: bool,
    pub keep_free: Vec<u16>,
    pub die_with: Option<u32>,
    pub nested: bool,
    pub guard: Guard,
    /// With `nested` or a probe: asks the helper for a mount tree of its own that no later mount
    /// on this computer reaches, and names the covers it proves locked there first ([`cut`]).
    pub cut: Option<Vec<PathBuf>>,
}

/// What the helper's call supervisor holds for an agent on a host with no namespaces to nest it in:
/// `loopback` names the connect ports it reaches on this machine only, `git` its own git folders,
/// the only `.git` entries it may make, and `signals` keeps its signals inside the programs it
/// started, and `held` names the entries it may not write, move, link or remove. With none of them
/// the helper becomes the harness; with any it stays as the harness's parent and answers its
/// guarded calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Guard {
    pub loopback: Vec<u16>,
    pub git: Option<GitOwn>,
    pub signals: bool,
    pub held: Option<Held>,
}

impl Guard {
    /// The helper arguments for this guard, in the order the helper reads them back.
    pub(crate) fn args(&self) -> Vec<OsString> {
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
        if let Some(held) = &self.held {
            for (flag, paths) in [
                (GUARD_HELD, &held.entries),
                (GUARD_HELD_TREE, &held.trees),
                (GUARD_HELD_STORE, &held.stores),
            ] {
                for path in paths {
                    args.push(flag.into());
                    args.push(path.into());
                }
            }
        }
        args
    }

    /// What the supervisor holds for this guard. Input: the policy's connect ports.
    /// Output: the guarded ports reached on this machine only and the others anywhere.
    pub(crate) fn supervised(&self, connect: &[u16]) -> Supervised {
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
            reach: (!self.loopback.is_empty()).then_some(reach),
            git: self.git.clone(),
            signals: self.signals,
            held: self.held.clone(),
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
        for path in &self.git_write {
            args.push(GIT_WRITE.into());
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
        args.extend(cut_args(self.cut.as_deref()));
        args.extend(self.guard.args());
        args
    }
}

/// Reads one call-supervisor flag into a guard. Inputs: the guard, the flag and the
/// arguments after it. Output: whether the flag was one of them, or the reason its value
/// is refused; an own git place is one only after `--guard-git`, which comes once, and a
/// held place is an absolute path.
pub(super) fn guard_flag(
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
        Some(GUARD_HELD) => Some(&mut guard.held.get_or_insert_with(Held::default).entries),
        Some(GUARD_HELD_TREE) => Some(&mut guard.held.get_or_insert_with(Held::default).trees),
        Some(GUARD_HELD_STORE) => Some(&mut guard.held.get_or_insert_with(Held::default).stores),
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
pub(crate) fn guard_fits(policy: &LandlockPolicy) -> Result<(), String> {
    let guard = &policy.guard;
    if *guard == Guard::default() {
        return Ok(());
    }
    if policy.die_with.is_some() {
        return Err(format!("{DIE_WITH} takes no call supervisor"));
    }
    let tcp_only = policy.limit_network && !policy.open_sockets;
    if !guard.loopback.is_empty() && !tcp_only {
        return Err(format!(
            "{GUARD_LOOPBACK} needs {LIMIT_NETWORK} without {OPEN_SOCKETS}"
        ));
    }
    if !guard
        .loopback
        .iter()
        .all(|port| policy.connect.contains(port))
    {
        return Err(format!("{GUARD_LOOPBACK} names a port with no {CONNECT}"));
    }
    let own = |git: &GitOwn| git.trees.len() + git.kids.len() + git.exact.len();
    if guard.git.as_ref().map_or(0, own) > GRANTS_MAX {
        return Err("too many own git places".into());
    }
    let held = |held: &Held| held.entries.len() + held.trees.len() + held.stores.len();
    if guard.held.as_ref().map_or(0, held) > GRANTS_MAX {
        return Err("too many held places".into());
    }
    Ok(())
}

/// The rights a git folder grant ([`LandlockPolicy::git_write`]) adds: read, write, truncate, make
/// and remove files and folders, and no symlink, no move from or to another folder, no device, fifo
/// or socket and no execute. Landlock unions a folder's rights with its ancestors', so a read grant
/// above the folder still gives execute. No input. Output: the rights.
pub fn git_folder_rights() -> BitFlags<AccessFs> {
    make_bitflags!(AccessFs::{
        ReadFile | ReadDir | WriteFile | Truncate | MakeReg | MakeDir | RemoveFile | RemoveDir
    })
}

/// The rules for one list of policy paths: a fixed place opens as the crate opens it, a
/// granted place through [`granted_place`]; a place that does not open gets no rule, and
/// one that is not a folder gets only the file rights of `access`. Inputs: the paths and
/// the rights. Output: the rules, or the refusal of a granted place that became a link.
fn place_rules(
    paths: &[PathBuf],
    access: BitFlags<AccessFs>,
) -> Result<Vec<PathBeneath<OwnedFd>>, String> {
    let flags = OFlags::PATH | OFlags::CLOEXEC;
    let mut rules = Vec::with_capacity(paths.len());
    for path in paths {
        let opened = if fixed_place(path) {
            rustix::fs::open(path.as_path(), flags, Mode::empty()).ok()
        } else {
            granted_place(path)?
        };
        let Some(place) = opened else {
            continue;
        };
        let folder = rustix::fs::fstat(&place)
            .is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::Directory);
        let rights = if folder {
            access
        } else {
            access & AccessFs::from_file(FS_ABI)
        };
        rules.push(PathBeneath::new(place, rights));
    }
    assert!(rules.len() <= paths.len());
    Ok(rules)
}

/// Applies the policy's Landlock rules to this process. A limited network handles every
/// network right and allows only its `connect` ports. An open network with kept ports
/// handles TCP bind alone and, with `rules`, allows every [`bindable`] port; without `rules`,
/// as in a probe, it only proves this kernel handles the right. A granted folder that became
/// a link is refused ([`granted_place`]). Inputs: the policy, and whether to add the bind
/// rules. Output: what the kernel enforced, or the refusal or ruleset error as text.
pub(super) fn enforce(policy: &LandlockPolicy, rules: bool) -> Result<RestrictionStatus, String> {
    let read = place_rules(&policy.read, AccessFs::from_read(FS_ABI))?;
    let write = place_rules(&policy.write, AccessFs::from_all(FS_ABI))?;
    let git_write = place_rules(&policy.git_write, git_folder_rights())?;
    held_ruleset(policy, rules, [read, write, git_write]).map_err(|error| error.to_string())
}

/// Builds and applies the ruleset [`enforce`] describes from the path rules already opened.
/// Inputs: the policy, whether to add the bind rules and the read, write and git write
/// rules. Output: what the kernel enforced, or the ruleset error.
fn held_ruleset(
    policy: &LandlockPolicy,
    rules: bool,
    places: [Vec<PathBeneath<OwnedFd>>; 3],
) -> Result<RestrictionStatus, RulesetError> {
    let keeps = !policy.limit_network && !policy.keep_free.is_empty();
    let mut ruleset = Ruleset::default().handle_access(AccessFs::from_all(FS_ABI))?;
    if policy.limit_network {
        ruleset = ruleset.handle_access(AccessNet::from_all(NET_ABI))?;
    } else if keeps {
        ruleset = ruleset.handle_access(AccessNet::BindTcp)?;
    }
    let mut created = ruleset.create()?;
    for list in places {
        created = created.add_rules(list.into_iter().map(Ok::<_, RulesetError>))?;
    }
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
pub(super) fn scope_signals() -> Result<RestrictionStatus, RulesetError> {
    Ruleset::default()
        .scope(Scope::from_all(SCOPE_ABI))?
        .create()?
        .restrict_self()
}

/// Installs the seccomp filter that agents, slirp4netns ([`egress::confine_slirp`]) and their
/// children keep: a Unix-domain `socket(2)` and `io_uring_setup` fail with `EACCES`, with
/// `tcp_only` so does every socket but TCP and routing netlink, and with `plain_tcp` so does
/// a stream socket that holds a TCP port without being plain TCP. Inputs: the two switches.
/// Output: the installed filter, or the reason this kernel or architecture cannot hold it.
pub(super) fn refuse_sockets(tcp_only: bool, plain_tcp: bool) -> Result<(), String> {
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
pub(super) fn refused_sockets(tcp_only: bool, plain_tcp: bool) -> Result<Vec<SeccompRule>, String> {
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

/// Starts a harness bubblewrap launched, after a private mount tree if it hides folders ([`cut`]),
/// behind the socket filter a Landlock agent with a network limit gets ([`refuse_sockets`] with
/// `tcp_only`): no Unix-domain socket, no io_uring ring, no family but TCP and routing netlink, so
/// no AF_VSOCK leaves the closed network. Each bound grant root is checked first to be the place
/// the engine opened at wrap ([`held_roots`]). Inputs: the harness, the covers and the bound
/// roots. Output: 3 when the cut, the check or the filter fails, 126 when the harness cannot
/// start.
pub(super) fn filtered(
    program: &Path,
    args: Vec<OsString>,
    covers: Option<&[PathBuf]>,
    bound: &[Bound],
) -> ExitCode {
    if let Some(covers) = covers
        && let Err(reason) = cut(covers)
    {
        eprintln!("crazyagent-landlock: {HIDDEN_MOUNTS_NOT_PRIVATE}: {reason}");
        return ExitCode::from(3);
    }
    if let Err(reason) = held_roots(bound) {
        eprintln!("crazyagent-landlock: {reason}");
        return ExitCode::from(3);
    }
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
