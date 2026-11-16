use super::super::{BRIDGE_PORT_MIN, REMOTES_MAX};
use super::{HelperRequest, LandlockPolicy, NEST_ARGS, accepted, die_with_parent, enforce};
use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::sys::signal::SigSet;
use nix::sys::socket::{
    AddressFamily, ControlMessage, ControlMessageOwned, MsgFlags, Shutdown, SockFlag, SockType,
    accept4, recv, recvmsg, send, sendmsg, setsockopt, shutdown, socketpair, sockopt,
};
use nix::sys::time::{TimeVal, TimeValLike};
use std::ffi::{OsStr, OsString};
use std::io::{IoSlice, IoSliceMut, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

pub(super) const BRIDGE: &str = "--bridge";
pub(super) const BRIDGE_FD: &str = "--bridge-fd";
pub(super) const BROKER: &str = "--broker";
pub(super) const UNSHARE_NET: &str = "--unshare-net";
/// The most ports one bridge carries: every remote a limited network may name.
const PORTS_MAX: usize = REMOTES_MAX;
/// One IPv4 and one IPv6 listener for each bridged port.
const LISTENERS_MAX: usize = 2 * PORTS_MAX;
/// The most connections one broker carries at a time; a further accept waits for one to end.
const LIVE_MAX: usize = 256;
const BUFFER_BYTES: usize = 64 * 1024;
/// How long the helper inside the nest waits for the broker's answer, in seconds.
const ANSWER_TIME_S: i64 = 10;
/// How long a listener waits after an accept that failed for a reason that passes.
const ACCEPT_PAUSE: Duration = Duration::from_millis(100);
const ENTRY_BYTES: usize = 3;
const V4: u8 = 4;
const V6: u8 = 6;
const READY: u8 = 1;

/// One listener the broker carries: its family ([`V4`] or [`V6`]), its port and its
/// descriptor in the broker.
type Listener = (u8, u16, RawFd);

/// The loopback bridge of a nested agent that has a network of its own, as the flags that
/// lead the helper's arguments give it: the ports carried to this computer's loopback and,
/// inside the nest only, the descriptor the helper hands its listeners over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bridge {
    pub fd: Option<RawFd>,
    pub ports: Vec<u16>,
}

impl Bridge {
    /// Splits the bridge flags that lead the helper's arguments from the rest: `--bridge`
    /// with a port, one to [`PORTS_MAX`] times, and `--bridge-fd` with a descriptor, at
    /// most once. Input: the helper arguments. Output: the bridge, none when no such flag
    /// leads, and the arguments after it; or the reason the flags are refused.
    pub fn leading(args: Vec<OsString>) -> Result<(Option<Self>, Vec<OsString>), String> {
        let mut bridge = Self {
            fd: None,
            ports: Vec::new(),
        };
        let mut rest = args.into_iter().peekable();
        while let Some(flag) = rest.next_if(|flag| flag == BRIDGE || flag == BRIDGE_FD) {
            if flag == BRIDGE {
                bridge.ports.push(bridged(rest.next())?);
            } else if bridge.fd.is_none() {
                bridge.fd = Some(descriptor(rest.next())?);
            } else {
                return Err(format!("{BRIDGE_FD} is given once"));
            }
        }
        let rest: Vec<OsString> = rest.collect();
        if bridge.fd.is_none() && bridge.ports.is_empty() {
            return Ok((None, rest));
        }
        distinct(&bridge.ports)?;
        assert!(bridge.ports.iter().all(|port| *port >= BRIDGE_PORT_MIN));
        Ok((Some(bridge), rest))
    }

    /// Tells whether a request may come with these leading flags. A nest takes the ports,
    /// without a descriptor, exactly when its bubblewrap unshares the network; a probe and
    /// a nested run take the ports with the descriptor the nest wrote; nothing else takes a
    /// bridge. Inputs: the bridge, if any, and the request. Output: Ok, or the reason.
    pub fn fits(bridge: Option<&Self>, request: &HelperRequest) -> Result<(), String> {
        let apart = match request {
            HelperRequest::Nest { args, .. } => args
                .iter()
                .take_while(|arg| *arg != "--")
                .any(|arg| arg == UNSHARE_NET),
            _ => false,
        };
        let Some(bridge) = bridge else {
            if apart {
                return Err(format!("{UNSHARE_NET} needs {BRIDGE}"));
            }
            return Ok(());
        };
        let handed = bridge.fd.is_some();
        let taken = match request {
            HelperRequest::Nest { .. } => apart && !handed,
            HelperRequest::Probe { .. } => handed,
            HelperRequest::Run { policy, .. } => handed && policy.nested,
            _ => false,
        };
        if !taken {
            return Err(format!(
                "{BRIDGE} leads a nest that unshares the network or, with {BRIDGE_FD}, a probe \
                 or a nested run inside it"
            ));
        }
        Ok(())
    }

    /// Hands this bridge's listeners to the broker, from inside the nest's network
    /// namespace and before any rule or filter is applied: `127.0.0.1` on each port and
    /// `::1` where it binds, sent in one message with what each is, then closed here with
    /// the descriptor once the broker has answered. Output: Ok, or the reason the bridge
    /// did not start.
    pub(super) fn hand_over(&self) -> Result<(), String> {
        let fd = self.fd.ok_or("the bridge has no descriptor")?;
        let mut listeners = Vec::new();
        for port in &self.ports {
            let v4 = crate::spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, *port))
                .map_err(|error| format!("bridge port {port}: {error}"))?;
            listeners.push((V4, *port, v4));
            if let Ok(v6) = crate::spawn_gate::bind_tcp_std((Ipv6Addr::LOCALHOST, *port)) {
                listeners.push((V6, *port, v6));
            }
        }
        assert!(listeners.len() <= LISTENERS_MAX);
        let named: Vec<(u8, u16)> = listeners
            .iter()
            .map(|(family, port, _)| (*family, *port))
            .collect();
        let fds: Vec<RawFd> = listeners
            .iter()
            .map(|(_, _, listener)| listener.as_raw_fd())
            .collect();
        let text = entries(&named);
        let sent = sendmsg::<()>(
            fd,
            &[IoSlice::new(&text)],
            &[ControlMessage::ScmRights(&fds)],
            MsgFlags::MSG_NOSIGNAL,
            None,
        );
        let mut answer = [0u8; 1];
        let answered = recv(fd, &mut answer, MsgFlags::empty());
        drop(listeners);
        nix::unistd::close(fd).map_err(|error| format!("bridge descriptor: {error}"))?;
        if sent != Ok(text.len()) || answered != Ok(1) || answer[0] != READY {
            return Err("the loopback bridge did not start".into());
        }
        Ok(())
    }
}

/// Reads one bridged port. Input: the argument after `--bridge`, or a port of the broker.
/// Output: the port, or the reason it is refused: a helper inside its own network namespace
/// listens on no port below [`BRIDGE_PORT_MIN`].
fn bridged(value: Option<OsString>) -> Result<u16, String> {
    super::port(value)
        .ok()
        .filter(|port| *port >= BRIDGE_PORT_MIN)
        .ok_or_else(|| format!("a bridged port is {BRIDGE_PORT_MIN} to 65535"))
}

/// Reads the descriptor `--bridge-fd` names. Input: the argument. Output: a descriptor
/// above the three standard ones, or the reason it is refused.
fn descriptor(value: Option<OsString>) -> Result<RawFd, String> {
    value
        .as_deref()
        .and_then(OsStr::to_str)
        .and_then(|text| text.parse::<RawFd>().ok())
        .filter(|fd| *fd > 2)
        .ok_or_else(|| format!("{BRIDGE_FD} needs a descriptor above 2"))
}

/// Checks the ports of one bridge. Input: the ports. Output: Ok for one to [`PORTS_MAX`]
/// ports with none twice, or the reason they are refused.
fn distinct(ports: &[u16]) -> Result<(), String> {
    let twice = ports
        .iter()
        .enumerate()
        .any(|(at, port)| ports[..at].contains(port));
    if ports.is_empty() || ports.len() > PORTS_MAX || twice {
        return Err(format!("{BRIDGE} takes 1 to {PORTS_MAX} ports, each once"));
    }
    Ok(())
}

/// Reads the broker's arguments, `--broker` first: the process it dies with, then the ports
/// it carries. Input: the helper arguments. Output: the request, or the reason they are
/// refused.
pub(super) fn broker_request(args: Vec<OsString>) -> Result<HelperRequest, String> {
    let mut rest = args.into_iter().skip(1);
    let parent =
        super::parent(rest.next()).map_err(|_| format!("{BROKER} needs a process id above 1"))?;
    let ports = rest
        .map(|port| bridged(Some(port)))
        .collect::<Result<Vec<u16>, String>>()?;
    distinct(&ports)?;
    Ok(HelperRequest::Broker { parent, ports })
}

/// Opens the bridge from outside the nest: a socket pair, close-on-exec from the call that
/// makes it, so the spawn gate has nothing to hold; the broker on this computer's network
/// with one end as its input; and the other end left open for the helper inside, named in
/// its flags ([`inner`]). Inputs: the ports and the nest's arguments. Output: the open end,
/// kept until bubblewrap starts, and the arguments with the flags; or the reason.
#[allow(clippy::disallowed_methods)]
pub(super) fn open(ports: &[u16], args: &[OsString]) -> Result<(OwnedFd, Vec<OsString>), String> {
    assert!(!ports.is_empty() && ports.len() <= PORTS_MAX);
    let (inside, outside) = socketpair(
        AddressFamily::Unix,
        SockType::SeqPacket,
        None,
        SockFlag::SOCK_CLOEXEC,
    )
    .map_err(|error| format!("bridge socket: {error}"))?;
    let wait = TimeVal::seconds(ANSWER_TIME_S);
    setsockopt(&inside, sockopt::ReceiveTimeout, &wait)
        .map_err(|error| format!("bridge socket: {error}"))?;
    let helper = std::env::current_exe().map_err(|error| format!("this helper: {error}"))?;
    let args = inner(args, inside.as_raw_fd(), ports, &helper)?;
    let mut broker = Command::new(&helper);
    broker
        .arg(BROKER)
        .arg(std::process::id().to_string())
        .args(ports.iter().map(u16::to_string))
        .env_clear()
        .stdin(Stdio::from(outside))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::spawn_gate::spawn(&mut broker).map_err(|error| format!("bridge broker: {error}"))?;
    fcntl(inside.as_raw_fd(), FcntlArg::F_SETFD(FdFlag::empty()))
        .map_err(|error| format!("bridge socket: {error}"))?;
    assert!(inside.as_raw_fd() > 2);
    Ok((inside, args))
}

/// Writes the bridge flags for the helper bubblewrap starts, right after its name: the
/// descriptor and each port, which [`Bridge::leading`] reads back inside the nest. A nest
/// whose command is not this helper is refused, so no other program gets the descriptor.
/// Inputs: the nest's arguments, the descriptor, the ports and this helper. Output: the
/// arguments with the flags, or the reason.
pub(super) fn inner(
    args: &[OsString],
    fd: RawFd,
    ports: &[u16],
    helper: &Path,
) -> Result<Vec<OsString>, String> {
    let Some(at) = own_command(args, helper) else {
        return Err(format!("{BRIDGE} needs this helper as the nest's command"));
    };
    let mut written = args[..=at].to_vec();
    written.push(BRIDGE_FD.into());
    written.push(fd.to_string().into());
    for port in ports {
        written.push(BRIDGE.into());
        written.push(port.to_string().into());
    }
    written.extend_from_slice(&args[at + 1..]);
    assert_eq!(written.len(), args.len() + 2 + 2 * ports.len());
    assert_eq!(written[at + 1], BRIDGE_FD);
    Ok(written)
}

/// Finds where the nest names its command: the word after the nest's `--`, past
/// [`NEST_ARGS`], when that word is this helper. Inputs: the nest's arguments and this
/// helper. Output: the word's index, or none for another program or no command.
pub(super) fn own_command(args: &[OsString], helper: &Path) -> Option<usize> {
    let at = args
        .iter()
        .skip(NEST_ARGS.len())
        .position(|arg| arg == "--")?
        + NEST_ARGS.len()
        + 1;
    let own = args
        .get(at)
        .and_then(|word| std::fs::canonicalize(word).ok())
        .is_some_and(|path| path == helper);
    own.then_some(at)
}

/// Writes the handover's text: for each listener its family (4 or 6) and its port, high
/// byte first. Input: family and port of each listener, in the order the descriptors are
/// sent. Output: the bytes.
fn entries(listeners: &[(u8, u16)]) -> Vec<u8> {
    let text: Vec<u8> = listeners
        .iter()
        .flat_map(|(family, port)| {
            let [high, low] = port.to_be_bytes();
            [*family, high, low]
        })
        .collect();
    assert_eq!(text.len(), ENTRY_BYTES * listeners.len());
    text
}

/// Reads a handover back in the broker: one entry for each descriptor received, each an
/// IPv4 or IPv6 listener on one of the broker's own ports, none twice, and an IPv4 listener
/// for every port. Inputs: the text, the descriptors and the broker's ports. Output: each
/// listener, or the reason the handover is refused.
fn listed(text: &[u8], fds: &[RawFd], ports: &[u16]) -> Result<Vec<Listener>, String> {
    if fds.is_empty() || fds.len() > LISTENERS_MAX || text.len() != ENTRY_BYTES * fds.len() {
        return Err("the handover does not name each listener once".into());
    }
    let mut listeners: Vec<Listener> = Vec::with_capacity(fds.len());
    for (entry, fd) in text.as_chunks::<ENTRY_BYTES>().0.iter().zip(fds) {
        let (family, port) = (entry[0], u16::from_be_bytes([entry[1], entry[2]]));
        let known = [V4, V6].contains(&family) && ports.contains(&port);
        let twice = listeners
            .iter()
            .any(|(had, at, _)| (*had, *at) == (family, port));
        if !known || twice {
            return Err(format!(
                "the handover names a listener of no bridged port ({port})"
            ));
        }
        listeners.push((family, port, *fd));
    }
    let every = ports.iter().all(|port| {
        listeners
            .iter()
            .any(|(family, at, _)| *family == V4 && at == port)
    });
    if !every {
        return Err("the handover leaves a bridged port without its listener".into());
    }
    assert_eq!(listeners.len(), fds.len());
    Ok(listeners)
}

/// Runs the broker of one bridge on this computer's network until the process that started
/// it ends: it takes the listeners the helper bound inside the nest and carries each
/// connection to this computer's loopback on the same port. Inputs: the process it dies
/// with and its ports. Output: the exit code, 3 when the bridge cannot start or has ended.
pub(super) fn broker(parent: u32, ports: &[u16]) -> ExitCode {
    let reason = match ready(parent, ports) {
        Ok(listeners) => carry(&listeners),
        Err(reason) => reason,
    };
    eprintln!("crazyagent-landlock: {reason}");
    ExitCode::from(3)
}

/// Readies the broker: every signal blocked, so only a kill or its parent's end stops it
/// while the agent still gets its stop grace; the listeners taken from its input; a
/// Landlock ruleset with no file right that connects to the bridged ports alone; then the
/// answer the helper inside the nest waits for. Inputs: the process it dies with and its
/// ports. Output: the listeners, or the reason.
fn ready(parent: u32, ports: &[u16]) -> Result<Vec<Listener>, String> {
    SigSet::all()
        .thread_block()
        .map_err(|error| format!("signal mask: {error}"))?;
    die_with_parent(parent)?;
    let channel = std::io::stdin().as_raw_fd();
    let listeners = receive(channel, ports)?;
    let policy = LandlockPolicy {
        limit_network: true,
        connect: ports.to_vec(),
        ..LandlockPolicy::default()
    };
    let held = enforce(&policy, false).map_err(|error| error.to_string())?;
    if !accepted(&held.ruleset, held.no_new_privs) {
        return Err("this kernel cannot hold the broker's boundary".into());
    }
    if send(channel, &[READY], MsgFlags::MSG_NOSIGNAL) != Ok(1) {
        return Err("the helper inside the nest is gone".into());
    }
    assert!(!listeners.is_empty());
    Ok(listeners)
}

/// Receives the one handover message from the broker's input. A message cut short, in its
/// text or in its descriptors, is refused. Inputs: the input descriptor and the broker's
/// ports. Output: each listener ([`listed`]), or the reason.
fn receive(channel: RawFd, ports: &[u16]) -> Result<Vec<Listener>, String> {
    let mut text = [0u8; ENTRY_BYTES * LISTENERS_MAX];
    let mut space = nix::cmsg_space!([RawFd; LISTENERS_MAX]);
    let mut slices = [IoSliceMut::new(&mut text)];
    let flags = MsgFlags::MSG_CMSG_CLOEXEC;
    let message = recvmsg::<()>(channel, &mut slices, Some(&mut space), flags)
        .map_err(|error| format!("bridge handover: {error}"))?;
    let cut = MsgFlags::MSG_TRUNC | MsgFlags::MSG_CTRUNC;
    if message.flags.intersects(cut) {
        return Err("the handover was cut short".into());
    }
    let mut fds: Vec<RawFd> = Vec::new();
    for control in message.cmsgs() {
        if let ControlMessageOwned::ScmRights(received) = control {
            fds.extend(received);
        }
    }
    let bytes = message.bytes;
    assert!(bytes <= ENTRY_BYTES * LISTENERS_MAX);
    listed(&text[..bytes], &fds, ports)
}

/// How many connections the broker carries now, and the signal that one has ended.
struct Live {
    count: Mutex<usize>,
    freed: Condvar,
}

impl Live {
    /// Takes one place among the [`LIVE_MAX`] live connections, waiting for one to end
    /// when all are taken.
    fn take(&self) {
        let mut count = self.count.lock().unwrap_or_else(PoisonError::into_inner);
        while *count >= LIVE_MAX {
            count = self
                .freed
                .wait(count)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *count += 1;
        assert!(*count <= LIVE_MAX);
    }

    /// Gives back the place of a connection that has ended.
    fn free(&self) {
        let mut count = self.count.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(*count > 0);
        *count -= 1;
        self.freed.notify_one();
    }
}

/// Carries connections on every listener, each on a thread of its own, until all listeners
/// are gone. Input: the listeners. Output: the reason the broker has nothing left to carry.
fn carry(listeners: &[Listener]) -> String {
    assert!(!listeners.is_empty() && listeners.len() <= LISTENERS_MAX);
    let live = Arc::new(Live {
        count: Mutex::new(0),
        freed: Condvar::new(),
    });
    std::thread::scope(|scope| {
        for (family, port, listener) in listeners {
            let live = Arc::clone(&live);
            scope.spawn(move || accept_on(*family, *port, *listener, &live));
        }
    });
    "every bridged listener is gone".into()
}

/// Accepts connections on one listener for as long as it is one, each carried on a thread
/// of its own once a place among the live connections is free. A failure that passes waits
/// [`ACCEPT_PAUSE`] and tries again. Inputs: the listener's family, port and descriptor,
/// and the live count.
fn accept_on(family: u8, port: u16, listener: RawFd, live: &Arc<Live>) {
    loop {
        live.take();
        let agent = match accept4(listener, SockFlag::SOCK_CLOEXEC) {
            Ok(agent) => agent,
            Err(Errno::EBADF | Errno::EINVAL | Errno::ENOTSOCK) => return live.free(),
            Err(_) => {
                live.free();
                std::thread::sleep(ACCEPT_PAUSE);
                continue;
            }
        };
        let carried = Arc::clone(live);
        let started = std::thread::Builder::new().spawn(move || {
            relay(family, port, agent);
            carried.free();
        });
        if started.is_err() {
            shut(agent);
            live.free();
        }
    }
}

/// Carries one accepted connection to this computer's loopback on the same port and back,
/// until both directions have ended, then closes it. With nothing listening there the
/// connection is closed at once. Inputs: the family the agent used, the port and the
/// accepted connection.
fn relay(family: u8, port: u16, agent: RawFd) {
    assert!(agent > 2);
    if let Some(host) = reach(family, port) {
        std::thread::scope(|scope| {
            scope.spawn(|| up(agent, &host));
            down(&host, agent);
        });
    }
    shut(agent);
}

/// Connects to this computer's loopback on a bridged port, the family the agent used first
/// and the other one after it, as a program on the shared network would fall back. Inputs:
/// the family and the port. Output: the connection, or none when nothing listens there.
fn reach(family: u8, port: u16) -> Option<TcpStream> {
    assert!(port >= BRIDGE_PORT_MIN);
    let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    let order = if family == V6 { [v6, v4] } else { [v4, v6] };
    crate::spawn_gate::connect_tcp_std(&order[..]).ok()
}

/// Copies what the agent sends to the program on this computer until the agent's side ends
/// or the write fails, then ends that direction for the program. Inputs: the accepted
/// connection and the connection to the program.
fn up(agent: RawFd, mut host: &TcpStream) {
    let mut buffer = vec![0u8; BUFFER_BYTES];
    while let Ok(read @ 1..) = recv(agent, &mut buffer, MsgFlags::empty()) {
        if host.write_all(&buffer[..read]).is_err() {
            break;
        }
    }
    let _ = host.shutdown(std::net::Shutdown::Write);
}

/// Copies what the program on this computer sends to the agent until the program's side
/// ends or the send fails, then ends that direction for the agent. Inputs: the connection
/// to the program and the accepted connection.
fn down(mut host: &TcpStream, agent: RawFd) {
    let mut buffer = vec![0u8; BUFFER_BYTES];
    while let Ok(read @ 1..) = host.read(&mut buffer) {
        if !sent(agent, &buffer[..read]) {
            break;
        }
    }
    let _ = shutdown(agent, Shutdown::Write);
}

/// Sends every byte to the agent. Inputs: the accepted connection and the bytes. Output:
/// true once all are sent, false when the agent's side refuses more.
fn sent(agent: RawFd, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        match send(agent, bytes, MsgFlags::MSG_NOSIGNAL) {
            Ok(wrote @ 1..) => bytes = &bytes[wrote..],
            _ => return false,
        }
    }
    true
}

/// Closes a connection the broker accepted. A close that fails leaves nothing to undo: the
/// descriptor is gone either way. Input: the descriptor.
fn shut(fd: RawFd) {
    let _ = nix::unistd::close(fd);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::Ipv4Addr;
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    fn words(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn ported(ports: std::ops::Range<u16>) -> Vec<OsString> {
        ports
            .flat_map(|port| [OsString::from(BRIDGE), port.to_string().into()])
            .collect()
    }

    #[test]
    fn bridge_flags_lead_the_arguments_and_leave_the_rest_as_it_was() {
        let inside = words(&[BRIDGE_FD, "5", BRIDGE, "8080", BRIDGE, "11434", "--probe"]);
        let (bridge, rest) = Bridge::leading(inside).unwrap();
        let handed = Bridge {
            fd: Some(5),
            ports: vec![8080, 11434],
        };
        assert_eq!(bridge, Some(handed));
        assert_eq!(rest, words(&["--probe"]));
        let outside = words(&[BRIDGE, "8080", "--nest", "--", "/usr/bin/bwrap"]);
        let (bridge, rest) = Bridge::leading(outside).unwrap();
        let asked = Bridge {
            fd: None,
            ports: vec![8080],
        };
        assert_eq!(bridge, Some(asked));
        assert_eq!(rest, words(&["--nest", "--", "/usr/bin/bwrap"]));
        let later = words(&["--probe", BRIDGE, "8080"]);
        assert_eq!(Bridge::leading(later.clone()), Ok((None, later.clone())));
        assert!(HelperRequest::parse(later).is_err());
        let (bridge, rest) = Bridge::leading(ported(8080..8088)).unwrap();
        assert_eq!(bridge.map(|bridge| bridge.ports.len()), Some(8));
        assert!(rest.is_empty());
    }

    #[test]
    fn a_bridge_takes_only_ports_the_helper_can_listen_on_and_one_descriptor_above_the_standard() {
        let refused: [&[&str]; 8] = [
            &[BRIDGE, "1023"],
            &[BRIDGE, "0"],
            &[BRIDGE, "70000"],
            &[BRIDGE],
            &[BRIDGE_FD, "5"],
            &[BRIDGE_FD, "2", BRIDGE, "8080"],
            &[BRIDGE_FD, "5", BRIDGE_FD, "6", BRIDGE, "8080"],
            &[BRIDGE, "8080", BRIDGE, "8080"],
        ];
        for flags in refused {
            assert!(Bridge::leading(words(flags)).is_err(), "{flags:?}");
        }
        assert!(Bridge::leading(ported(8080..8089)).is_err());
        let lowest = words(&[BRIDGE_FD, "3", BRIDGE, "1024"]);
        let held = Bridge {
            fd: Some(3),
            ports: vec![1024],
        };
        assert_eq!(Bridge::leading(lowest), Ok((Some(held), Vec::new())));
    }

    #[test]
    fn only_a_nest_a_probe_and_a_nested_run_take_a_bridge() {
        let asked = |words: Vec<OsString>| HelperRequest::parse(words).unwrap();
        let nest = |net: &[&str]| {
            let mut flags = words(&["--nest", "--", "/usr/bin/bwrap"]);
            flags.extend(words(&NEST_ARGS));
            flags.extend(words(net));
            flags.extend(words(&[
                "--",
                "/opt/crazyagent/crazyagent-landlock",
                "--probe",
            ]));
            asked(flags)
        };
        let (apart, shared) = (nest(&[UNSHARE_NET]), nest(&[]));
        let probe = asked(words(&["--probe"]));
        let nested = asked(words(&["--nested", "--", "/bin/true"]));
        let plain = asked(words(&["--", "/bin/true"]));
        let filter = asked(words(&["--filter-sockets", "--", "/bin/true"]));
        let scoped = asked(words(&["--scoped"]));
        let broker = asked(words(&[BROKER, "4242", "8080"]));
        let bridge = |fd| Bridge {
            fd,
            ports: vec![8080],
        };
        let (outer, handed) = (bridge(None), bridge(Some(5)));
        for request in [&apart, &probe, &nested, &plain, &filter, &scoped, &broker] {
            let taken = [(&apart, &outer), (&probe, &handed), (&nested, &handed)];
            for bridge in [&outer, &handed] {
                let fits = taken.contains(&(request, bridge));
                let said = Bridge::fits(Some(bridge), request);
                assert_eq!(said.is_ok(), fits, "{request:?} with {bridge:?}: {said:?}");
            }
            let alone = Bridge::fits(None, request);
            let own = !std::ptr::eq(request, &apart);
            assert_eq!(alone.is_ok(), own, "{request:?} alone: {alone:?}");
        }
        assert_eq!(Bridge::fits(None, &shared), Ok(()));
        assert!(Bridge::fits(Some(&outer), &shared).is_err());
        assert!(Bridge::fits(Some(&handed), &shared).is_err());
    }

    #[test]
    fn the_broker_is_asked_with_the_process_it_dies_with_and_its_ports_alone() {
        let asked = |flags: &[&str]| HelperRequest::parse(words(flags));
        let carried = HelperRequest::Broker {
            parent: 4242,
            ports: vec![8080, 11434],
        };
        assert_eq!(asked(&[BROKER, "4242", "8080", "11434"]), Ok(carried));
        let refused: [&[&str]; 7] = [
            &[BROKER],
            &[BROKER, "4242"],
            &[BROKER, "1", "8080"],
            &[BROKER, "4242", "80"],
            &[BROKER, "4242", "8080", "8080"],
            &[BROKER, "4242", "8080", "--probe"],
            &["--probe", BROKER, "4242", "8080"],
        ];
        for flags in refused {
            assert!(asked(flags).is_err(), "{flags:?}");
        }
        let mut many = words(&[BROKER, "4242"]);
        many.extend((8080..8089u16).map(|port| OsString::from(port.to_string())));
        assert!(HelperRequest::parse(many).is_err());
    }

    #[test]
    fn the_nest_writes_the_bridge_flags_for_this_helper_and_for_no_other_program() {
        let helper = std::env::current_exe().unwrap();
        let mut args = words(&NEST_ARGS);
        args.extend(words(&[UNSHARE_NET, "--chdir", "/work", "--"]));
        args.push(helper.clone().into());
        args.extend(words(&["--nested", "--", "/bin/sh", "--", "x"]));
        let at = NEST_ARGS.len() + 5;
        let written = inner(&args, 7, &[8080, 11434], &helper).unwrap();
        assert_eq!(written[..at], args[..at]);
        assert_eq!(written[at + 6..], args[at..]);
        let (bridge, rest) = Bridge::leading(written[at..].to_vec()).unwrap();
        let handed = Bridge {
            fd: Some(7),
            ports: vec![8080, 11434],
        };
        assert_eq!(bridge, Some(handed));
        assert_eq!(rest, args[at..]);
        let mut other = args.clone();
        other[at - 1] = "/bin/sh".into();
        assert!(inner(&other, 7, &[8080], &helper).is_err());
        assert!(inner(&args[..at - 2], 7, &[8080], &helper).is_err());
        assert!(inner(&args[..at - 1], 7, &[8080], &helper).is_err());
    }

    #[test]
    fn the_handover_names_each_listener_and_the_broker_takes_only_its_own_ports() {
        let text = entries(&[(V4, 8080), (V6, 8080), (V4, 11434)]);
        assert_eq!(text, [4, 0x1f, 0x90, 6, 0x1f, 0x90, 4, 0x2c, 0xaa]);
        let carried = vec![(V4, 8080, 10), (V6, 8080, 11), (V4, 11434, 12)];
        assert_eq!(listed(&text, &[10, 11, 12], &[8080, 11434]), Ok(carried));
        assert!(listed(&text, &[10, 11], &[8080, 11434]).is_err());
        assert!(listed(&text, &[10, 11, 12, 13], &[8080, 11434]).is_err());
        assert!(listed(&text, &[10, 11, 12], &[8080]).is_err());
        assert!(listed(&text[..8], &[10, 11, 12], &[8080, 11434]).is_err());
        assert!(listed(&[], &[], &[8080]).is_err());
        let unknown = entries(&[(5, 8080)]);
        assert!(listed(&unknown, &[10], &[8080]).is_err());
        let twice = entries(&[(V4, 8080), (V4, 8080)]);
        assert!(listed(&twice, &[10, 11], &[8080]).is_err());
        let half = entries(&[(V6, 8080)]);
        assert!(listed(&half, &[10], &[8080]).is_err());
        let short = entries(&[(V4, 8080)]);
        assert!(listed(&short, &[10], &[8080, 11434]).is_err());
    }

    #[test]
    fn the_broker_carries_a_connection_both_ways_to_the_same_port_on_this_computer() {
        let program = crate::spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = program.local_addr().unwrap().port();
        program.set_nonblocking(true).unwrap();
        let nest = crate::spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut agent = crate::spawn_gate::connect_tcp_std(nest.local_addr().unwrap()).unwrap();
        let accepted = accept4(nest.as_raw_fd(), SockFlag::SOCK_CLOEXEC).unwrap();
        let carried = std::thread::spawn(move || relay(V4, port, accepted));
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut served = loop {
            match crate::spawn_gate::accept_tcp_std(&program) {
                Ok((served, _)) => break served,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("the broker reached nothing on port {port}: {error}"),
            }
        };
        served.set_nonblocking(false).unwrap();
        agent.write_all(b"ping").unwrap();
        let mut asked = [0u8; 4];
        served.read_exact(&mut asked).unwrap();
        assert_eq!(&asked, b"ping");
        served.write_all(b"pong").unwrap();
        drop(served);
        let mut answer = Vec::new();
        agent.read_to_end(&mut answer).unwrap();
        assert_eq!(answer, b"pong");
        drop(agent);
        carried.join().unwrap();
    }

    #[test]
    fn a_bridged_port_with_nothing_behind_it_closes_the_connection() {
        let free = crate::spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        let nest = crate::spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut agent = crate::spawn_gate::connect_tcp_std(nest.local_addr().unwrap()).unwrap();
        let accepted = accept4(nest.as_raw_fd(), SockFlag::SOCK_CLOEXEC).unwrap();
        relay(V4, port, accepted);
        let mut answer = Vec::new();
        assert_eq!(agent.read_to_end(&mut answer).ok(), Some(0));
    }
}
