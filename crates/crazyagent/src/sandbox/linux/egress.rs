use super::super::REMOTES_MAX;
use super::bridge::UNSHARE_NET;
use super::{
    BWRAP_PATHS, FS_ABI, HelperRequest, NEST_ARGS, accepted, die_with_parent, trusted_file,
};
use landlock::{
    Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetError, path_beneath_rules,
};
use rustix::fs::FileType;
use rustix::thread::{CapabilitySet, CapabilitySets, LinkNameSpaceType};
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::io::{PipeReader, PipeWriter, Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};
use std::str::FromStr;
use std::time::Duration;

pub(super) const ROUTE: &str = "--route";
pub(super) const EGRESS: &str = "--egress";
pub(super) const SLIRP: &str = "--slirp";
/// What slirp4netns reads and executes under its rules ([`confine_slirp`]), Landlock's read
/// rights holding execute: its program and libraries, the loader cache, the resolver file it
/// forwards names to, whose rule binds the file a link there points to, and the random
/// source, as a trace on the farm runner showed (V243 linux routed r1, arm C). A path
/// missing on this computer adds no rule.
const SLIRP_READ: &[&str] = &[
    "/usr",
    "/lib",
    "/lib64",
    "/etc/ld.so.cache",
    RESOLV_CONF,
    "/dev/urandom",
];
const TUN: &str = "/dev/net/tun";
/// The flag stage 1 writes right after this helper's name in a routed nest: the descriptor
/// the setup child's go byte reaches stage 2 on ([`started`]).
pub(super) const ROUTE_FD: &str = "--route-fd";
const INFO_FD: &str = "--info-fd";
const BLOCK_FD: &str = "--block-fd";
const RO_BIND_DATA: &str = "--ro-bind-data";
const RESOLV_CONF: &str = "/etc/resolv.conf";
/// The bubblewrap flags stage 1 writes itself, each with a descriptor it opened, in this
/// order and right after `--unshare-net`, so the engine never names one ([`placed`]).
pub(super) const DESCRIPTOR_FLAGS: [&str; 3] = [INFO_FD, BLOCK_FD, RO_BIND_DATA];
/// Where the setup child takes slirp4netns from, and only from there: no flag or variable
/// names another program, which would then run with network capabilities in the agent's
/// user namespace (DEC-179 SBX-2).
pub const SLIRP_PATHS: &[&str] = &["/usr/bin/slirp4netns"];
/// Where the setup child takes the nf_tables `iptables` programs from, and only from
/// there: the IPv4 restore and save, then the IPv6 restore and save.
pub(super) const IPTABLES_PATHS: [&str; 4] = [
    "/usr/sbin/iptables-restore",
    "/usr/sbin/iptables-save",
    "/usr/sbin/ip6tables-restore",
    "/usr/sbin/ip6tables-save",
];
/// What the setup child says when the network namespace it was handed is the one it runs
/// in, so the agent would share this computer's network.
pub const NETWORK_SHARED: &str = "the agent's network is not one of its own";
/// What the setup child says when its input and output are not the two pipes stage 1
/// hands it, as when a caller other than stage 1 starts it (leader 23:16).
pub const SETUP_UNPIPED: &str = "the setup child runs only on the pipes its nest hands it";
/// What the setup child says when bubblewrap's info names a process its parent did not
/// start.
pub const SETUP_NOT_CHILD: &str = "the setup child builds a network only for its nest's child";
/// What the setup child says when its parent is not one of the fixed bubblewraps.
pub const SETUP_NOT_NEST: &str = "the setup child runs only under a fixed bubblewrap";
/// What stage 2 says when no go byte came: bubblewrap starts its command on end of file,
/// so a setup child that ended without one must not leave the agent running on a network
/// whose rules were never read back (rv-code2 SX2-1, rv-sa241 N1).
const ROUTE_NOT_BUILT: &str = "the agent's network was not built, so the agent does not start";
const SANDBOX_RESOLVER: &str = "10.0.2.3";
/// slirp4netns's own network: its gateway, which maps to this computer, and its resolver
/// alias, which maps to this computer's DNS address on any port. No route-port rule reaches
/// it (rv-sa241 LEAD-1).
const SLIRP_NETWORK: &str = "10.0.2.0/24";
/// The loopback addresses: dropped after the `lo` accept, so only the agent's own loopback,
/// where its bridged ports listen, is ever reached at one (rv-sa241 LEAD-1).
const LOOPBACK_NETWORK: &str = "127.0.0.0/8";
/// The link-local addresses, where a cloud VM answers its metadata service: no route port
/// reaches them, even 80 or 443 on any host (rv-sa241 NOTE-1).
const LINK_LOCAL_NETWORK: &str = "169.254.0.0/16";
const TAP: &str = "tap0";
const CHILD_PID_KEY: &[u8] = b"\"child-pid\":";
const INFO_BYTES_MAX: usize = 4096;
const STATUS_BYTES_MAX: u64 = 16 * 1024;
const RESOLVER_FILE_BYTES_MAX: u64 = 64 * 1024;
const RESOLVER_LINE_BYTES_MAX: usize = 256;
const RESOLVER_LINES_MAX: usize = 8;
/// The longest resolver file stage 1 writes: less than a pipe holds, so the write never
/// waits for bubblewrap to read.
const RESOLVER_TEXT_BYTES_MAX: usize = 4096;
const SAVED_BYTES_MAX: usize = 64 * 1024;
const VERSION_BYTES_MAX: usize = 256;
const NUMBER_DIGITS_MAX: usize = 10;
const READY_TIME: Duration = Duration::from_secs(10);
const READY: u8 = b'1';
const BLOCK: u8 = b'1';
const GO: u8 = b'2';
const CHAINS: &str = concat!(
    "*filter\n",
    ":INPUT ACCEPT [0:0]\n",
    ":FORWARD DROP [0:0]\n",
    ":OUTPUT DROP [0:0]\n",
    "-A OUTPUT -o lo -j ACCEPT\n",
);

/// Reads a decimal number written with digits alone, so no sign, space or other spelling
/// passes. Input: the text. Output: the number, or none.
fn number<T: FromStr>(text: &str) -> Option<T> {
    let digits = !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    (digits && text.len() <= NUMBER_DIGITS_MAX)
        .then(|| text.parse().ok())
        .flatten()
}

/// Reads one port the rules let out. Input: the text. Output: a port 1 to 65535, or none.
fn route_port(text: &str) -> Option<u16> {
    number::<u16>(text).filter(|port| *port != 0)
}

/// Splits the route flags that lead the helper's arguments from the rest: `--route` with a
/// port the agent may reach on any host, one to [`REMOTES_MAX`] times, each once. Input:
/// the helper arguments. Output: the ports, empty when no such flag leads, and the
/// arguments after them; or the reason the flags are refused.
pub(super) fn leading(args: Vec<OsString>) -> Result<(Vec<u16>, Vec<OsString>), String> {
    let mut ports: Vec<u16> = Vec::new();
    let mut rest = args.into_iter().peekable();
    while rest.next_if(|flag| flag == ROUTE).is_some() {
        let port = rest
            .next()
            .as_deref()
            .and_then(OsStr::to_str)
            .and_then(route_port);
        let Some(port) = port.filter(|port| !ports.contains(port) && ports.len() < REMOTES_MAX)
        else {
            return Err(format!(
                "{ROUTE} takes 1 to {REMOTES_MAX} ports 1 to 65535, each once"
            ));
        };
        ports.push(port);
    }
    assert!(ports.len() <= REMOTES_MAX);
    Ok((ports, rest.collect()))
}

/// Tells whether a request may come with these route ports and this go descriptor: only a
/// nest whose bubblewrap unshares the network right after its fixed arguments takes ports,
/// so no agent is routed on this computer's network, and only a probe or a nested run
/// inside one takes the descriptor. Inputs: the ports, empty for none, the descriptor and
/// the request. Output: Ok, or the reason.
pub(super) fn fits(
    routes: &[u16],
    started: Option<RawFd>,
    request: &HelperRequest,
) -> Result<(), String> {
    assert!(routes.len() <= REMOTES_MAX);
    if started.is_some() && !routes.is_empty() {
        return Err(format!(
            "{ROUTE_FD} takes no {ROUTE}; stage 1 writes it inside the nest"
        ));
    }
    if started.is_some() {
        return match request {
            HelperRequest::Probe { .. } => Ok(()),
            HelperRequest::Run { policy, .. } if policy.nested => Ok(()),
            _ => Err(format!(
                "{ROUTE_FD} leads only a probe or a nested run in a routed nest"
            )),
        };
    }
    if routes.is_empty() {
        return Ok(());
    }
    match request {
        HelperRequest::Nest { args, .. }
            if args
                .get(NEST_ARGS.len())
                .is_some_and(|arg| arg == UNSHARE_NET) =>
        {
            Ok(())
        }
        _ => Err(format!(
            "{ROUTE} leads only a nest whose bubblewrap starts with {UNSHARE_NET} after its \
             fixed arguments"
        )),
    }
}

/// Reads the setup child's arguments, `--egress` first: the stage-1 process it dies with,
/// then the ports the rules let out, all digits alone. Input: the helper arguments. Output:
/// the request, or the reason they are refused.
pub(super) fn request(args: Vec<OsString>) -> Result<HelperRequest, String> {
    assert!(args.first().is_some_and(|flag| flag == EGRESS));
    let refused =
        || format!("{EGRESS} takes a process id above 1, then 1 to {REMOTES_MAX} ports, each once");
    let words: Option<Vec<&str>> = args[1..].iter().map(|arg| arg.to_str()).collect();
    let Some((parent, ports)) = words.as_deref().and_then(<[&str]>::split_first) else {
        return Err(refused());
    };
    let parent = number::<u32>(parent)
        .filter(|parent| *parent > 1 && i32::try_from(*parent).is_ok())
        .ok_or_else(refused)?;
    let ports: Vec<u16> = ports
        .iter()
        .map(|port| route_port(port))
        .collect::<Option<_>>()
        .ok_or_else(refused)?;
    let twice = ports
        .iter()
        .enumerate()
        .any(|(at, port)| ports[..at].contains(port));
    if ports.is_empty() || ports.len() > REMOTES_MAX || twice {
        return Err(refused());
    }
    Ok(HelperRequest::Egress { parent, ports })
}

/// What stage 1 keeps open for bubblewrap until it becomes it: the write end bubblewrap
/// writes its info to, the read end its child blocks on, a second read end of that pipe
/// for stage 2's go byte, and the read end of the resolver file. Each closes on exec until
/// [`Ends::hand_over`].
pub(super) struct Ends {
    info: PipeWriter,
    block: PipeReader,
    go: PipeReader,
    resolver: PipeReader,
}

impl Ends {
    /// Lets bubblewrap inherit the four ends, right before stage 1 becomes it and after
    /// every child stage 1 starts, so no other process holds them. Output: Ok, or the
    /// reason.
    pub(super) fn hand_over(&self) -> Result<(), String> {
        let ends = [
            self.info.as_fd(),
            self.block.as_fd(),
            self.go.as_fd(),
            self.resolver.as_fd(),
        ];
        for end in ends {
            rustix::io::fcntl_setfd(end, rustix::io::FdFlags::empty())
                .map_err(|error| format!("the agent's network: {error}"))?;
        }
        Ok(())
    }
}

/// Opens the routed network from stage 1: the info, block and resolver pipes, the resolver
/// file written whole into its pipe, the arguments with the descriptor flags ([`insert`])
/// and the go flag for this helper ([`inner`]), then the setup child started on the other
/// two ends with this process as the one it dies with. Inputs: the route ports and the
/// nest's arguments. Output: the ends and the arguments; or the reason.
pub(super) fn open(ports: &[u16], args: &[OsString]) -> Result<(Ends, Vec<OsString>), String> {
    assert!(!ports.is_empty() && ports.len() <= REMOTES_MAX);
    let failed = |error: std::io::Error| format!("the agent's network: {error}");
    let (info_read, info) = std::io::pipe().map_err(failed)?;
    let (block, block_write) = std::io::pipe().map_err(failed)?;
    let go = block.try_clone().map_err(failed)?;
    let (resolver, mut resolver_write) = std::io::pipe().map_err(failed)?;
    let text = resolver_text(&host_resolver());
    assert!(text.len() <= RESOLVER_TEXT_BYTES_MAX);
    resolver_write.write_all(text.as_bytes()).map_err(failed)?;
    drop(resolver_write);
    let fds = [info.as_raw_fd(), block.as_raw_fd(), resolver.as_raw_fd()];
    if fds.iter().any(|fd| *fd <= 2) || go.as_raw_fd() <= 2 {
        return Err("the agent's network: a standard descriptor was closed".into());
    }
    let helper = std::env::current_exe().map_err(failed)?;
    let args = inner(&insert(args, fds), go.as_raw_fd(), &helper)?;
    let mut setup = Command::new(&helper);
    setup
        .arg(EGRESS)
        .arg(std::process::id().to_string())
        .args(ports.iter().map(u16::to_string))
        .env_clear()
        .stdin(Stdio::from(info_read))
        .stdout(Stdio::from(block_write))
        .stderr(Stdio::inherit());
    crate::spawn_gate::spawn(&mut setup).map_err(failed)?;
    drop(setup);
    Ok((
        Ends {
            info,
            block,
            go,
            resolver,
        },
        args,
    ))
}

/// Reads this computer's resolver file for its `search` and `options` lines, at most
/// [`RESOLVER_FILE_BYTES_MAX`] of it. No input. Output: the text, empty when it cannot be
/// read.
fn host_resolver() -> String {
    let mut text = String::new();
    let read = fs::File::open(RESOLV_CONF)
        .and_then(|file| file.take(RESOLVER_FILE_BYTES_MAX).read_to_string(&mut text));
    if read.is_err() {
        text.clear();
    }
    text
}

/// The setup child's work, outside the sandbox: it joins the agent's namespaces, starts
/// slirp4netns, applies the rules and reads them back, drops its capabilities, and only
/// then writes two bytes, one for bubblewrap's block and the go byte stage 2 waits on
/// ([`started`]); a refused or failed setup writes neither, so stage 2 refuses, and a
/// failed one also kills the waiting agent ([`stop`]). Inputs: the stage-1 process id and
/// the route ports. Output: the exit code, 3 when the network could not be built.
pub(super) fn setup(parent: u32, ports: &[u16]) -> ExitCode {
    assert!(parent > 1 && !ports.is_empty() && ports.len() <= REMOTES_MAX);
    let child = match nest_child(parent) {
        Ok(child) => child,
        Err(reason) => {
            eprintln!("crazyagent-landlock: {reason}");
            return ExitCode::from(3);
        }
    };
    let built = build(child, ports).and_then(|network| {
        let mut block = std::io::stdout().lock();
        block
            .write_all(&[BLOCK, GO])
            .and_then(|()| block.flush())
            .map_err(|error| format!("the block pipe: {error}"))?;
        Ok(network)
    });
    match built {
        Ok(mut network) => {
            let ended = network.process.wait();
            assert!(ended.is_ok(), "slirp4netns is this process's child to reap");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!("crazyagent-landlock: {reason}");
            stop(parent, child);
            ExitCode::from(3)
        }
    }
}

/// Checks that the setup child runs as stage 1's child for stage 1's own bubblewrap: its
/// input and output are pipes, it dies with `parent`, bubblewrap's info names a child of
/// `parent`, and `parent` runs one of the fixed bubblewraps. A refusal kills nothing and
/// writes no go byte, so stage 2 starts no agent. Input: the stage-1 process id. Output:
/// bubblewrap's child, or the reason.
fn nest_child(parent: u32) -> Result<u32, String> {
    let fifo = |stat: rustix::io::Result<rustix::fs::Stat>| {
        stat.is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::Fifo)
    };
    if !fifo(rustix::fs::fstat(std::io::stdin())) || !fifo(rustix::fs::fstat(std::io::stdout())) {
        return Err(SETUP_UNPIPED.into());
    }
    die_with_parent(parent).map_err(|_| SETUP_NOT_CHILD.to_string())?;
    let child = child_pid(&mut std::io::stdin().lock())?;
    if parent_of(child) != Some(parent) {
        return Err(SETUP_NOT_CHILD.into());
    }
    if !nest_program(parent) {
        return Err(SETUP_NOT_NEST.into());
    }
    Ok(child)
}

/// Reads bubblewrap's info from its pipe until it names its child, at most
/// [`INFO_BYTES_MAX`] of it, without waiting for the pipe to close. Input: the pipe.
/// Output: the child's process id, or the reason.
fn child_pid(input: &mut impl Read) -> Result<u32, String> {
    let mut text: Vec<u8> = Vec::with_capacity(INFO_BYTES_MAX);
    let mut chunk = [0u8; 512];
    loop {
        if let Some(named) = named_child(&text) {
            return named;
        }
        if text.len() >= INFO_BYTES_MAX {
            return Err(SETUP_NOT_CHILD.into());
        }
        let read = match input.read(&mut chunk) {
            Ok(0) => return Err(SETUP_NOT_CHILD.into()),
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("bubblewrap's info: {error}")),
        };
        let room = INFO_BYTES_MAX - text.len();
        text.extend_from_slice(&chunk[..read.min(room)]);
    }
}

/// Finds the child bubblewrap names in its info text: the digits after `"child-pid":`,
/// once a character that is not a digit ends them. Input: the text read so far. Output:
/// none while the number may still go on, else the process id above 1 or the reason.
fn named_child(text: &[u8]) -> Option<Result<u32, String>> {
    let at = text
        .windows(CHILD_PID_KEY.len())
        .position(|window| window == CHILD_PID_KEY)?;
    let rest = &text[at + CHILD_PID_KEY.len()..];
    let start = rest.iter().position(|byte| *byte != b' ')?;
    let digits = rest[start..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if start + digits >= rest.len() {
        return None;
    }
    let child = std::str::from_utf8(&rest[start..start + digits])
        .ok()
        .and_then(number::<u32>)
        .filter(|child| *child > 1 && i32::try_from(*child).is_ok());
    Some(child.ok_or_else(|| SETUP_NOT_CHILD.to_string()))
}

/// Reads the parent of a process from `/proc`. Input: its process id. Output: the parent's
/// process id, or none when the process is gone or the file cannot be read.
fn parent_of(child: u32) -> Option<u32> {
    let mut status = String::new();
    fs::File::open(format!("/proc/{child}/status"))
        .ok()?
        .take(STATUS_BYTES_MAX)
        .read_to_string(&mut status)
        .ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .and_then(|ppid| number::<u32>(ppid.trim()))
}

/// Tells whether a process runs one of the fixed bubblewraps a nest starts: the file it
/// runs is the same file, by device and inode, as one at [`BWRAP_PATHS`] that
/// [`trusted_file`] accepts. Input: its process id. Output: true only then.
fn nest_program(parent: u32) -> bool {
    let Ok(running) = fs::metadata(format!("/proc/{parent}/exe")) else {
        return false;
    };
    BWRAP_PATHS
        .iter()
        .filter_map(|path| trusted_file(Path::new(path)))
        .filter_map(|file| file.metadata().ok())
        .any(|held| (held.dev(), held.ino()) == (running.dev(), running.ino()))
}

/// slirp4netns as the setup child started it, with the write end of its exit pipe: it
/// ends once that end closes, as when the setup child dies with stage 1, and is killed
/// when dropped.
struct Slirp {
    process: Child,
    _exit: PipeWriter,
}

impl Drop for Slirp {
    /// Kills and reaps slirp4netns unless it has already ended. No input. No output.
    fn drop(&mut self) {
        if self.process.kill().is_ok() {
            let reaped = self.process.wait();
            assert!(
                reaped.is_ok(),
                "slirp4netns is this process's child to reap"
            );
        }
    }
}

/// Builds the agent's network: checks that its network namespace is its own, takes the
/// tools from their fixed places, starts slirp4netns, joins the agent's user and network
/// namespaces, applies the IPv4 and IPv6 rules with exactly the two network capabilities
/// in its ambient set and reads them back, then drops every capability. Inputs: the
/// agent's process id and the route ports. Output: slirp4netns, or the reason.
fn build(child: u32, ports: &[u16]) -> Result<Slirp, String> {
    let failed = |error: std::io::Error| format!("the agent's network: {error}");
    let net = fs::File::open(format!("/proc/{child}/ns/net")).map_err(failed)?;
    let user = fs::File::open(format!("/proc/{child}/ns/user")).map_err(failed)?;
    let own = fs::metadata("/proc/thread-self/ns/net").map_err(failed)?;
    let theirs = net.metadata().map_err(failed)?;
    if (own.dev(), own.ino()) == (theirs.dev(), theirs.ino()) {
        return Err(NETWORK_SHARED.into());
    }
    tools()?;
    let slirp = start_slirp(child)?;
    let joined = |kind: LinkNameSpaceType, file: &fs::File| {
        rustix::thread::move_into_link_name_space(file.as_fd(), Some(kind))
            .map_err(|error| format!("the agent's namespaces: {error}"))
    };
    joined(LinkNameSpaceType::User, &user)?;
    joined(LinkNameSpaceType::Network, &net)?;
    raise_ambient()?;
    let [restore, save, restore_v6, save_v6] = IPTABLES_PATHS;
    let applied = apply(restore, save, &egress_rules(ports))
        .and_then(|()| apply(restore_v6, save_v6, &egress_rules_v6()));
    drop_capabilities()?;
    applied?;
    Ok(slirp)
}

/// Finds slirp4netns at its fixed place, a file [`trusted_file`] accepts. No input.
/// Output: its path, or the reason.
fn slirp_path() -> Result<&'static str, String> {
    SLIRP_PATHS
        .iter()
        .copied()
        .find(|path| trusted_file(Path::new(path)).is_some())
        .ok_or_else(|| "slirp4netns is not at its fixed place or others may change it".into())
}

/// Finds the tools at their fixed places, each a file [`trusted_file`] accepts, with both
/// restore programs on the nf_tables backend ([`nf_tables`]). No input. Output: Ok, or the
/// reason.
fn tools() -> Result<(), String> {
    slirp_path()?;
    for path in IPTABLES_PATHS {
        if trusted_file(Path::new(path)).is_none() {
            return Err(format!("{path} is not there or others may change it"));
        }
    }
    for restore in [IPTABLES_PATHS[0], IPTABLES_PATHS[2]] {
        let mut asked = Command::new(restore);
        asked
            .arg("--version")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let version =
            crate::spawn_gate::output(&mut asked).map_err(|error| format!("{restore}: {error}"))?;
        if !version.status.success() || !nf_tables(&String::from_utf8_lossy(&version.stdout)) {
            return Err(format!("{restore} is not the nf_tables iptables"));
        }
    }
    Ok(())
}

/// Starts slirp4netns for the agent's network, from this computer's network, through this
/// helper's [`SLIRP`] mode, which confines it first ([`slirp`]), and waits up to
/// [`READY_TIME`] for its ready byte. Its input is the read end of its exit pipe; its ready
/// end is the only other descriptor it inherits. Input: the agent's process id. Output:
/// slirp4netns, or the reason.
fn start_slirp(child: u32) -> Result<Slirp, String> {
    let failed = |error: std::io::Error| format!("slirp4netns: {error}");
    let helper = std::env::current_exe().map_err(failed)?;
    let (exit_read, exit_write) = std::io::pipe().map_err(failed)?;
    let (ready, handed) = crate::spawn_gate::unix_pair_std().map_err(failed)?;
    ready.set_read_timeout(Some(READY_TIME)).map_err(failed)?;
    let handed = OwnedFd::from(handed);
    let fd = handed.as_raw_fd();
    assert!(fd > 2);
    let mut command = Command::new(helper);
    command
        .arg(SLIRP)
        .arg(child.to_string())
        .arg(fd.to_string())
        .env_clear()
        .stdin(Stdio::from(exit_read))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let process = crate::spawn_gate::spawn_handing(&mut command, handed).map_err(failed)?;
    drop(command);
    let started = Slirp {
        process,
        _exit: exit_write,
    };
    let mut byte = [0u8; 1];
    match (&ready).read(&mut byte) {
        Ok(1) if byte[0] == READY => Ok(started),
        _ => Err("slirp4netns did not start".into()),
    }
}

/// Opens the agent's user and network namespaces while Landlock still lets it, then confines
/// this thread and its children under no new privileges: read and execute beneath
/// [`SLIRP_READ`], all rights on [`TUN`], signals scoped to its domain where the kernel can,
/// and no Unix-domain socket, the way to the engine's sockets by path. Input: the agent's
/// process id. Output: the two namespaces, user first, or the reason.
pub(super) fn confine_slirp(child: u32) -> Result<[OwnedFd; 2], String> {
    assert!(child > 1);
    let failed = |error: std::io::Error| format!("slirp4netns: {error}");
    let user = fs::File::open(format!("/proc/{child}/ns/user")).map_err(failed)?;
    let net = fs::File::open(format!("/proc/{child}/ns/net")).map_err(failed)?;
    let refused = |error: RulesetError| format!("slirp4netns's rules: {error}");
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(FS_ABI))
        .map_err(refused)?
        .create()
        .map_err(refused)?
        .add_rules(path_beneath_rules(SLIRP_READ, AccessFs::from_read(FS_ABI)))
        .map_err(refused)?
        .add_rules(path_beneath_rules([TUN], AccessFs::from_all(FS_ABI)))
        .map_err(refused)?
        .restrict_self()
        .map_err(refused)?;
    if !accepted(&status.ruleset, status.no_new_privs) {
        return Err(format!(
            "this kernel cannot hold slirp4netns's rules ({:?})",
            status.ruleset
        ));
    }
    super::scope_signals().map_err(refused)?;
    super::refuse_sockets(false, false)?;
    Ok([user.into(), net.into()])
}

/// The helper's [`SLIRP`] mode, the setup child's hop to slirp4netns: it confines itself
/// ([`confine_slirp`]), lets the two namespaces through exec, and becomes slirp4netns, which
/// joins them by those descriptors with its own seccomp filter and no sandbox of its own (a
/// user who is not root cannot start it). Inputs: the agent's process id and the ready
/// descriptor. Output: 3 when it cannot be confined, 126 when slirp4netns cannot start.
pub(super) fn slirp(child: u32, ready: RawFd) -> ExitCode {
    assert!(child > 1 && ready > 2);
    let threads = fs::read_dir("/proc/self/task").map(Iterator::count);
    assert!(
        threads.is_ok_and(|threads| threads == 1),
        "the rules hold the whole slirp process"
    );
    let confined = slirp_path().and_then(|program| {
        let namespaces = confine_slirp(child)?;
        for namespace in &namespaces {
            rustix::io::fcntl_setfd(namespace, rustix::io::FdFlags::empty())
                .map_err(|error| format!("slirp4netns: {error}"))?;
        }
        Ok((program, namespaces))
    });
    let (program, [user, net]) = match confined {
        Ok(confined) => confined,
        Err(reason) => {
            eprintln!("crazyagent-landlock: {reason}");
            return ExitCode::from(3);
        }
    };
    let error = Command::new(program)
        .args(slirp_args(user.as_raw_fd(), net.as_raw_fd(), ready))
        .exec();
    eprintln!("crazyagent-landlock: cannot start {program}: {error}");
    ExitCode::from(126)
}

/// The arguments slirp4netns starts with: the agent's namespaces by this process's
/// descriptors, its exit on the input pipe, its ready byte on `ready`, its seccomp filter,
/// and no `--enable-sandbox`. Inputs: the user and network namespace descriptors and the
/// ready descriptor. Output: the arguments.
fn slirp_args(user: RawFd, net: RawFd, ready: RawFd) -> Vec<String> {
    assert!(user > 2 && net > 2 && ready > 2);
    vec![
        "--configure".into(),
        "--mtu=65520".into(),
        "--disable-host-loopback".into(),
        "--enable-seccomp".into(),
        "--netns-type=path".into(),
        format!("--userns-path=/proc/self/fd/{user}"),
        "--exit-fd=0".into(),
        format!("--ready-fd={ready}"),
        format!("/proc/self/fd/{net}"),
        TAP.into(),
    ]
}

/// Reads the slirp mode's arguments, `--slirp` first: the agent's process id above 1, then
/// the ready descriptor above 2, digits alone. Input: the helper arguments. Output: the
/// request, or the reason they are refused.
pub(super) fn slirp_request(args: Vec<OsString>) -> Result<HelperRequest, String> {
    assert!(args.first().is_some_and(|flag| flag == SLIRP));
    let refused = || format!("{SLIRP} takes a process id above 1, then a descriptor above 2");
    let [_, child, ready] = args.as_slice() else {
        return Err(refused());
    };
    let child = child
        .to_str()
        .and_then(number::<u32>)
        .filter(|child| *child > 1 && i32::try_from(*child).is_ok())
        .ok_or_else(refused)?;
    let ready = ready
        .to_str()
        .and_then(number::<RawFd>)
        .filter(|ready| *ready > 2)
        .ok_or_else(refused)?;
    Ok(HelperRequest::Slirp { child, ready })
}

/// Raises exactly [`ambient_wanted`] into the ambient set, after keeping only those in the
/// permitted, effective and inheritable sets, so the `iptables` programs keep them across
/// exec and nothing else. No input. Output: Ok, or the reason.
fn raise_ambient() -> Result<(), String> {
    let wanted = ambient_wanted();
    let failed = |error: rustix::io::Errno| format!("network capabilities: {error}");
    let held = rustix::thread::capabilities(None).map_err(failed)?;
    if !held.permitted.contains(wanted) {
        return Err("the agent's user namespace gave no network capabilities".into());
    }
    let kept = CapabilitySets {
        effective: wanted,
        permitted: wanted,
        inheritable: wanted,
    };
    rustix::thread::set_capabilities(None, kept).map_err(failed)?;
    for capability in wanted.iter() {
        rustix::thread::configure_capability_in_ambient_set(capability, true).map_err(failed)?;
    }
    let ambient = ambient_held().ok_or("network capabilities: the ambient set is unreadable")?;
    assert_eq!(ambient, wanted);
    Ok(())
}

/// Reads this thread's ambient set from `/proc`. No input. Output: the set, or none when
/// it cannot be read.
fn ambient_held() -> Option<CapabilitySet> {
    let mut status = String::new();
    fs::File::open("/proc/thread-self/status")
        .ok()?
        .take(STATUS_BYTES_MAX)
        .read_to_string(&mut status)
        .ok()?;
    let hex = status
        .lines()
        .find_map(|line| line.strip_prefix("CapAmb:"))?;
    let bits = u64::from_str_radix(hex.trim(), 16).ok()?;
    Some(CapabilitySet::from_bits_retain(bits))
}

/// Drops every capability: the ambient set, then the effective, permitted and inheritable
/// sets. No input. Output: Ok once all four are empty, or the reason.
fn drop_capabilities() -> Result<(), String> {
    let failed = |error: rustix::io::Errno| format!("network capabilities: {error}");
    rustix::thread::clear_ambient_capability_set().map_err(failed)?;
    let none = CapabilitySets {
        effective: CapabilitySet::empty(),
        permitted: CapabilitySet::empty(),
        inheritable: CapabilitySet::empty(),
    };
    rustix::thread::set_capabilities(None, none).map_err(failed)?;
    assert_eq!(rustix::thread::capabilities(None).map_err(failed)?, none);
    Ok(())
}

/// Applies one family's rules and reads them back: the restore program takes the rules on
/// its input and replaces the filter table, then the save program prints that table, at
/// most [`SAVED_BYTES_MAX`] of it, and it must match the rules ([`canonical_rules`]).
/// Inputs: the two programs and the rules. Output: Ok, or the reason.
fn apply(restore: &str, save: &str, rules: &str) -> Result<(), String> {
    let mut command = Command::new(restore);
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let mut running =
        crate::spawn_gate::spawn(&mut command).map_err(|error| format!("{restore}: {error}"))?;
    let written = running
        .stdin
        .take()
        .map(|mut input| input.write_all(rules.as_bytes()));
    let status = running
        .wait()
        .map_err(|error| format!("{restore}: {error}"))?;
    if !matches!(written, Some(Ok(()))) || !status.success() {
        return Err(format!("{restore} did not take the rules"));
    }
    let mut reading = Command::new(save);
    reading
        .args(["-t", "filter"])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let saved =
        crate::spawn_gate::output(&mut reading).map_err(|error| format!("{save}: {error}"))?;
    if !saved.status.success() || saved.stdout.len() > SAVED_BYTES_MAX {
        return Err(format!("{save} did not print the rules"));
    }
    let saved = String::from_utf8(saved.stdout).map_err(|_| format!("{save} printed non-text"))?;
    if canonical_rules(&saved) != canonical_rules(rules) {
        return Err(format!("{save} read back other rules than were applied"));
    }
    Ok(())
}

/// Kills the agent bubblewrap holds on its block pipe after a failed setup, so it never
/// starts, and only while it is still `parent`'s child, so no other process that took its
/// id is touched. Inputs: the stage-1 process id and the agent's. No output.
fn stop(parent: u32, child: u32) {
    if parent_of(child) != Some(parent) {
        return;
    }
    let Some(pid) = i32::try_from(child)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return;
    };
    if let Err(error) = rustix::process::kill_process(pid, rustix::process::Signal::KILL) {
        eprintln!("crazyagent-landlock: the agent was not stopped: {error}");
    }
}

/// The IPv4 rules inside the agent's network, as `iptables-restore` reads them and in the
/// form `iptables-save` prints them: loopback for the bridged ports, TCP 53 to slirp's
/// resolver alone, then nothing to slirp's network ([`SLIRP_NETWORK`]), a loopback address
/// off `lo` ([`LOOPBACK_NETWORK`]), a link-local one ([`LINK_LOCAL_NETWORK`]) or a cloud's
/// metadata service outside it ([`crate::sandbox::CLOUD_METADATA`], each IPv4 one, in order),
/// then out through `tap0` to each port. Input: the ports on any host. Output: the rules.
fn egress_rules(any: &[u16]) -> String {
    assert!(!any.is_empty() && any.len() <= REMOTES_MAX);
    assert!(!any.contains(&0));
    let mut rules = String::from(CHAINS);
    let _ = writeln!(
        rules,
        "-A OUTPUT -d {SANDBOX_RESOLVER}/32 -o {TAP} -p tcp -m tcp --dport 53 -j ACCEPT"
    );
    let _ = writeln!(rules, "-A OUTPUT -d {SLIRP_NETWORK} -j DROP");
    let _ = writeln!(rules, "-A OUTPUT -d {LOOPBACK_NETWORK} -j DROP");
    let _ = writeln!(rules, "-A OUTPUT -d {LINK_LOCAL_NETWORK} -j DROP");
    let mut metadata = 0;
    for address in crate::sandbox::CLOUD_METADATA {
        if let std::net::IpAddr::V4(address) = address {
            let _ = writeln!(rules, "-A OUTPUT -d {address}/32 -j DROP");
            metadata += 1;
        }
    }
    assert!(metadata > 0);
    assert_eq!(rules.matches(" -j ACCEPT\n").count(), 2);
    assert_eq!(rules.matches(" -j DROP\n").count(), 3 + metadata);
    assert_eq!(rules.matches(SANDBOX_RESOLVER).count(), 1);
    for port in any {
        let _ = writeln!(
            rules,
            "-A OUTPUT -o {TAP} -p tcp -m tcp --dport {port} -j ACCEPT"
        );
    }
    rules.push_str("COMMIT\n");
    rules
}

/// The IPv6 rules inside the agent's network: out over loopback only, since slirp4netns
/// runs without IPv6. No input. Output: the rules.
fn egress_rules_v6() -> String {
    format!("{CHAINS}COMMIT\n")
}

/// The rules as the read-back compares them: the table, chain policy and `-A` lines and
/// `COMMIT`, without `#` lines and without the packet and byte counters. Input: rules as
/// written or as `iptables-save` printed them. Output: the canonical text.
fn canonical_rules(text: &str) -> String {
    assert!(text.len() <= SAVED_BYTES_MAX);
    let mut kept = String::with_capacity(text.len());
    for line in text.lines().map(str::trim_end) {
        let line = match line.as_bytes().first() {
            Some(b'*' | b'-') => line,
            Some(b':') => line
                .rsplit_once(' ')
                .filter(|(_, counters)| counters.starts_with('[') && counters.ends_with(']'))
                .map_or(line, |(chain, _)| chain),
            _ if line == "COMMIT" => line,
            _ => continue,
        };
        kept.push_str(line);
        kept.push('\n');
    }
    kept
}

/// The capabilities the setup child raises into its ambient set before it starts the
/// `iptables` programs, which a non-zero uid would otherwise lose on exec. No input.
/// Output: exactly CAP_NET_ADMIN and CAP_NET_RAW.
fn ambient_wanted() -> CapabilitySet {
    CapabilitySet::NET_ADMIN | CapabilitySet::NET_RAW
}

/// The resolver file bound over `/etc/resolv.conf` inside the agent's network: one
/// nameserver, slirp's resolver, then the host's `search` and `options` lines and
/// `options use-vc`, since the agent looks names up over TCP. Input: the host's file, empty
/// when it cannot be read. Output: the file.
fn resolver_text(host: &str) -> String {
    let mut text = format!("nameserver {SANDBOX_RESOLVER}\n");
    let kept = host
        .lines()
        .filter_map(|line| {
            let mut words = line.split_ascii_whitespace();
            let key = words
                .next()
                .filter(|key| *key == "search" || *key == "options")?;
            let values: Vec<&str> = words.collect();
            let line = format!("{key} {}", values.join(" "));
            (!values.is_empty() && line.len() <= RESOLVER_LINE_BYTES_MAX).then_some(line)
        })
        .take(RESOLVER_LINES_MAX);
    for line in kept {
        text.push_str(&line);
        text.push('\n');
    }
    text.push_str("options use-vc\n");
    assert!(text.len() <= RESOLVER_TEXT_BYTES_MAX);
    text
}

/// Tells whether an `iptables` program uses the nf_tables backend, from what `--version`
/// printed: the legacy backend takes a lock the setup child cannot hold. Input: the text.
/// Output: true only for nf_tables.
fn nf_tables(version: &str) -> bool {
    version.len() <= VERSION_BYTES_MAX && version.trim_end().ends_with(" (nf_tables)")
}

/// Writes stage 1's descriptor flags into the nest's bubblewrap arguments, right after
/// `--unshare-net`: the info and block pipes and the resolver file bound over
/// `/etc/resolv.conf`. Inputs: the nest's arguments and the three descriptors. Output: the
/// arguments with the flags, which [`placed`] accepts.
fn insert(args: &[OsString], fds: [RawFd; 3]) -> Vec<OsString> {
    assert!(fds.iter().all(|fd| *fd > 2));
    assert!(fds[0] != fds[1] && fds[1] != fds[2] && fds[0] != fds[2]);
    let at = NEST_ARGS.len();
    assert!(args.get(at).is_some_and(|arg| arg == UNSHARE_NET));
    let mut written = args[..=at].to_vec();
    for (flag, fd) in DESCRIPTOR_FLAGS.into_iter().zip(fds) {
        written.push(flag.into());
        written.push(fd.to_string().into());
    }
    written.push(RESOLV_CONF.into());
    written.extend_from_slice(&args[at + 1..]);
    assert_eq!(written.len(), args.len() + 2 * DESCRIPTOR_FLAGS.len() + 1);
    written
}

/// Checks bubblewrap's arguments as stage 1 starts it for a routed agent: [`NEST_ARGS`],
/// `--unshare-net`, then the descriptor flags in order, each with its own descriptor above
/// 2, and the resolver file bound over `/etc/resolv.conf` alone, with no descriptor flag
/// among the options after them. Input: the arguments. Output: Ok, or the reason.
pub(super) fn placed(args: &[OsString]) -> Result<(), String> {
    let refused = || Err("bubblewrap's descriptor flags are not the ones stage 1 writes".into());
    let at = NEST_ARGS.len();
    let fixed = args
        .iter()
        .zip(NEST_ARGS)
        .all(|(arg, fixed)| arg.as_os_str() == OsStr::new(fixed));
    let Some(written) = args.get(at..at + 2 * DESCRIPTOR_FLAGS.len() + 2) else {
        return refused();
    };
    if !fixed || written[0] != UNSHARE_NET || written[written.len() - 1] != RESOLV_CONF {
        return refused();
    }
    let mut fds: Vec<RawFd> = Vec::with_capacity(DESCRIPTOR_FLAGS.len());
    for (pair, flag) in written[1..written.len() - 1]
        .as_chunks::<2>()
        .0
        .iter()
        .zip(DESCRIPTOR_FLAGS)
    {
        let fd = pair[1]
            .to_str()
            .and_then(number::<RawFd>)
            .filter(|fd| *fd > 2 && !fds.contains(fd));
        match fd {
            Some(fd) if pair[0] == flag => fds.push(fd),
            _ => return refused(),
        }
    }
    let named = args[at + written.len()..]
        .iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| DESCRIPTOR_FLAGS.iter().any(|flag| arg == flag));
    if named {
        return refused();
    }
    assert_eq!(fds.len(), DESCRIPTOR_FLAGS.len());
    Ok(())
}

/// Writes the route flag for the helper bubblewrap starts, right after its name: the
/// descriptor the go byte arrives on, which [`handed`] reads back inside the nest. A routed
/// nest whose command is not this helper is refused, bridged or not, so no other program
/// starts there without the go byte. Inputs: the nest's arguments, the descriptor and this
/// helper. Output: the arguments with the flag, or the reason.
pub(super) fn inner(args: &[OsString], fd: RawFd, helper: &Path) -> Result<Vec<OsString>, String> {
    assert!(fd > 2);
    let Some(at) = super::bridge::own_command(args, helper) else {
        return Err(format!(
            "{ROUTE_FD} needs this helper as the nest's command"
        ));
    };
    let mut written = args[..=at].to_vec();
    written.push(ROUTE_FD.into());
    written.push(fd.to_string().into());
    written.extend_from_slice(&args[at + 1..]);
    assert_eq!(written.len(), args.len() + 2);
    assert_eq!(written[at + 1], ROUTE_FD);
    Ok(written)
}

/// Splits the route flag that may follow the bridge flags at the head of stage 2's
/// arguments: `--route-fd` with a descriptor above 2, at most once. Input: the arguments.
/// Output: the descriptor, none when no such flag leads, and the arguments after it; or the
/// reason it is refused.
pub(super) fn handed(args: Vec<OsString>) -> Result<(Option<RawFd>, Vec<OsString>), String> {
    let mut rest = args.into_iter().peekable();
    if rest.next_if(|flag| flag == ROUTE_FD).is_none() {
        return Ok((None, rest.collect()));
    }
    let fd = rest
        .next()
        .as_deref()
        .and_then(OsStr::to_str)
        .and_then(number::<RawFd>)
        .filter(|fd| *fd > 2)
        .ok_or_else(|| format!("{ROUTE_FD} needs a descriptor above 2"))?;
    Ok((Some(fd), rest.collect()))
}

/// Waits inside the nest for the go byte, which the setup child writes only once the
/// agent's rules are read back and its capabilities dropped, then closes the descriptor so
/// the harness never holds it. Input: the descriptor `--route-fd` names. Output: Ok on the
/// go byte; [`ROUTE_NOT_BUILT`] on end of file, an error or any other byte.
pub(super) fn started(fd: RawFd) -> Result<(), String> {
    assert!(fd > 2);
    let mut byte = [0u8; 1];
    let read = loop {
        match nix::unistd::read(fd, &mut byte) {
            Err(nix::errno::Errno::EINTR) => {}
            read => break read,
        }
    };
    let closed = nix::unistd::close(fd);
    match (read, closed) {
        (Ok(1), Ok(())) if byte[0] == GO => Ok(()),
        _ => Err(ROUTE_NOT_BUILT.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_PORT: &str = concat!(
        "*filter\n",
        ":INPUT ACCEPT [0:0]\n",
        ":FORWARD DROP [0:0]\n",
        ":OUTPUT DROP [0:0]\n",
        "-A OUTPUT -o lo -j ACCEPT\n",
        "-A OUTPUT -d 10.0.2.3/32 -o tap0 -p tcp -m tcp --dport 53 -j ACCEPT\n",
        "-A OUTPUT -d 10.0.2.0/24 -j DROP\n",
        "-A OUTPUT -d 127.0.0.0/8 -j DROP\n",
        "-A OUTPUT -d 169.254.0.0/16 -j DROP\n",
        "-A OUTPUT -d 100.100.100.200/32 -j DROP\n",
        "-A OUTPUT -d 168.63.129.16/32 -j DROP\n",
        "-A OUTPUT -o tap0 -p tcp -m tcp --dport 443 -j ACCEPT\n",
        "COMMIT\n",
    );

    const V6: &str = concat!(
        "*filter\n",
        ":INPUT ACCEPT [0:0]\n",
        ":FORWARD DROP [0:0]\n",
        ":OUTPUT DROP [0:0]\n",
        "-A OUTPUT -o lo -j ACCEPT\n",
        "COMMIT\n",
    );

    const SAVED: &str = concat!(
        "# Generated by iptables-save v1.8.10 (nf_tables) on Sun Nov 15 23:10:00 2026\n",
        "*filter\n",
        ":INPUT ACCEPT [4:240]\n",
        ":FORWARD DROP [0:0]\n",
        ":OUTPUT DROP [12:720]\n",
        "-A OUTPUT -o lo -j ACCEPT\n",
        "-A OUTPUT -d 10.0.2.3/32 -o tap0 -p tcp -m tcp --dport 53 -j ACCEPT\n",
        "-A OUTPUT -d 10.0.2.0/24 -j DROP\n",
        "-A OUTPUT -d 127.0.0.0/8 -j DROP\n",
        "-A OUTPUT -d 169.254.0.0/16 -j DROP\n",
        "-A OUTPUT -d 100.100.100.200/32 -j DROP\n",
        "-A OUTPUT -d 168.63.129.16/32 -j DROP\n",
        "-A OUTPUT -o tap0 -p tcp -m tcp --dport 443 -j ACCEPT\n",
        "COMMIT\n",
        "# Completed on Sun Nov 15 23:10:00 2026\n",
    );

    fn words(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    fn nest_args(after: &[&str]) -> Vec<OsString> {
        let mut args = words(&NEST_ARGS);
        args.extend(words(after));
        args
    }

    #[test]
    fn the_rules_let_an_agent_out_only_through_its_ports_and_tcp_53_to_the_sandbox_resolver() {
        assert_eq!(egress_rules(&[443]), ONE_PORT);
        let second = "-A OUTPUT -o tap0 -p tcp -m tcp --dport 8443 -j ACCEPT\nCOMMIT\n";
        assert_eq!(
            egress_rules(&[443, 8443]),
            ONE_PORT.replace("COMMIT\n", second)
        );
        assert_eq!(egress_rules_v6(), V6);
        for rules in [
            egress_rules(&[443]),
            egress_rules(&[443, 8443]),
            egress_rules_v6(),
        ] {
            assert!(rules.ends_with("COMMIT\n"), "{rules}");
            assert!(rules.contains("\n:OUTPUT DROP [0:0]\n"), "{rules}");
            assert!(!rules.contains("udp"), "{rules}");
        }
        let dns: Vec<String> = egress_rules(&[443, 8443])
            .lines()
            .filter(|line| line.contains("--dport 53 "))
            .map(str::to_string)
            .collect();
        assert_eq!(
            dns,
            ["-A OUTPUT -d 10.0.2.3/32 -o tap0 -p tcp -m tcp --dport 53 -j ACCEPT"]
        );
    }

    #[test]
    fn no_route_port_rule_reaches_the_slirp_network_or_loopback_and_only_dns_reaches_its_resolver()
    {
        let dns = "-A OUTPUT -d 10.0.2.3/32 -o tap0 -p tcp -m tcp --dport 53 -j ACCEPT";
        let fence = "-A OUTPUT -d 10.0.2.0/24 -j DROP";
        let belt = "-A OUTPUT -d 127.0.0.0/8 -j DROP";
        let link = "-A OUTPUT -d 169.254.0.0/16 -j DROP";
        let metadata = "-A OUTPUT -d 100.100.100.200/32 -j DROP";
        let wire = "-A OUTPUT -d 168.63.129.16/32 -j DROP";
        for ports in [&[443][..], &[53, 443], &[1, 8443, 65535]] {
            let rules = egress_rules(ports);
            let lines: Vec<&str> = rules.lines().collect();
            let fenced = lines.iter().position(|line| *line == fence);
            let Some(fenced) = fenced else {
                panic!("no drop of the slirp network: {rules}");
            };
            let before: Vec<&str> = lines[..fenced]
                .iter()
                .copied()
                .filter(|line| line.starts_with("-A "))
                .collect();
            assert_eq!(before, ["-A OUTPUT -o lo -j ACCEPT", dns], "{rules}");
            assert_eq!(lines.get(fenced + 1), Some(&belt), "{rules}");
            assert_eq!(lines.get(fenced + 2), Some(&link), "{rules}");
            assert_eq!(lines.get(fenced + 3), Some(&metadata), "{rules}");
            assert_eq!(lines.get(fenced + 4), Some(&wire), "{rules}");
            for port in ports {
                let line = format!("-A OUTPUT -o tap0 -p tcp -m tcp --dport {port} -j ACCEPT");
                let at = lines.iter().position(|kept| *kept == line);
                assert!(at.is_some_and(|at| at > fenced + 4), "{port}: {rules}");
            }
            assert!(
                !rules.contains("10.0.2.2"),
                "the fence covers the gateway: {rules}"
            );
            assert_eq!(rules.matches("10.0.2.3").count(), 1, "{rules}");
        }
    }

    #[test]
    fn the_read_back_drops_comments_and_counters_and_keeps_every_policy_and_rule() {
        let canonical = concat!(
            "*filter\n",
            ":INPUT ACCEPT\n",
            ":FORWARD DROP\n",
            ":OUTPUT DROP\n",
            "-A OUTPUT -o lo -j ACCEPT\n",
            "-A OUTPUT -d 10.0.2.3/32 -o tap0 -p tcp -m tcp --dport 53 -j ACCEPT\n",
            "-A OUTPUT -d 10.0.2.0/24 -j DROP\n",
            "-A OUTPUT -d 127.0.0.0/8 -j DROP\n",
            "-A OUTPUT -d 169.254.0.0/16 -j DROP\n",
            "-A OUTPUT -d 100.100.100.200/32 -j DROP\n",
            "-A OUTPUT -d 168.63.129.16/32 -j DROP\n",
            "-A OUTPUT -o tap0 -p tcp -m tcp --dport 443 -j ACCEPT\n",
            "COMMIT\n",
        );
        assert_eq!(canonical_rules(ONE_PORT), canonical);
        assert_eq!(canonical_rules(SAVED), canonical);
        let widened = SAVED.replace(":OUTPUT DROP [12:720]", ":OUTPUT ACCEPT [12:720]");
        let added = SAVED.replace("COMMIT\n", "-A OUTPUT -o tap0 -p udp -j ACCEPT\nCOMMIT\n");
        let dropped = SAVED.replace("-A OUTPUT -d 10.0.2.0/24 -j DROP\n", "");
        let reached = SAVED.replace("-A OUTPUT -d 100.100.100.200/32 -j DROP\n", "");
        let wired = SAVED.replace("-A OUTPUT -d 168.63.129.16/32 -j DROP\n", "");
        for changed in [widened, added, dropped, reached, wired] {
            assert_ne!(canonical_rules(&changed), canonical, "{changed}");
        }
        assert_eq!(
            canonical_rules(V6),
            canonical_rules(&format!("# saved\n{V6}"))
        );
    }

    #[test]
    fn the_setup_child_raises_exactly_net_admin_and_net_raw_into_its_ambient_set() {
        assert_eq!(
            ambient_wanted(),
            CapabilitySet::NET_ADMIN | CapabilitySet::NET_RAW
        );
        assert_eq!(ambient_wanted().bits(), 0x3000);
        assert_eq!(ambient_wanted().iter().count(), 2);
    }

    #[test]
    fn the_resolver_file_names_only_the_sandbox_resolver_and_keeps_search_and_options() {
        let resolved = concat!(
            "# This is /run/systemd/resolve/stub-resolv.conf managed by man:systemd-resolved(8).\n",
            "nameserver 127.0.0.53\n",
            "options edns0 trust-ad\n",
            "search lan\n",
        );
        assert_eq!(
            resolver_text(resolved),
            "nameserver 10.0.2.3\noptions edns0 trust-ad\nsearch lan\noptions use-vc\n"
        );
        let several = "nameserver 192.168.1.1\nnameserver 8.8.8.8\n; old\nnameserver ::1\n";
        assert_eq!(
            resolver_text(several),
            "nameserver 10.0.2.3\noptions use-vc\n"
        );
        assert_eq!(resolver_text(""), "nameserver 10.0.2.3\noptions use-vc\n");
        let spaced = "search\t a.example   b.example\nsortlist 10.0.0.0/8\ndomain lan\n";
        assert_eq!(
            resolver_text(spaced),
            "nameserver 10.0.2.3\nsearch a.example b.example\noptions use-vc\n"
        );
        let long = format!("search {}\n", "a".repeat(400));
        assert_eq!(
            resolver_text(&long),
            "nameserver 10.0.2.3\noptions use-vc\n"
        );
        let many = "options ndots:1\n".repeat(64);
        let kept = resolver_text(&many);
        assert_eq!(
            kept.lines()
                .filter(|line| line.starts_with("nameserver"))
                .count(),
            1
        );
        assert!(kept.lines().count() <= 10, "{kept}");
    }

    #[test]
    fn only_an_nf_tables_iptables_is_run() {
        assert!(nf_tables("iptables-restore v1.8.10 (nf_tables)\n"));
        assert!(nf_tables("ip6tables-restore v1.8.9 (nf_tables)"));
        for refused in [
            "iptables-restore v1.8.7 (legacy)\n",
            "",
            "nf_tables",
            "v1.8.10\n",
        ] {
            assert!(!nf_tables(refused), "{refused:?}");
        }
    }

    #[test]
    fn stage_one_writes_its_descriptor_flags_right_after_unshare_net() {
        let args = nest_args(&["--unshare-net", "--chdir", "/p", "--", "/h", "--probe"]);
        let written = insert(&args, [5, 6, 7]);
        let expected = nest_args(&[
            "--unshare-net",
            INFO_FD,
            "5",
            BLOCK_FD,
            "6",
            RO_BIND_DATA,
            "7",
            RESOLV_CONF,
            "--chdir",
            "/p",
            "--",
            "/h",
            "--probe",
        ]);
        assert_eq!(written, expected);
        assert_eq!(placed(&written), Ok(()));
    }

    #[test]
    fn bubblewrap_starts_only_with_the_descriptor_flags_in_order_and_the_resolver_file() {
        let tail = ["--chdir", "/p", "--", "/h", "--probe"];
        let with = |flags: &[&str]| {
            let mut after = vec!["--unshare-net"];
            after.extend_from_slice(flags);
            after.extend_from_slice(&tail);
            nest_args(&after)
        };
        let right = [INFO_FD, "5", BLOCK_FD, "6", RO_BIND_DATA, "7", RESOLV_CONF];
        assert_eq!(placed(&with(&right)), Ok(()));
        let swapped = [BLOCK_FD, "6", INFO_FD, "5", RO_BIND_DATA, "7", RESOLV_CONF];
        let elsewhere = [INFO_FD, "5", BLOCK_FD, "6", RO_BIND_DATA, "7", "/etc/hosts"];
        let linked = [
            INFO_FD,
            "5",
            BLOCK_FD,
            "6",
            RO_BIND_DATA,
            "7",
            "/run/resolv.conf",
        ];
        let standard = [INFO_FD, "2", BLOCK_FD, "6", RO_BIND_DATA, "7", RESOLV_CONF];
        let shared = [INFO_FD, "5", BLOCK_FD, "5", RO_BIND_DATA, "7", RESOLV_CONF];
        let short = [INFO_FD, "5", BLOCK_FD, "6"];
        for refused in [
            &swapped[..],
            &elsewhere,
            &linked,
            &standard,
            &shared,
            &short,
        ] {
            assert!(placed(&with(refused)).is_err(), "{refused:?}");
        }
        let mut twice = with(&right);
        let at = twice.len() - tail.len();
        twice.splice(at..at, words(&[BLOCK_FD, "8"]));
        assert!(placed(&twice).is_err());
        let mut unshared = nest_args(&right);
        unshared.extend(words(&tail));
        assert!(placed(&unshared).is_err(), "no --unshare-net");
    }

    #[test]
    fn route_flags_lead_the_arguments_with_ports_each_once() {
        let rest = words(&["--bridge", "18080", "--nest", "--"]);
        let mut args = words(&[ROUTE, "443", ROUTE, "8443"]);
        args.extend(rest.clone());
        assert_eq!(leading(args), Ok((vec![443, 8443], rest.clone())));
        assert_eq!(leading(rest.clone()), Ok((Vec::new(), rest.clone())));
        let mut most = Vec::new();
        for port in 1..=REMOTES_MAX {
            most.extend(words(&[ROUTE, port.to_string().as_str()]));
        }
        assert_eq!(
            leading(most.clone()).map(|(ports, _)| ports.len()),
            Ok(REMOTES_MAX)
        );
        most.extend(words(&[ROUTE, "9999"]));
        assert!(leading(most).is_err(), "more than REMOTES_MAX");
        for refused in [
            &[ROUTE][..],
            &[ROUTE, "0"],
            &[ROUTE, "65536"],
            &[ROUTE, "port"],
            &[ROUTE, "443", ROUTE, "443"],
        ] {
            assert!(leading(words(refused)).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn the_setup_child_takes_its_parent_and_the_route_ports_alone() {
        assert_eq!(
            request(words(&[EGRESS, "1234", "443", "8443"])),
            Ok(HelperRequest::Egress {
                parent: 1234,
                ports: vec![443, 8443],
            })
        );
        let nine: Vec<String> = (1..=9).map(|port| port.to_string()).collect();
        let mut too_many = vec![EGRESS, "1234"];
        too_many.extend(nine.iter().map(String::as_str));
        for refused in [
            &[EGRESS][..],
            &[EGRESS, "1234"],
            &[EGRESS, "1", "443"],
            &[EGRESS, "pid", "443"],
            &[EGRESS, "1234", "0"],
            &[EGRESS, "1234", "65536"],
            &[EGRESS, "1234", "443x"],
            &[EGRESS, "+1234", "443"],
            &[EGRESS, "1234", "443", "443"],
            &[EGRESS, "1234", "--route", "443"],
            too_many.as_slice(),
        ] {
            assert!(request(words(refused)).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn stage_two_starts_only_on_the_go_byte_and_then_holds_no_descriptor() {
        use std::os::fd::IntoRawFd;
        let handed = |bytes: &[u8]| {
            let (read, mut write) = std::io::pipe().unwrap();
            write.write_all(bytes).unwrap();
            (read.into_raw_fd(), write)
        };
        let closed = |write: &mut PipeWriter| write.write(&[GO]).map_err(|error| error.kind());
        let broken = Err(std::io::ErrorKind::BrokenPipe);
        assert_ne!(BLOCK, GO);
        let (fd, mut write) = handed(&[GO]);
        assert_eq!(started(fd), Ok(()));
        assert_eq!(closed(&mut write), broken, "the go descriptor is closed");
        let (fd, mut write) = handed(&[BLOCK]);
        assert_eq!(
            started(fd),
            Err(ROUTE_NOT_BUILT.to_owned()),
            "a byte but the go byte"
        );
        assert_eq!(closed(&mut write), broken);
        let (fd, write) = handed(&[]);
        drop(write);
        assert_eq!(
            started(fd),
            Err(ROUTE_NOT_BUILT.to_owned()),
            "a setup child gone"
        );
    }

    #[test]
    fn a_routed_nest_names_its_go_descriptor_to_this_helper_alone_after_the_bridge_flags() {
        use super::super::bridge::{self, BRIDGE, BRIDGE_FD};
        use super::super::{NEST, PROBE, SCOPED, read_request};
        let helper = std::env::current_exe().unwrap();
        let mut args = nest_args(&[UNSHARE_NET, "--chdir", "/work", "--"]);
        args.push(helper.clone().into());
        args.push(PROBE.into());
        let described = insert(&args, [5, 6, 7]);
        let routed = inner(&described, 8, &helper);
        assert!(routed.is_ok(), "{routed:?}");
        let Ok(routed) = routed else {
            return;
        };
        let written = bridge::inner(&routed, 9, &[18080], &helper).unwrap();
        assert_eq!(placed(&written), Ok(()));
        let named = written
            .iter()
            .position(|word| word.as_os_str() == helper.as_os_str());
        let at = named.unwrap() + 1;
        let inside = words(&[BRIDGE_FD, "9", BRIDGE, "18080", ROUTE_FD, "8", PROBE]);
        assert_eq!(written[at..], inside);
        let asked = |args: Vec<OsString>| {
            read_request(args).map(|(routes, bridge, started, request)| {
                let probe = matches!(request, HelperRequest::Probe { .. });
                (
                    routes,
                    bridge.map(|held| (held.fd, held.ports)),
                    started,
                    probe,
                )
            })
        };
        let bridged = Some((Some(9), vec![18080]));
        assert_eq!(asked(inside), Ok((Vec::new(), bridged, Some(8), true)));
        assert_eq!(
            asked(words(&[ROUTE_FD, "8", PROBE])),
            Ok((Vec::new(), None, Some(8), true))
        );
        let nested = asked(words(&[ROUTE_FD, "8", "--nested", "--", "/bin/true"]));
        assert_eq!(nested.map(|asked| (asked.2, asked.3)), Ok((Some(8), false)));
        let mut other = described.clone();
        let command = other.len() - 2;
        other[command] = "/bin/sh".into();
        assert!(inner(&other, 8, &helper).is_err(), "not this helper");
        let mut outer = words(&[ROUTE, "443", ROUTE_FD, "8", NEST, "--", BWRAP_PATHS[0]]);
        outer.extend(args.iter().cloned());
        for refused in [
            &[ROUTE_FD, "2", PROBE][..],
            &[ROUTE_FD, "x", PROBE],
            &[ROUTE_FD, "8", ROUTE_FD, "9", PROBE],
            &[ROUTE_FD, "8", BRIDGE, "18080", PROBE],
            &[ROUTE_FD, "8", "--", "/bin/true"],
            &[ROUTE_FD, "8", SCOPED],
            &[ROUTE, "443", ROUTE_FD, "8", PROBE],
            &[ROUTE, "443", ROUTE_FD, "8", "--nested", "--", "/bin/true"],
        ] {
            assert!(asked(words(refused)).is_err(), "{refused:?}");
        }
        assert!(asked(outer).is_err(), "stage 1 takes no go descriptor");
    }

    /// V243 linux routed: slirp4netns runs confined to the files it needs, so a read and a
    /// write in a folder outside them are refused, while the loader cache it needs still
    /// reads. The rules go on a thread of the row's own, so no other row runs under them.
    #[test]
    fn slirp_reads_and_writes_only_the_files_it_needs() {
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret");
        fs::write(&secret, b"secret").unwrap();
        assert_eq!(
            fs::read(&secret).unwrap(),
            b"secret",
            "the control: it reads unconfined"
        );
        let child = std::process::id();
        let (opened, read, written, cache) = std::thread::spawn(move || {
            let opened = confine_slirp(child);
            let read = fs::read(&secret);
            let written = fs::write(secret.with_file_name("written"), b"written");
            let cache = fs::read("/etc/ld.so.cache");
            (opened, read, written, cache)
        })
        .join()
        .unwrap();
        assert!(opened.is_ok(), "{opened:?}");
        let refused = |error: &std::io::Error| error.kind() == std::io::ErrorKind::PermissionDenied;
        assert!(
            read.as_ref().is_err_and(refused),
            "slirp read outside its files: {read:?}"
        );
        assert!(
            written.as_ref().is_err_and(refused),
            "slirp wrote outside its files: {written:?}"
        );
        assert!(
            cache.is_ok(),
            "slirp cannot read the loader cache: {cache:?}"
        );
    }

    /// V243 linux routed r2, rv-sec3 F1: slirp4netns runs in this computer's mount namespace,
    /// where Landlock does not mediate a pathname socket connect, so a confined slirp must
    /// not reach a socket such as the engine's tmux server's. A socket pair, which carries
    /// its tap descriptor, and a UDP socket, which carries the agent's traffic, still open.
    #[test]
    fn slirp_connects_to_no_unix_socket() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("tmux.sock");
        assert_eq!(
            crate::socket_address::address(&path).unwrap(),
            path,
            "the socket is reached at its own path, so no file opens on the way"
        );
        let _server = crate::spawn_gate::bind_unix_std(&path).unwrap();
        assert!(
            crate::spawn_gate::connect_unix_std(&path).is_ok(),
            "the control: it connects unconfined"
        );
        let child = std::process::id();
        let (opened, connected, pair, udp) = std::thread::spawn(move || {
            let opened = confine_slirp(child);
            let connected = crate::spawn_gate::connect_unix_std(&path);
            let pair = crate::spawn_gate::unix_pair_std();
            let udp = crate::spawn_gate::bind_udp_std((std::net::Ipv4Addr::LOCALHOST, 0));
            (opened, connected, pair, udp)
        })
        .join()
        .unwrap();
        assert!(opened.is_ok(), "{opened:?}");
        assert!(
            connected
                .as_ref()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied),
            "slirp connected to a pathname socket: {connected:?}"
        );
        assert!(
            pair.is_ok(),
            "slirp cannot make its tap socket pair: {pair:?}"
        );
        assert!(udp.is_ok(), "slirp cannot open a UDP socket: {udp:?}");
    }
}
