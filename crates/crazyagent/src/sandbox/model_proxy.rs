use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The most bytes a CONNECT request head may take before the proxy refuses it.
pub const HEADER_BYTES_MAX: usize = 8 * 1024;
/// How long a client has to send its CONNECT request and its TLS ClientHello.
pub const HANDSHAKE_TIME: Duration = Duration::from_secs(10);
/// The most tunnels one agent's proxy keeps open at once.
pub const TUNNELS_MAX: usize = 16;
/// How long a tunnel may carry nothing in either direction before the proxy closes it.
pub const IDLE_TIME: Duration = Duration::from_secs(600);
/// The most hosts one proxy lets its agent reach.
pub const HOSTS_MAX: usize = 8;

const RECORD_BYTES_MAX: usize = 5 + 16 * 1024;
const POLL: Duration = Duration::from_millis(250);
const WRITE_TIME: Duration = Duration::from_secs(60);
const HOST_BYTES_MAX: usize = 253;
const COPY_BYTES: usize = 16 * 1024;
const SHOWN_BYTES_MAX: usize = 96;
const ESTABLISHED: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";

/// One host a model proxy lets an agent reach: a DNS name, never an IP literal, and its
/// port. Names are kept in lower case, as hosts compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelHost {
    name: String,
    port: u16,
}

impl ModelHost {
    /// Checks one host. Inputs: the DNS name and the port. Output: the host, or none for
    /// an IP literal, a name that is not a plain DNS name, or port 0.
    pub fn new(name: &str, port: u16) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        (port != 0 && dns_name(&name)).then_some(Self { name, port })
    }

    /// The host's DNS name, in lower case.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The host's port.
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// Tells whether `name` is a lower-case DNS name of letters, digits and inner hyphens that
/// no resolver reads as an address: not an IP literal, no label in hex form, no all-digit
/// last label. Input: the name. Output: true for a plain name.
fn dns_name(name: &str) -> bool {
    let labels: Vec<&str> = name.split('.').collect();
    !name.is_empty()
        && name.len() <= HOST_BYTES_MAX
        && name.parse::<IpAddr>().is_err()
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && !label.starts_with("0x")
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && !labels
            .last()
            .is_some_and(|last| last.bytes().all(|b| b.is_ascii_digit()))
}

/// The variables that send a harness's HTTPS and HTTP traffic, and its tools', through the
/// proxy on `port`, with no host exempt: upper and lower case, as programs read either.
/// Input: the proxy's port. Output: the name and value pairs.
pub fn variables(port: u16) -> [(&'static str, String); 6] {
    assert_ne!(port, 0);
    let url = format!("http://127.0.0.1:{port}");
    [
        ("HTTPS_PROXY", url.clone()),
        ("https_proxy", url.clone()),
        ("HTTP_PROXY", url.clone()),
        ("http_proxy", url),
        ("NO_PROXY", String::new()),
        ("no_proxy", String::new()),
    ]
}

/// One agent's model proxy (S4b): a listener on 127.0.0.1 that opens a CONNECT tunnel
/// only to a host on the agent's list, resolves the host itself and passes the TLS bytes
/// through untouched, after checking that the ClientHello names the same host. The agent's
/// Seatbelt profile reaches only this port, so the proxy is its one way to the network.
/// Dropping it closes the listener and, within a poll, every open tunnel.
pub struct ModelProxy {
    port: u16,
    stop: Arc<AtomicBool>,
    open: Arc<AtomicUsize>,
    listener: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ModelProxy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelProxy")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl ModelProxy {
    /// Starts one agent's proxy on 127.0.0.1, on `port` when a kept pane needs the port its
    /// profile names again, otherwise on a free port. Inputs: the agent id its refusal lines
    /// name, its hosts (1 to `HOSTS_MAX`) and the port. Output: the running proxy, or the
    /// bind error, such as a saved port another process took.
    pub fn bind(agent: i64, hosts: Vec<ModelHost>, port: Option<u16>) -> io::Result<Self> {
        assert!(!hosts.is_empty() && hosts.len() <= HOSTS_MAX);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port.unwrap_or(0)))?;
        let bound = listener.local_addr()?.port();
        assert!(bound != 0 && port.is_none_or(|port| port == bound));
        let stop = Arc::new(AtomicBool::new(false));
        let open = Arc::new(AtomicUsize::new(0));
        let (flag, count, hosts) = (stop.clone(), open.clone(), Arc::new(hosts));
        let thread = std::thread::Builder::new()
            .name(format!("model-proxy-{agent}"))
            .spawn(move || serve(agent, &listener, &hosts, &flag, &count))?;
        Ok(Self {
            port: bound,
            stop,
            open,
            listener: Some(thread),
        })
    }

    /// The loopback port the proxy listens on, which the agent's profile and variables name.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for ModelProxy {
    /// Stops the listener, wakes its accept with one connection of its own, and waits a
    /// short bound for the open tunnels to notice the stop. Takes the proxy.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)),
            Duration::from_secs(1),
        );
        if let Some(thread) = self.listener.take() {
            let _ = thread.join();
        }
        let deadline = Instant::now() + POLL * 8;
        while self.open.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(POLL / 10);
        }
    }
}

/// Counts one open tunnel for as long as it lives.
struct Held(Arc<AtomicUsize>);

impl Drop for Held {
    fn drop(&mut self) {
        let before = self.0.fetch_sub(1, Ordering::SeqCst);
        assert!((1..=TUNNELS_MAX).contains(&before));
    }
}

/// Accepts the agent's connections until the proxy stops: each one becomes a tunnel thread,
/// and one past `TUNNELS_MAX` gets a 503. Inputs: the agent, the listener, its hosts, the
/// stop flag and the open-tunnel count. Output: none.
fn serve(
    agent: i64,
    listener: &TcpListener,
    hosts: &Arc<Vec<ModelHost>>,
    stop: &Arc<AtomicBool>,
    open: &Arc<AtomicUsize>,
) {
    for incoming in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(client) = incoming else {
            std::thread::sleep(POLL / 10);
            continue;
        };
        let admitted = open.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            (count < TUNNELS_MAX).then_some(count + 1)
        });
        if admitted.is_err() {
            refuse(
                agent,
                &client,
                "503 Service Unavailable",
                "too many tunnels",
            );
            continue;
        }
        let held = Held(open.clone());
        let (hosts, stop) = (hosts.clone(), stop.clone());
        let _ = std::thread::Builder::new()
            .name(format!("model-tunnel-{agent}"))
            .spawn(move || {
                let _held = held;
                tunnel(agent, client, &hosts, &stop);
            });
    }
}

/// Answers a connection the proxy will not tunnel and logs the agent and what it asked
/// for. Inputs: the agent, the client, the status line and what was asked. Output: none.
fn refuse(agent: i64, mut client: &TcpStream, status: &str, asked: &str) {
    eprintln!("model_proxy_refused agent={agent} host={}", shown(asked));
    let _ = client.write_all(
        format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
    );
    let _ = client.shutdown(Shutdown::Both);
}

/// Cuts a tunnel the proxy already answered with 200 and logs why. Inputs: the agent, the
/// client, the host and the reason. Output: none.
fn cut(agent: i64, client: &TcpStream, host: &str, reason: &str) {
    eprintln!(
        "model_proxy_cut agent={agent} host={} reason={reason}",
        shown(host)
    );
    let _ = client.shutdown(Shutdown::Both);
}

/// What a log line shows of a client's words: at most `SHOWN_BYTES_MAX` characters, each
/// one that is not printable ASCII shown as `_`, so no client can write its own log line.
/// Input: the words. Output: the shown text.
fn shown(asked: &str) -> String {
    asked
        .chars()
        .take(SHOWN_BYTES_MAX)
        .map(|c| if c.is_ascii_graphic() { c } else { '_' })
        .collect()
}

/// Serves one client: its CONNECT request must name a host on the list, its first TLS
/// record must be a ClientHello whose server name is that host, and only then does the
/// proxy resolve the host, connect and pass bytes both ways. Inputs: the agent, the client,
/// its hosts and the stop flag. Output: none; the tunnel ends when either side closes.
fn tunnel(agent: i64, client: TcpStream, hosts: &[ModelHost], stop: &AtomicBool) {
    let deadline = Instant::now() + HANDSHAKE_TIME;
    let Ok((head, rest)) = read_head(&client, deadline) else {
        refuse(agent, &client, "403 Forbidden", "unreadable request");
        return;
    };
    let first = head.lines().next().unwrap_or_default();
    let Some(host) = connect_target(first).filter(|target| hosts.contains(target)) else {
        refuse(
            agent,
            &client,
            "403 Forbidden",
            first.split(' ').nth(1).unwrap_or(first),
        );
        return;
    };
    if (&client).write_all(ESTABLISHED).is_err() {
        return;
    }
    let Ok((hello, record)) = read_record(&client, rest, deadline) else {
        cut(agent, &client, host.name(), "no ClientHello");
        return;
    };
    if !server_name(&hello[..record]).is_some_and(|name| name.eq_ignore_ascii_case(host.name())) {
        cut(agent, &client, host.name(), "server name differs");
        return;
    }
    if stop.load(Ordering::SeqCst) {
        cut(agent, &client, host.name(), "proxy stopped");
        return;
    }
    let Ok(upstream) = open_upstream(&host, deadline) else {
        cut(agent, &client, host.name(), "unreachable");
        return;
    };
    if (&upstream).write_all(&hello).is_err() {
        return;
    }
    pump(client, upstream, stop);
}

/// Reads a request head, up to its blank line, within `HEADER_BYTES_MAX` and the deadline.
/// Inputs: the client and the deadline. Output: the head as text and any bytes the client
/// sent after it, or an error for a head too long, not text, or not finished in time.
fn read_head(mut client: &TcpStream, deadline: Instant) -> io::Result<(String, Vec<u8>)> {
    let mut buffer = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buffer.split_off(end + 4);
            let head = String::from_utf8(buffer)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "head is not text"))?;
            return Ok((head, rest));
        }
        if buffer.len() > HEADER_BYTES_MAX {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "head too long"));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        client.set_read_timeout(Some(left))?;
        let read = client.read(&mut chunk)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

/// Reads the client's first TLS record whole, within `RECORD_BYTES_MAX` and the deadline.
/// Inputs: the client, the bytes it already sent and the deadline. Output: every byte read
/// so far and where the first record ends, or an error for anything but a handshake record.
fn read_record(
    mut client: &TcpStream,
    mut buffer: Vec<u8>,
    deadline: Instant,
) -> io::Result<(Vec<u8>, usize)> {
    let mut chunk = [0u8; 4096];
    loop {
        if buffer.len() >= 5 {
            if buffer[0] != 0x16 || buffer[1] != 0x03 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "not a handshake",
                ));
            }
            let end = 5 + usize::from(u16::from_be_bytes([buffer[3], buffer[4]]));
            if end > RECORD_BYTES_MAX {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "record too long",
                ));
            }
            if buffer.len() >= end {
                return Ok((buffer, end));
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        client.set_read_timeout(Some(left))?;
        let read = client.read(&mut chunk)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > RECORD_BYTES_MAX + chunk.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "record too long",
            ));
        }
    }
}

/// Reads the host a CONNECT request line names: `CONNECT name:port HTTP/1.x`, the name a
/// plain DNS name. Input: the request line. Output: the host, or none for anything else.
fn connect_target(line: &str) -> Option<ModelHost> {
    let mut words = line.split(' ');
    let (method, target, version) = (words.next()?, words.next()?, words.next()?);
    if words.next().is_some() || method != "CONNECT" || !matches!(version, "HTTP/1.1" | "HTTP/1.0")
    {
        return None;
    }
    let (name, port) = target.rsplit_once(':')?;
    if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    ModelHost::new(name, port.parse().ok()?)
}

/// Reads the server name a TLS ClientHello carries in its server_name extension. The
/// ClientHello must fit in the record. Input: one whole TLS record. Output: the name, or
/// none for another message, a malformed one, no name, or more than one host name.
pub fn server_name(record: &[u8]) -> Option<&str> {
    let length = usize::from(u16::from_be_bytes([*record.get(3)?, *record.get(4)?]));
    let body = record.get(5..5 + length)?;
    if *record.first()? != 0x16 || *body.first()? != 0x01 {
        return None;
    }
    let size = (usize::from(*body.get(1)?) << 16)
        | (usize::from(*body.get(2)?) << 8)
        | usize::from(*body.get(3)?);
    let hello = body.get(4..4 + size)?;
    let mut at = 2 + 32;
    at += 1 + usize::from(*hello.get(at)?);
    at += 2 + usize::from(u16::from_be_bytes([*hello.get(at)?, *hello.get(at + 1)?]));
    at += 1 + usize::from(*hello.get(at)?);
    let total = usize::from(u16::from_be_bytes([*hello.get(at)?, *hello.get(at + 1)?]));
    let mut extensions = hello.get(at + 2..at + 2 + total)?;
    while !extensions.is_empty() {
        let kind = u16::from_be_bytes([*extensions.first()?, *extensions.get(1)?]);
        let size = usize::from(u16::from_be_bytes([
            *extensions.get(2)?,
            *extensions.get(3)?,
        ]));
        let data = extensions.get(4..4 + size)?;
        extensions = &extensions[4 + size..];
        if kind != 0 {
            continue;
        }
        let listed = usize::from(u16::from_be_bytes([*data.first()?, *data.get(1)?]));
        let mut names = data.get(2..2 + listed)?;
        let mut found = None;
        while !names.is_empty() {
            let (kind, size) = (
                *names.first()?,
                usize::from(u16::from_be_bytes([*names.get(1)?, *names.get(2)?])),
            );
            let name = names.get(3..3 + size)?;
            names = &names[3 + size..];
            if kind == 0 {
                if found.is_some() {
                    return None;
                }
                found = Some(std::str::from_utf8(name).ok()?);
            }
        }
        return found;
    }
    None
}

/// Resolves one host on the engine's side and connects to the first address that answers
/// before the deadline. Inputs: the host and the deadline. Output: the connection.
fn open_upstream(host: &ModelHost, deadline: Instant) -> io::Result<TcpStream> {
    let mut last = io::Error::from(io::ErrorKind::NotFound);
    for address in (host.name(), host.port()).to_socket_addrs()? {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match TcpStream::connect_timeout(&address, left) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// Passes bytes both ways until either side closes, the proxy stops or nothing moved for
/// `IDLE_TIME`. Inputs: the client, the upstream connection and the stop flag. Output: none.
fn pump(client: TcpStream, upstream: TcpStream, stop: &AtomicBool) {
    let started = Instant::now();
    let moved = AtomicU64::new(0);
    let (Ok(client_back), Ok(upstream_back)) = (client.try_clone(), upstream.try_clone()) else {
        let _ = client.shutdown(Shutdown::Both);
        let _ = upstream.shutdown(Shutdown::Both);
        return;
    };
    std::thread::scope(|scope| {
        scope.spawn(|| copy(&upstream_back, &client_back, stop, started, &moved));
        copy(&client, &upstream, stop, started, &moved);
    });
}

/// Copies one direction of a tunnel. On the reading side's end it ends the writing side's
/// output; on a stop, an idle tunnel or an error it closes both. Inputs: the two sides, the
/// stop flag, the tunnel's start and the time anything last moved. Output: none.
fn copy(
    mut from: &TcpStream,
    mut to: &TcpStream,
    stop: &AtomicBool,
    started: Instant,
    moved: &AtomicU64,
) {
    let mut buffer = vec![0u8; COPY_BYTES];
    let ready =
        from.set_read_timeout(Some(POLL)).is_ok() && to.set_write_timeout(Some(WRITE_TIME)).is_ok();
    while ready && !stop.load(Ordering::SeqCst) {
        let since = started.elapsed().as_millis();
        match from.read(&mut buffer) {
            Ok(0) => {
                let _ = to.shutdown(Shutdown::Write);
                return;
            }
            Ok(read) => {
                if to.write_all(&buffer[..read]).is_err() {
                    break;
                }
                moved.store(u64::try_from(since).unwrap_or(u64::MAX), Ordering::SeqCst);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                let quiet = since.saturating_sub(u128::from(moved.load(Ordering::SeqCst)));
                if quiet >= IDLE_TIME.as_millis() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = from.shutdown(Shutdown::Both);
    let _ = to.shutdown(Shutdown::Both);
}

#[cfg(test)]
mod tests;
