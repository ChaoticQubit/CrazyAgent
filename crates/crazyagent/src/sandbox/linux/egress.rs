use super::super::REMOTES_MAX;
use super::bridge::UNSHARE_NET;
use super::{BWRAP_PATHS, HelperRequest, NEST_ARGS, die_with_parent, trusted_file};
use rustix::fs::FileType;
use rustix::thread::{CapabilitySet, CapabilitySets, LinkNameSpaceType};
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::io::{PipeReader, PipeWriter, Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};
use std::str::FromStr;
use std::time::Duration;

pub(super) const ROUTE: &str = "--route";
pub(super) const EGRESS: &str = "--egress";
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
const SANDBOX_RESOLVER: &str = "10.0.2.3";
const SLIRP_GATEWAY: &str = "10.0.2.2";
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
            return Err(format!("{ROUTE} takes 1 to {REMOTES_MAX} ports 1 to 65535, each once"));
        };
        ports.push(port);
    }
    assert!(ports.len() <= REMOTES_MAX);
    Ok((ports, rest.collect()))
}

/// Tells whether a request may come with these route ports: only a nest whose bubblewrap
/// unshares the network right after its fixed arguments takes them, so no agent is routed
/// on this computer's network. Inputs: the ports, empty for none, and the request. Output:
/// Ok, or the reason.
pub(super) fn fits(routes: &[u16], request: &HelperRequest) -> Result<(), String> {
    assert!(routes.len() <= REMOTES_MAX);
    if routes.is_empty() {
        return Ok(());
    }
    match request {
        HelperRequest::Nest { args, .. }
            if args.get(NEST_ARGS.len()).is_some_and(|arg| arg == UNSHARE_NET) =>
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
    let refused = || {
        format!("{EGRESS} takes a process id above 1, then 1 to {REMOTES_MAX} ports, each once")
    };
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
/// writes its info to, the read end its child blocks on, and the read end of the resolver
/// file. Each closes on exec until [`Ends::hand_over`].
pub(super) struct Ends {
    info: PipeWriter,
    block: PipeReader,
    resolver: PipeReader,
}

impl Ends {
    /// Lets bubblewrap inherit the three ends, right before stage 1 becomes it and after
    /// every child stage 1 starts, so no other process holds them. Output: Ok, or the
    /// reason.
    pub(super) fn hand_over(&self) -> Result<(), String> {
        for end in [self.info.as_fd(), self.block.as_fd(), self.resolver.as_fd()] {
            rustix::io::fcntl_setfd(end, rustix::io::FdFlags::empty())
                .map_err(|error| format!("the agent's network: {error}"))?;
        }
        Ok(())
    }
}

/// Opens the routed network from stage 1: the info, block and resolver pipes, the resolver
/// file written whole into its pipe, and the setup child started on the other two ends
/// with this process as the one it dies with. Inputs: the route ports and the nest's
/// arguments. Output: the ends and the arguments with the descriptor flags ([`insert`]);
/// or the reason.
pub(super) fn open(ports: &[u16], args: &[OsString]) -> Result<(Ends, Vec<OsString>), String> {
    assert!(!ports.is_empty() && ports.len() <= REMOTES_MAX);
    let failed = |error: std::io::Error| format!("the agent's network: {error}");
    let (info_read, info) = std::io::pipe().map_err(failed)?;
    let (block, block_write) = std::io::pipe().map_err(failed)?;
    let (resolver, mut resolver_write) = std::io::pipe().map_err(failed)?;
    let text = resolver_text(&host_resolver());
    assert!(text.len() <= RESOLVER_TEXT_BYTES_MAX);
    resolver_write.write_all(text.as_bytes()).map_err(failed)?;
    drop(resolver_write);
    let fds = [info.as_raw_fd(), block.as_raw_fd(), resolver.as_raw_fd()];
    if fds.iter().any(|fd| *fd <= 2) {
        return Err("the agent's network: a standard descriptor was closed".into());
    }
    let helper = std::env::current_exe().map_err(failed)?;
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
    let args = insert(args, fds);
    Ok((
        Ends {
            info,
            block,
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
/// slirp4netns, applies the rules and reads them back, and only then lets bubblewrap start
/// the agent; a refused or failed setup writes no block byte and kills the waiting agent
/// ([`stop`]). Inputs: the stage-1 process id and the route ports. Output: the exit code,
/// 3 when the network could not be built, which leaves the agent unstarted.
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
            .write_all(&[BLOCK])
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
/// `parent`, and `parent` runs one of the fixed bubblewraps. A refusal kills nothing.
/// Input: the stage-1 process id. Output: bubblewrap's child, or the reason.
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
            assert!(reaped.is_ok(), "slirp4netns is this process's child to reap");
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
    let slirp = tools()?;
    let slirp = start_slirp(slirp, child)?;
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

/// Finds the tools at their fixed places, each a file [`trusted_file`] accepts, with both
/// restore programs on the nf_tables backend ([`nf_tables`]). No input. Output: the
/// slirp4netns path, or the reason.
fn tools() -> Result<&'static str, String> {
    let slirp = SLIRP_PATHS
        .iter()
        .copied()
        .find(|path| trusted_file(Path::new(path)).is_some())
        .ok_or("slirp4netns is not at its fixed place or others may change it")?;
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
        let version = crate::spawn_gate::output(&mut asked)
            .map_err(|error| format!("{restore}: {error}"))?;
        if !version.status.success() || !nf_tables(&String::from_utf8_lossy(&version.stdout)) {
            return Err(format!("{restore} is not the nf_tables iptables"));
        }
    }
    Ok(slirp)
}

/// Starts slirp4netns for the agent's network, from this computer's network, and waits up
/// to [`READY_TIME`] for its ready byte. Its input is the read end of its exit pipe; its
/// ready end is the only other descriptor it inherits. Inputs: its path and the agent's
/// process id. Output: slirp4netns, or the reason.
fn start_slirp(slirp: &str, child: u32) -> Result<Slirp, String> {
    let failed = |error: std::io::Error| format!("slirp4netns: {error}");
    let (exit_read, exit_write) = std::io::pipe().map_err(failed)?;
    let (ready, handed) = crate::spawn_gate::unix_pair_std().map_err(failed)?;
    ready.set_read_timeout(Some(READY_TIME)).map_err(failed)?;
    let handed = OwnedFd::from(handed);
    let fd = handed.as_raw_fd();
    assert!(fd > 2);
    let mut command = Command::new(slirp);
    command
        .args([
            "--configure",
            "--mtu=65520",
            "--disable-host-loopback",
            "--enable-sandbox",
            "--enable-seccomp",
            "--netns-type=path",
        ])
        .arg(format!("--userns-path=/proc/{child}/ns/user"))
        .arg("--exit-fd=0")
        .arg(format!("--ready-fd={fd}"))
        .arg(format!("/proc/{child}/ns/net"))
        .arg(TAP)
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
    let hex = status.lines().find_map(|line| line.strip_prefix("CapAmb:"))?;
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
/// form `iptables-save` prints them: out only through `tap0` to each port, TCP 53 only to
/// slirp's resolver, never to slirp's gateway, and loopback for the bridged ports. Input:
/// the ports on any host. Output: the rules.
fn egress_rules(any: &[u16]) -> String {
    assert!(!any.is_empty() && any.len() <= REMOTES_MAX);
    assert!(!any.contains(&0));
    let mut rules = String::from(CHAINS);
    let _ = writeln!(rules, "-A OUTPUT -d {SLIRP_GATEWAY}/32 -j DROP");
    let _ = writeln!(
        rules,
        "-A OUTPUT -d {SANDBOX_RESOLVER}/32 -o {TAP} -p tcp -m tcp --dport 53 -j ACCEPT"
    );
    for port in any {
        let _ = writeln!(rules, "-A OUTPUT -o {TAP} -p tcp -m tcp --dport {port} -j ACCEPT");
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
            let key = words.next().filter(|key| *key == "search" || *key == "options")?;
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
    for (pair, flag) in written[1..written.len() - 1].chunks_exact(2).zip(DESCRIPTOR_FLAGS) {
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
        for rules in [egress_rules(&[443]), egress_rules(&[443, 8443]), egress_rules_v6()] {
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
    fn no_route_port_rule_reaches_the_slirp_network_and_only_dns_reaches_its_resolver() {
        let dns = "-A OUTPUT -d 10.0.2.3/32 -o tap0 -p tcp -m tcp --dport 53 -j ACCEPT";
        let fence = "-A OUTPUT -d 10.0.2.0/24 -j DROP";
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
            for port in ports {
                let line = format!("-A OUTPUT -o tap0 -p tcp -m tcp --dport {port} -j ACCEPT");
                let at = lines.iter().position(|kept| *kept == line);
                assert!(at.is_some_and(|at| at > fenced), "{port}: {rules}");
            }
            assert!(!rules.contains("10.0.2.2"), "the fence covers the gateway: {rules}");
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
            "-A OUTPUT -o tap0 -p tcp -m tcp --dport 443 -j ACCEPT\n",
            "COMMIT\n",
        );
        assert_eq!(canonical_rules(ONE_PORT), canonical);
        assert_eq!(canonical_rules(SAVED), canonical);
        let widened = SAVED.replace(":OUTPUT DROP [12:720]", ":OUTPUT ACCEPT [12:720]");
        let added = SAVED.replace("COMMIT\n", "-A OUTPUT -o tap0 -p udp -j ACCEPT\nCOMMIT\n");
        let dropped = SAVED.replace("-A OUTPUT -d 10.0.2.0/24 -j DROP\n", "");
        for changed in [widened, added, dropped] {
            assert_ne!(canonical_rules(&changed), canonical, "{changed}");
        }
        assert_eq!(canonical_rules(V6), canonical_rules(&format!("# saved\n{V6}")));
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
        assert_eq!(resolver_text(&long), "nameserver 10.0.2.3\noptions use-vc\n");
        let many = "options ndots:1\n".repeat(64);
        let kept = resolver_text(&many);
        assert_eq!(kept.lines().filter(|line| line.starts_with("nameserver")).count(), 1);
        assert!(kept.lines().count() <= 10, "{kept}");
    }

    #[test]
    fn only_an_nf_tables_iptables_is_run() {
        assert!(nf_tables("iptables-restore v1.8.10 (nf_tables)\n"));
        assert!(nf_tables("ip6tables-restore v1.8.9 (nf_tables)"));
        for refused in ["iptables-restore v1.8.7 (legacy)\n", "", "nf_tables", "v1.8.10\n"] {
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
        let linked = [INFO_FD, "5", BLOCK_FD, "6", RO_BIND_DATA, "7", "/run/resolv.conf"];
        let standard = [INFO_FD, "2", BLOCK_FD, "6", RO_BIND_DATA, "7", RESOLV_CONF];
        let shared = [INFO_FD, "5", BLOCK_FD, "5", RO_BIND_DATA, "7", RESOLV_CONF];
        let short = [INFO_FD, "5", BLOCK_FD, "6"];
        for refused in [&swapped[..], &elsewhere, &linked, &standard, &shared, &short] {
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
}
