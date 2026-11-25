use crate::hidden_paths::HIDDEN_MAX;
use crate::lines::HELD_NOT_PUT_BACK_EXIT;
use crate::paths::clean_absolute;
use crate::ports::{
    KEPT_PORTS_MAX, OWNER_PORTS, PICKED_PORTS_START, PROXY_PORTS, RELAY_PORTS, REMOTES_MAX,
};
use ::landlock::RulesetStatus;
use crazyagent_linux_supervisor::rules::GitOwn;
use crazyagent_linux_supervisor::serve;
use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr};
use std::ops::RangeInclusive;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

pub mod bridge;
pub mod egress;
pub mod landlock;
#[cfg(test)]
mod tests;

use bridge::Bridge;
use landlock::*;

/// Fixed places a distribution installs bubblewrap. It is never looked up through `PATH`.
pub const BWRAP_PATHS: &[&str] = &["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"];

pub(crate) const GRANTS_MAX: usize = 1024;

pub(crate) const HELPER_PATHS_MAX: usize = GRANTS_MAX + 64;

const READ: &str = "--read";

const WRITE: &str = "--write";

pub(crate) const GIT_WRITE: &str = "--git-write";

pub(crate) const CONNECT: &str = "--connect";

pub(crate) const LIMIT_NETWORK: &str = "--limit-network";

pub(crate) const OPEN_SOCKETS: &str = "--open-sockets";

pub(crate) const KEEP_FREE: &str = "--keep-free";

pub const PROBE: &str = "--probe";

pub(crate) const FILTER_SOCKETS: &str = "--filter-sockets";

pub(crate) const DIE_WITH: &str = "--die-with";

pub const NEST: &str = "--nest";

pub const NESTED: &str = "--nested";

pub(crate) const PRIVATE_MOUNTS: &str = "--private-mounts";

pub(crate) const COVER: &str = "--cover";

pub(crate) const SCOPED: &str = "--scoped";

pub(crate) const GUARD_LOOPBACK: &str = "--guard-loopback";

pub(crate) const GUARD_GIT: &str = "--guard-git";

pub(crate) const OWN_TREE: &str = "--own-tree";

pub(crate) const OWN_KIDS: &str = "--own-kids";

pub(crate) const OWN_GIT: &str = "--own-git";

pub(crate) const GUARD_SIGNALS: &str = "--guard-signals";

pub(crate) const GUARD_HELD: &str = "--guard-held";

pub(crate) const GUARD_HELD_TREE: &str = "--guard-held-tree";

pub(crate) const GUARD_HELD_STORE: &str = "--guard-held-store";

pub(crate) const PROBE_SUPERVISOR: &str = "--probe-supervisor";

const PROBE_SUPERVISED: &str = "--probe-supervised";

/// Why the helper runs no mode while its environment holds a variable that makes glibc load code
/// ([`loader_variable`]): every mode, and the bubblewrap a nest starts, runs in the helper's own
/// AppArmor profile, which may make user namespaces, so the variable would load a caller's code
/// there. The engine never sets one.
pub const LOADER_REFUSED: &str = "the helper refuses variables that load code";

/// The set-user-ID and set-group-ID mode bits: a bubblewrap with either runs as someone
/// other than the engine's user, so [`trusted`] refuses it.
pub(crate) const SET_ID_BITS: u32 = 0o6000;

/// The group and other write bits: a program anyone but its owner may change is never run.
const OTHERS_WRITE_BITS: u32 = 0o022;

/// The bubblewrap arguments a nested agent starts under: its own user, process, IPC and
/// host-name namespaces and `/proc`, root and devices bound through, so the helper's
/// Landlock rules stay the only file policy; a bridged or routed agent adds `--unshare-net`
/// ([`nest_command`]). `--die-with-parent` kills bubblewrap's outer process, which the
/// helper becomes, and the agent with it, once the engine thread that started it exits.
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

/// How the helper says the kernel holds the ruleset only in part or not at all, the one probe
/// failure a kernel refusal names ([`probe_refusal`]).
pub(crate) const HOLD_REFUSED: &str = "this kernel cannot hold the boundary";

/// The most ports one agent is kept off: the owner's fixed ports, the kept local model
/// ports and every model proxy and model relay port.
pub(crate) const KEEP_FREE_MAX: usize = OWNER_PORTS.len()
    + KEPT_PORTS_MAX
    + (*PROXY_PORTS.end() - *PROXY_PORTS.start()) as usize
    + 1
    + (*RELAY_PORTS.end() - *RELAY_PORTS.start()) as usize
    + 1;

const PICKED_PORTS_FILE: &str = "/proc/sys/net/ipv4/ip_local_port_range";

const PICKED_PORTS_BYTES_MAX: u64 = 64;

pub(crate) const PICKED_PORTS_UNREADABLE: &str = "CrazyAgent cannot read which ports this Linux hands out \
     by itself (net.ipv4.ip_local_port_range), so it cannot keep the owner's ports free of an \
     agent";

const _: () = {
    let mut index = 0;
    while index < OWNER_PORTS.len() {
        assert!(OWNER_PORTS[index] < PICKED_PORTS_START);
        index += 1;
    }
};

/// Why an agent whose project hides a folder does not start where the sandbox helper cannot give it
/// a mount tree of its own that no later mount on this computer reaches: a second mount of the
/// folder's disk made during the turn would lead past the cover.
pub const HIDDEN_MOUNTS_NOT_PRIVATE: &str = "hidden folders on Linux need an agent's mounts \
     cut off from this computer's, and this computer does not let the sandbox helper do it: \
     allow user namespaces for the programs bubblewrap starts, or stop hiding the folders, \
     then start the agent again";

/// What the `crazyagent-landlock` helper was asked to do: probe whether this kernel holds a
/// Landlock ruleset, apply a policy and start a harness, inside bubblewrap install only
/// the socket filter and start a harness, start bubblewrap from a fixed place with
/// [`NEST_ARGS`], which then starts the helper again inside its namespaces, tell whether
/// the kernel scopes signals, carry a nested agent's bridged ports as its broker, build a
/// routed agent's network as its setup child, start that network's slirp4netns confined,
/// tell whether this machine runs the call supervisor, or be the check that question starts.
#[derive(Debug, PartialEq, Eq)]
pub enum HelperRequest {
    Probe {
        limit_network: bool,
        open_sockets: bool,
        keep_free: bool,
        cut: Option<Vec<PathBuf>>,
    },
    Run {
        policy: Box<LandlockPolicy>,
        program: PathBuf,
        args: Vec<OsString>,
    },
    Filter {
        program: PathBuf,
        args: Vec<OsString>,
        cut: Option<Vec<PathBuf>>,
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
    Slirp {
        child: u32,
        ready: RawFd,
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
    /// [`egress::request`] takes, or `--slirp` with what [`egress::slirp_request`] takes.
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
        if args.first().is_some_and(|flag| flag == egress::SLIRP) {
            return egress::slirp_request(args);
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
                Some(GIT_WRITE) => policy.git_write.push(absolute(rest.next())?),
                Some(CONNECT) => policy.connect.push(port(rest.next())?),
                Some(KEEP_FREE) => policy.keep_free.push(port(rest.next())?),
                Some(DIE_WITH) if policy.die_with.is_none() => {
                    policy.die_with = Some(parent(rest.next())?);
                }
                Some(PRIVATE_MOUNTS) if policy.cut.is_none() => policy.cut = Some(Vec::new()),
                Some(COVER) => cover(policy.cut.as_mut(), rest.next())?,
                Some("--") if nest => {
                    let alone = !probe && !filter && policy == LandlockPolicy::default();
                    return nest_request(alone, rest);
                }
                Some("--") if !probe && filter => {
                    let cut = policy.cut.take();
                    if policy != LandlockPolicy::default() {
                        return Err(format!("{FILTER_SOCKETS} takes no policy"));
                    }
                    let program = absolute(rest.next())?;
                    return Ok(Self::Filter {
                        program,
                        args: rest.collect(),
                        cut,
                    });
                }
                Some("--") if !probe => return run_request(policy, rest),
                _ => return Err(format!("unexpected argument {}", flag.to_string_lossy())),
            }
        }
        let bare = policy.read.is_empty()
            && policy.write.is_empty()
            && policy.git_write.is_empty()
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
                cut: policy.cut,
            });
        }
        Err("missing the harness command after --".into())
    }
}

/// The helper words that ask for a private mount tree with these covers, or none.
/// Input: the covers, when the tree is asked for. Output: [`PRIVATE_MOUNTS`], then
/// [`COVER`] and the path for each cover.
pub(crate) fn cut_args(covers: Option<&[PathBuf]>) -> Vec<OsString> {
    let Some(covers) = covers else {
        return Vec::new();
    };
    let mut args = vec![OsString::from(PRIVATE_MOUNTS)];
    for cover in covers {
        args.push(COVER.into());
        args.push(cover.into());
    }
    assert_eq!(args.len(), 1 + 2 * covers.len());
    args
}

/// Adds one [`COVER`] to a private tree: after [`PRIVATE_MOUNTS`], a clean absolute path
/// below the root, each once, at most [`HIDDEN_MAX`]. Inputs: the covers
/// so far, when the tree was asked for, and the next word. Output: Ok, or the reason.
fn cover(covers: Option<&mut Vec<PathBuf>>, value: Option<OsString>) -> Result<(), String> {
    let covers = covers.ok_or_else(|| format!("{COVER} follows {PRIVATE_MOUNTS}"))?;
    let path = absolute(value)?;
    if !clean_absolute(&path) || path.parent().is_none() {
        return Err(format!(
            "{COVER} takes a clean folder below /: {}",
            path.display()
        ));
    }
    if covers.contains(&path) || covers.len() >= HIDDEN_MAX {
        return Err(format!(
            "{COVER} names each folder once, at most {}",
            HIDDEN_MAX
        ));
    }
    covers.push(path);
    Ok(())
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
    if policy.cut.is_some() && !policy.nested {
        return Err(format!("{PRIVATE_MOUNTS} needs {NESTED}"));
    }
    let program = absolute(rest.next())?;
    if policy.read.len() + policy.write.len() + policy.git_write.len() > HELPER_PATHS_MAX {
        return Err("too many path grants".into());
    }
    Ok(HelperRequest::Run {
        policy: Box::new(policy),
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

/// Tells whether what follows [`NEST_ARGS`] is what the engine's own nests write: an optional
/// `--unshare-net`, at most [`HIDDEN_MAX`] `--tmpfs P --remount-ro P` covers (P clean, absolute,
/// below the root), an optional clean absolute `--chdir`, then `--` and the command, so no caller
/// adds another bubblewrap option. Input: the words after the fixed arguments. Output: true when
/// they are.
fn own_options(words: &[OsString]) -> bool {
    let mut words = match words {
        [unshare, rest @ ..] if unshare == bridge::UNSHARE_NET => rest,
        _ => words,
    };
    let mut covers = 0;
    while let [tmpfs, path, remount, again, rest @ ..] = words
        && tmpfs == "--tmpfs"
    {
        let folder = Path::new(path);
        if remount != "--remount-ro"
            || again != path
            || !clean_absolute(folder)
            || folder.parent().is_none()
        {
            return false;
        }
        covers += 1;
        words = rest;
    }
    if covers > HIDDEN_MAX {
        return false;
    }
    let words = match words {
        [chdir, folder, rest @ ..] if chdir == "--chdir" => {
            if !clean_absolute(Path::new(folder)) {
                return false;
            }
            rest
        }
        _ => words,
    };
    matches!(words, [end, _command, ..] if end == "--")
}

/// Reads what follows `--nest --`: bubblewrap at one of its fixed places, then exactly
/// [`NEST_ARGS`], then only the options [`own_options`] allows and the command bubblewrap
/// starts, so the helper starts nothing else on a caller's word and no caller names a
/// descriptor flag stage 1 writes itself ([`egress::DESCRIPTOR_FLAGS`]). Inputs: whether
/// `--nest` was the only flag, and those arguments. Output: the request, or the reason.
fn nest_request(
    alone: bool,
    mut rest: impl Iterator<Item = OsString>,
) -> Result<HelperRequest, String> {
    if !alone {
        return Err(format!("{NEST} takes no other flag"));
    }
    let bwrap = absolute(rest.next())?;
    if !BWRAP_PATHS.iter().any(|path| Path::new(path) == bwrap) {
        return Err(format!(
            "{NEST} starts only bubblewrap from its fixed places"
        ));
    }
    let args: Vec<OsString> = rest.collect();
    let fixed = args
        .iter()
        .zip(NEST_ARGS)
        .all(|(arg, fixed)| arg.as_os_str() == OsStr::new(fixed));
    if !fixed || args.len() <= NEST_ARGS.len() {
        return Err(format!(
            "{NEST} needs bubblewrap's fixed arguments and a command"
        ));
    }
    if !own_options(&args[NEST_ARGS.len()..]) {
        return Err(format!(
            "{NEST} takes only --unshare-net, hidden folder covers and an absolute --chdir \
             before `--` and a command"
        ));
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
pub fn read_picked_ports() -> Option<RangeInclusive<u16>> {
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
pub(crate) fn kept_outside_picked(
    kept: &[u16],
    picked: Option<&RangeInclusive<u16>>,
) -> Result<(), String> {
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
pub(crate) fn bindable(keep_free: &[u16]) -> Vec<u16> {
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
/// ([`LOADER_REFUSED`]), or before it closed the descriptors it inherited unnamed
/// ([`close_inherited`]).
/// Output: the exit code, 2 for refused arguments or such a variable, 3 when the kernel
/// cannot hold the policy, the session or the signal mask, the process it dies with is gone
/// or an inherited descriptor stays open, 126 when the harness or bubblewrap cannot start.
pub fn main(args: Vec<OsString>) -> ExitCode {
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
    let (routes, bridge, started, request) = match read_request(args) {
        Ok(asked) => asked,
        Err(reason) => {
            eprintln!("crazyagent-landlock: {reason}");
            return ExitCode::from(2);
        }
    };
    let named: Vec<RawFd> = [
        bridge.as_ref().and_then(|bridge| bridge.fd),
        started,
        named_descriptor(&request),
    ]
    .into_iter()
    .flatten()
    .collect();
    if let Err(reason) = close_inherited(&named) {
        eprintln!("crazyagent-landlock: {reason}");
        return ExitCode::from(3);
    }
    let (policy, launch) = match request {
        HelperRequest::Filter { program, args, cut } => {
            return filtered(&program, args, cut.as_deref());
        }
        HelperRequest::Nest { bwrap, args } => {
            return nest(&bwrap, &args, bridge.as_ref(), &routes);
        }
        HelperRequest::Scoped => return scoped(),
        HelperRequest::Broker { parent, ports } => return bridge::broker(parent, &ports),
        HelperRequest::Egress { parent, ports } => return egress::setup(parent, &ports),
        HelperRequest::Slirp { child, ready } => return egress::slirp(child, ready),
        HelperRequest::Supervisor { folder } => return probe_supervisor(&folder),
        HelperRequest::Supervised { port, folder } => return probe_supervised(port, &folder),
        HelperRequest::Probe {
            limit_network,
            open_sockets,
            keep_free,
            cut,
        } => (
            LandlockPolicy {
                limit_network,
                open_sockets,
                keep_free: OWNER_PORTS[..usize::from(keep_free)].to_vec(),
                cut,
                ..LandlockPolicy::default()
            },
            None,
        ),
        HelperRequest::Run {
            policy,
            program,
            args,
        } => (*policy, Some((program, args))),
    };
    hold(&policy, launch, bridge.as_ref(), started)
}

/// Reads what the helper was asked: the route flags first ([`egress::leading`]), then the
/// bridge flags ([`Bridge::leading`]), then the go descriptor ([`egress::handed`]), then
/// the request, each set of flags only where the request takes it ([`Bridge::fits`],
/// [`egress::fits`]). A routed nest with no bridged port unshares the network for its
/// routes alone, so the bridge's check is left to the route's. Input: the helper
/// arguments. Output: the route ports, empty for none, the bridge, the go descriptor and
/// the request; or the reason.
pub(crate) fn read_request(args: Vec<OsString>) -> Result<Asked, String> {
    let (routes, args) = egress::leading(args)?;
    let (bridge, args) = Bridge::leading(args)?;
    let (started, args) = egress::handed(args)?;
    let request = HelperRequest::parse(args)?;
    if routes.is_empty() || bridge.is_some() {
        Bridge::fits(bridge.as_ref(), &request)?;
    }
    egress::fits(&routes, started, &request)?;
    assert!(routes.len() <= REMOTES_MAX);
    assert!(started.is_none() || routes.is_empty());
    Ok((routes, bridge, started, request))
}

/// What [`read_request`] reads: the route ports, the bridge, the go descriptor and the
/// request.
type Asked = (Vec<u16>, Option<Bridge>, Option<RawFd>, HelperRequest);

/// The descriptor a request names in its own arguments, which [`close_inherited`] must leave
/// open for it. Every request is listed, so a mode that takes a descriptor answers here.
/// Input: the request. Output: the descriptor, or none.
fn named_descriptor(request: &HelperRequest) -> Option<RawFd> {
    match request {
        HelperRequest::Slirp { ready, .. } => Some(*ready),
        HelperRequest::Probe { .. }
        | HelperRequest::Run { .. }
        | HelperRequest::Filter { .. }
        | HelperRequest::Nest { .. }
        | HelperRequest::Scoped
        | HelperRequest::Broker { .. }
        | HelperRequest::Egress { .. }
        | HelperRequest::Supervisor { .. }
        | HelperRequest::Supervised { .. } => None,
    }
}

/// Closes every descriptor above the standard three that this process holds and `named` leaves out,
/// before any mode runs. One the engine's own parent left open, not closed on exec, would otherwise
/// reach bubblewrap, slirp4netns and the harness, or stay readable through `/proc` in a helper that
/// stays a parent. Input: the named descriptors. Output: Ok, or the reason one cannot be listed or
/// closed.
fn close_inherited(named: &[RawFd]) -> Result<(), String> {
    assert!(named.iter().all(|fd| *fd > 2));
    let failed = |error: std::io::Error| format!("inherited descriptors: {error}");
    let mut open = Vec::new();
    for entry in fs::read_dir("/proc/self/fd").map_err(failed)? {
        let name = entry.map_err(failed)?.file_name();
        let fd = name
            .to_str()
            .and_then(|text| text.parse::<RawFd>().ok())
            .ok_or("inherited descriptors: /proc/self/fd lists a name that is not a number")?;
        if fd > 2 && !named.contains(&fd) {
            open.push(fd);
        }
    }
    for fd in open {
        match nix::unistd::close(fd) {
            Ok(()) | Err(nix::errno::Errno::EBADF | nix::errno::Errno::EINTR) => {}
            Err(error) => return Err(format!("inherited descriptor {fd}: {error}")),
        }
    }
    Ok(())
}

/// Holds a policy on this process, then becomes the harness, or with a guard stays as its parent
/// ([`supervise`]); a probe only tells whether this kernel held it. In order: in a routed nest the
/// go byte ([`egress::started`]), under a bridge the bridged listeners to the broker (so kept ports
/// are a private network's), the private mount tree when it hides folders ([`cut`]), the rules.
/// Inputs: the policy, the harness if any, the bridge and the go descriptor. Output: the exit code.
fn hold(
    policy: &LandlockPolicy,
    launch: Option<(PathBuf, Vec<OsString>)>,
    bridge: Option<&Bridge>,
    started: Option<RawFd>,
) -> ExitCode {
    if let Some(fd) = started
        && let Err(reason) = egress::started(fd)
    {
        eprintln!("crazyagent-landlock: {reason}");
        return ExitCode::from(3);
    }
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
    if let Some(covers) = &policy.cut
        && let Err(reason) = cut(covers)
    {
        eprintln!("crazyagent-landlock: {HIDDEN_MOUNTS_NOT_PRIVATE}: {reason}");
        return ExitCode::from(3);
    }
    match enforce(policy, launch.is_some()) {
        Ok(status) if accepted(&status.ruleset, status.no_new_privs) => {}
        Ok(status) => {
            eprintln!("crazyagent-landlock: {HOLD_REFUSED} ({:?})", status.ruleset);
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

const _: () = assert!(
    serve::NOT_PUT_BACK_EXIT == HELD_NOT_PUT_BACK_EXIT
        && serve::NOT_PUT_BACK_EXIT > 0
        && serve::NOT_PUT_BACK_EXIT < serve::STOPPED_EXIT,
    "the held-stop exit code is the engine's and sits below the supervisor's other stops"
);

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
    let checked =
        kernel_processes::spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, 0)).and_then(|listener| {
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
                held: None,
            };
            serve::run(all.supervised(&[port]), &mut check)
        });
    match checked {
        Ok(0) => ExitCode::SUCCESS,
        _ => ExitCode::from(1),
    }
}

/// Tells whether a folder is the private probe folder the engine made ([`private_folder`]): a real
/// folder, not a link, that this user owns with mode 0700 and that holds nothing. Input: the
/// absolute folder. Output: true only then.
pub(crate) fn private_empty(folder: &Path) -> bool {
    assert!(folder.is_absolute());
    let Ok(held) = fs::symlink_metadata(folder) else {
        return false;
    };
    let own = held.uid() == rustix::process::geteuid().as_raw();
    let private = held.is_dir() && own && held.mode() & 0o7777 == 0o700;
    private && fs::read_dir(folder).is_ok_and(|mut entries| entries.next().is_none())
}

/// The call supervisor's check, run as its harness: the guarded port is reached on this machine and
/// refused on another address, a new file is made and written, a `.git` is refused, and so are a
/// signal to the supervisor, an open of its memory for writing and a take of its descriptor.
/// Inputs: the guarded port and an empty folder. Output: 0 when all seven hold, else 1.
fn probe_supervised(port: u16, folder: &Path) -> ExitCode {
    let refused = |error: std::io::Error| error.kind() == std::io::ErrorKind::PermissionDenied;
    let elsewhere = SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), port));
    let made = folder.join("made");
    let here = kernel_processes::spawn_gate::connect_tcp_std((Ipv4Addr::LOCALHOST, port));
    let there =
        kernel_processes::spawn_gate::connect_tcp_std_timeout(&elsewhere, Duration::from_secs(2));
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
/// bubblewrap cannot start. With route ports it first starts the setup child that builds
/// the agent's network and writes the descriptor flags bubblewrap reports and waits on
/// ([`egress::open`]); with a bridge it then starts the broker on this network and writes
/// the bridge flags for the helper inside ([`bridge::open`]). It keeps every end open until
/// bubblewrap has it, and hands the route ends over only after both children started.
fn nest(bwrap: &Path, args: &[OsString], bridge: Option<&Bridge>, routes: &[u16]) -> ExitCode {
    assert!(routes.len() <= REMOTES_MAX);
    let Some(program) = trusted_file(bwrap) else {
        eprintln!(
            "crazyagent-landlock: {} is not a bubblewrap to nest under",
            bwrap.display()
        );
        return ExitCode::from(3);
    };
    let ready = own_session()
        .and_then(|()| match routes {
            [] => Ok((None, args.to_vec())),
            routes => egress::open(routes, args).map(|(ends, args)| (Some(ends), args)),
        })
        .and_then(|(ends, args)| match bridge {
            Some(bridge) => {
                bridge::open(&bridge.ports, &args).map(|(end, args)| (ends, Some(end), args))
            }
            None => Ok((ends, None, args)),
        })
        .and_then(|opened| {
            if let Some(ends) = &opened.0 {
                ends.hand_over()?;
            }
            nix::sys::signal::SigSet::all()
                .thread_block()
                .map_err(|error| format!("signal mask: {error}"))?;
            nix::sys::prctl::set_no_new_privs()
                .map_err(|error| format!("no new privileges: {error}"))?;
            Ok(opened)
        });
    let (_ends, _end, args) = match ready {
        Ok(opened) => opened,
        Err(reason) => {
            eprintln!("crazyagent-landlock: {reason}");
            return ExitCode::from(3);
        }
    };
    assert!(routes.is_empty() || egress::placed(&args).is_ok());
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

/// Opens a bubblewrap or tool the helper may run and checks the file it opened, not the
/// path again: a file that root or this user owns, with neither [`SET_ID_BITS`] bit nor
/// [`OTHERS_WRITE_BITS`], so it runs with this user's privileges and only root or this user
/// put it there or may change it. It opens without reading, so a program that can only be
/// run still opens. Input: its path. Output: the open file, which [`nest`] starts, or none.
pub(crate) fn trusted_file(bwrap: &Path) -> Option<fs::File> {
    let own = rustix::process::geteuid().as_raw();
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open(bwrap)
        .ok()?;
    let held = file.metadata().ok()?;
    let plain = held.is_file() && held.mode() & (SET_ID_BITS | OTHERS_WRITE_BITS) == 0;
    (plain && [0, own].contains(&held.uid())).then_some(file)
}

/// One line of the mount list: the mount's id, its disk's device number, the folder of
/// that disk it shows and the folder it is mounted on.
pub(crate) struct Mount {
    pub(crate) id: u64,
    pub(crate) device: String,
    pub(crate) root: PathBuf,
    pub(crate) point: PathBuf,
}

/// Reads the mount list text, one mount per line as `/proc/self/mountinfo` writes it.
/// Input: the text. Output: the mounts, or none when a line does not read plainly.
pub(crate) fn mounts(text: &str) -> Option<Vec<Mount>> {
    text.lines()
        .map(|line| {
            let mut fields = line.split(' ');
            let id = fields.next()?.parse().ok()?;
            fields.next()?;
            let device = fields.next()?;
            let (major, minor) = device.split_once(':')?;
            major.parse::<u32>().ok()?;
            minor.parse::<u32>().ok()?;
            let root = unescaped(fields.next()?)?;
            let point = unescaped(fields.next()?)?;
            point.is_absolute().then(|| Mount {
                id,
                device: device.to_owned(),
                root,
                point,
            })
        })
        .collect()
}

/// Undoes the mount list's octal escapes, such as `\040` for a space. Input: one field.
/// Output: the path, or none for an escape that is not three octal digits.
fn unescaped(field: &str) -> Option<PathBuf> {
    let bytes = field.as_bytes();
    let mut path = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'\\' {
            path.push(bytes[at]);
            at += 1;
            continue;
        }
        let digits = bytes.get(at + 1..at + 4)?;
        if !digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) {
            return None;
        }
        let value = digits
            .iter()
            .fold(0u32, |value, digit| value * 8 + u32::from(digit - b'0'));
        path.push(u8::try_from(value).ok()?);
        at += 4;
    }
    assert_eq!(at, bytes.len());
    Some(PathBuf::from(OsStr::from_bytes(&path)))
}

/// Gives the helper a mount tree of its own that no later mount on this computer reaches, before
/// Landlock, and proves it: one thread, a user and a mount namespace of its own mapped to the same
/// ids, every mount private, each cover and `/proc` a mount that `umount2` refuses as locked, then
/// no capability, bounding set included. Input: the covers. Output: Ok, or the reason.
fn cut(covers: &[PathBuf]) -> Result<(), String> {
    let read = |path: &str| fs::read_to_string(path).map_err(|error| format!("{path}: {error}"));
    let write = |path: &str, text: String| {
        fs::write(path, text).map_err(|error| format!("{path}: {error}"))
    };
    if !alone(&read("/proc/self/status")?) {
        return Err("the helper runs more than one thread".into());
    }
    let uid = rustix::process::getuid().as_raw();
    let gid = rustix::process::getgid().as_raw();
    nix::sched::unshare(
        nix::sched::CloneFlags::CLONE_NEWUSER | nix::sched::CloneFlags::CLONE_NEWNS,
    )
    .map_err(|error| format!("a user and mount namespace of its own: {error}"))?;
    write("/proc/self/setgroups", "deny".into())?;
    write("/proc/self/uid_map", format!("{uid} {uid} 1\n"))?;
    write("/proc/self/gid_map", format!("{gid} {gid} 1\n"))?;
    let private = nix::mount::MsFlags::MS_REC | nix::mount::MsFlags::MS_PRIVATE;
    nix::mount::mount(None::<&str>, "/", None::<&str>, private, None::<&str>)
        .map_err(|error| format!("private mounts: {error}"))?;
    let mountinfo = read("/proc/self/mountinfo")?;
    if !propagation_free(&mountinfo) {
        return Err("a mount still takes mount events".into());
    }
    for cover in covers
        .iter()
        .map(PathBuf::as_path)
        .chain([Path::new("/proc")])
    {
        if !covered(&mountinfo, cover) {
            return Err(format!("{} is not a mount", cover.display()));
        }
        match nix::mount::umount2(cover, nix::mount::MntFlags::empty()) {
            Err(nix::errno::Errno::EINVAL) => {}
            Ok(()) => return Err(format!("{} came off", cover.display())),
            Err(error) => return Err(format!("{}: {error}", cover.display())),
        }
    }
    for capability in rustix::thread::CapabilitySet::all().iter() {
        match rustix::thread::remove_capability_from_bounding_set(capability) {
            Ok(()) | Err(rustix::io::Errno::INVAL) => {}
            Err(error) => return Err(format!("bounding set: {error}")),
        }
    }
    egress::drop_capabilities()?;
    if !capabilities_bare(&read("/proc/self/status")?) {
        return Err("a capability is left".into());
    }
    Ok(())
}

/// Tells whether a process status text names one thread. Input: `/proc/self/status`.
/// Output: true only for `Threads: 1`.
fn alone(status: &str) -> bool {
    status.lines().any(|line| {
        line.strip_prefix("Threads:")
            .is_some_and(|count| count.trim() == "1")
    })
}

/// Tells whether no mount in a mount list takes or gives mount events: no line carries a
/// `shared:` or `master:` field. Input: the mount list text. Output: true only then, and
/// never for a list with no line or a line that does not read plainly.
fn propagation_free(mountinfo: &str) -> bool {
    !mountinfo.is_empty()
        && mounts(mountinfo).is_some()
        && mountinfo.lines().all(|line| {
            line.split_once(" - ").is_some_and(|(head, _)| {
                head.split(' ')
                    .skip(6)
                    .all(|field| !field.starts_with("shared:") && !field.starts_with("master:"))
            })
        })
}

/// Tells whether a mount list holds a mount at exactly this folder. Inputs: the mount list
/// text and the folder. Output: true only then.
fn covered(mountinfo: &str, folder: &Path) -> bool {
    mounts(mountinfo).is_some_and(|mounts| mounts.iter().any(|mount| mount.point == folder))
}

/// Tells whether a process status text shows every capability set empty: inheritable,
/// permitted, effective, bounding and ambient. Input: `/proc/self/status`. Output: true
/// only when each of the five shows once and reads zero.
fn capabilities_bare(status: &str) -> bool {
    ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"]
        .iter()
        .all(|name| {
            let mut values = status.lines().filter_map(|line| line.strip_prefix(name));
            let value = values.next();
            values.next().is_none()
                && value.is_some_and(|value| u64::from_str_radix(value.trim(), 16) == Ok(0))
        })
}
