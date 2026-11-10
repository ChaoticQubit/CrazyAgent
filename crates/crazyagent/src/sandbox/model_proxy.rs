use crate::spawn_gate;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, TcpListener, TcpStream};
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
const ACCEPT_POLL: Duration = Duration::from_millis(5);
const WRITE_TIME: Duration = Duration::from_secs(60);
const HOST_BYTES_MAX: usize = 253;
const COPY_BYTES: usize = 16 * 1024;
const SHOWN_BYTES_MAX: usize = 96;
const ESTABLISHED: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";
const SERVER_NAME: u16 = 0x0000;
const ENCRYPTED_CLIENT_HELLO: u16 = 0xfe0d;
const ENCRYPTED_SERVER_NAME: u16 = 0xffce;
const BIND_TRIES: usize = 16;
const REFUSALS_SHOWN: u64 = 20;
const REFUSAL_TOTAL_EVERY: Duration = Duration::from_secs(60);
const OWN_ADDRESS: &str = "resolves to this machine";

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

/// One agent's model proxy (S4b): a listener on 127.0.0.1 and one on ::1, on the same port,
/// that opens a CONNECT tunnel only to a host on the agent's list, resolves the host itself
/// and passes the TLS bytes through untouched, after checking that the ClientHello names the
/// same host. The agent's Seatbelt profile reaches only this port on loopback, which Seatbelt
/// names `localhost` and which covers both addresses, so holding both leaves no other
/// process a side of the port while the proxy runs (S4B-2). Dropping it closes the
/// listeners and, within a poll, every open tunnel.
pub struct ModelProxy {
    port: u16,
    shared: Arc<Shared>,
    listener: Option<JoinHandle<()>>,
}

/// What one proxy's threads share: the agent its log lines name, its hosts, the stop flag,
/// the open-tunnel count, the refusals so far, when its last refusal total was logged (in
/// milliseconds from `started`), and whether its tunnels may dial an address on this machine,
/// which only a test's proxy may.
struct Shared {
    agent: i64,
    hosts: Vec<ModelHost>,
    stop: AtomicBool,
    open: AtomicUsize,
    refusals: AtomicU64,
    totaled: AtomicU64,
    started: Instant,
    any_address: bool,
}

impl Shared {
    /// Counts one refusal and logs its line while the proxy has logged fewer than
    /// `REFUSALS_SHOWN`, after that a total at most once per `REFUSAL_TOTAL_EVERY`, so a
    /// client refused in a loop cannot fill the engine log (CS-5). Input: the line, which
    /// names no client secret. Output: none.
    fn log(&self, line: &str) {
        let count = self.refusals.fetch_add(1, Ordering::SeqCst) + 1;
        let now = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let last = self.totaled.load(Ordering::SeqCst);
        match shown_refusal(count, Duration::from_millis(now.saturating_sub(last))) {
            Shown::Line => eprintln!("{line}"),
            Shown::Total
                if self
                    .totaled
                    .compare_exchange(last, now, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok() =>
            {
                eprintln!("model_proxy_refusals agent={} total={count}", self.agent);
            }
            Shown::Total | Shown::Quiet => {}
        }
    }
}

/// What a proxy logs for one refusal: its line, a running total, or nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Line,
    Total,
    Quiet,
}

/// Which refusals a proxy logs: the first `REFUSALS_SHOWN` in full, then a total once at
/// least `REFUSAL_TOTAL_EVERY` has passed since the last total (CS-5). Inputs: the
/// refusal's count, from 1, and the time since the last total. Output: what to log.
fn shown_refusal(count: u64, since_total: Duration) -> Shown {
    assert!(count >= 1);
    if count <= REFUSALS_SHOWN {
        Shown::Line
    } else if since_total >= REFUSAL_TOTAL_EVERY {
        Shown::Total
    } else {
        Shown::Quiet
    }
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
    /// Starts one agent's proxy on 127.0.0.1 and ::1, on `port` when a kept pane needs the
    /// port its profile names again, otherwise on a port free on both. Inputs: the agent id
    /// its refusal lines name, its hosts (1 to `HOSTS_MAX`) and the port. Output: the running
    /// proxy, or the bind error, such as a saved port another process took on either address.
    pub fn bind(agent: i64, hosts: Vec<ModelHost>, port: Option<u16>) -> io::Result<Self> {
        Self::start(agent, hosts, port, false)
    }

    /// The same proxy, except that its tunnels may also dial addresses on this machine, so a
    /// test's tunnels reach a local listener. Inputs and output as [`ModelProxy::bind`].
    #[cfg(test)]
    pub(crate) fn bind_local(
        agent: i64,
        hosts: Vec<ModelHost>,
        port: Option<u16>,
    ) -> io::Result<Self> {
        Self::start(agent, hosts, port, true)
    }

    /// How many refusals the proxy has counted. Output: the count.
    #[cfg(test)]
    pub(crate) fn refusals(&self) -> u64 {
        self.shared.refusals.load(Ordering::SeqCst)
    }

    /// Starts the proxy [`ModelProxy::bind`] describes. Inputs: the agent, its hosts, the
    /// port, and whether its tunnels may dial addresses on this machine. Output: the
    /// running proxy, or the bind error.
    fn start(
        agent: i64,
        hosts: Vec<ModelHost>,
        port: Option<u16>,
        any_address: bool,
    ) -> io::Result<Self> {
        assert!(!hosts.is_empty() && hosts.len() <= HOSTS_MAX);
        let listeners = loopback_pair(port)?;
        let bound = listeners[0].local_addr()?.port();
        assert!(bound != 0 && port.is_none_or(|port| port == bound));
        for listener in &listeners {
            let address = listener.local_addr()?;
            assert!(address.ip().is_loopback() && address.port() == bound);
            listener.set_nonblocking(true)?;
        }
        let shared = Arc::new(Shared {
            agent,
            hosts,
            stop: AtomicBool::new(false),
            open: AtomicUsize::new(0),
            refusals: AtomicU64::new(0),
            totaled: AtomicU64::new(0),
            started: Instant::now(),
            any_address,
        });
        let serving = shared.clone();
        let thread = std::thread::Builder::new()
            .name(format!("model-proxy-{agent}"))
            .spawn(move || serve(&listeners, &serving))?;
        Ok(Self {
            port: bound,
            shared,
            listener: Some(thread),
        })
    }

    /// The loopback port the proxy listens on, which the agent's profile and variables name.
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// Binds 127.0.0.1 and ::1 on one port: `port` when given, otherwise a free port of
/// 127.0.0.1 that ::1 also has free, trying again up to `BIND_TRIES` times. Input: the port.
/// Output: the two listeners, 127.0.0.1 first, or the first bind error.
fn loopback_pair(port: Option<u16>) -> io::Result<[TcpListener; 2]> {
    let mut tries = 1;
    loop {
        let v4 = spawn_gate::bind_tcp_std((Ipv4Addr::LOCALHOST, port.unwrap_or(0)))?;
        let bound = v4.local_addr()?.port();
        match spawn_gate::bind_tcp_std((Ipv6Addr::LOCALHOST, bound)) {
            Ok(v6) => return Ok([v4, v6]),
            Err(error)
                if port.is_none()
                    && error.kind() == io::ErrorKind::AddrInUse
                    && tries < BIND_TRIES =>
            {
                tries += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

impl Drop for ModelProxy {
    /// Stops the listeners, which the serving thread sees within one accept poll, waits a
    /// short bound for the open tunnels to notice the stop, and logs the refusal total when
    /// the proxy logged only some of its refusal lines. Takes the proxy.
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.listener.take() {
            let _ = thread.join();
        }
        let deadline = Instant::now() + POLL * 8;
        while self.shared.open.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(POLL / 10);
        }
        let count = self.shared.refusals.load(Ordering::SeqCst);
        if count > REFUSALS_SHOWN {
            eprintln!(
                "model_proxy_refusals agent={} total={count}",
                self.shared.agent
            );
        }
    }
}

/// Counts one open tunnel for as long as it lives.
struct Held(Arc<Shared>);

impl Drop for Held {
    fn drop(&mut self) {
        let before = self.0.open.fetch_sub(1, Ordering::SeqCst);
        assert!((1..=TUNNELS_MAX).contains(&before));
    }
}

/// Accepts the agent's connections on both non-blocking listeners until the proxy stops,
/// polling every `ACCEPT_POLL` while neither has one waiting. Inputs: the listeners and
/// what the proxy's threads share. Output: none.
fn serve(listeners: &[TcpListener; 2], shared: &Arc<Shared>) {
    while !shared.stop.load(Ordering::SeqCst) {
        let mut waiting = false;
        for listener in listeners {
            if let Ok((client, _)) = spawn_gate::accept_tcp_std(listener) {
                waiting = true;
                admit(client, shared);
            }
        }
        if !waiting {
            std::thread::sleep(ACCEPT_POLL);
        }
    }
}

/// Starts a tunnel thread for one accepted connection, or answers a 503 to one past
/// `TUNNELS_MAX`. Inputs: the connection and what the proxy's threads share. Output: none.
fn admit(client: TcpStream, shared: &Arc<Shared>) {
    let admitted = shared
        .open
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            (count < TUNNELS_MAX).then_some(count + 1)
        });
    if admitted.is_err() {
        refuse(
            shared,
            &client,
            "503 Service Unavailable",
            &format!(
                "model_proxy_refused agent={} reason=too_many_tunnels",
                shared.agent
            ),
        );
        return;
    }
    let held = Held(shared.clone());
    let _ = std::thread::Builder::new()
        .name(format!("model-tunnel-{}", shared.agent))
        .spawn(move || tunnel(client, &held.0));
}

/// The log line for a refused request: the agent, the request's method and the host and
/// port it named, never its path, query or user info, which may hold a credential (S4B-3).
/// A method that is not a plain upper-case word, or a target with no plain host, shows as
/// `-`. Inputs: the agent and the request line. Output: the line.
fn refusal(agent: i64, line: &str) -> String {
    let mut words = line.split(' ');
    let (method, target) = (
        words.next().unwrap_or_default(),
        words.next().unwrap_or_default(),
    );
    let host = if method == "CONNECT" {
        target
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b))
            .then(|| target.to_owned())
    } else {
        url::Url::parse(target).ok().and_then(|url| {
            let name = url.host_str()?;
            Some(match url.port_or_known_default() {
                Some(port) => format!("{name}:{port}"),
                None => name.to_owned(),
            })
        })
    };
    let method = if !method.is_empty()
        && method.len() <= 16
        && method.bytes().all(|b| b.is_ascii_uppercase())
    {
        method
    } else {
        "-"
    };
    format!(
        "model_proxy_refused agent={agent} method={method} host={}",
        shown(
            host.as_deref()
                .filter(|host| !host.is_empty())
                .unwrap_or("-")
        )
    )
}

/// Answers a connection the proxy will not tunnel and counts it, logging why within the
/// proxy's bound. Inputs: what the proxy's threads share, the client, the status line and
/// the log line, which names no client secret. Output: none.
fn refuse(shared: &Shared, mut client: &TcpStream, status: &str, line: &str) {
    shared.log(line);
    let _ = client.write_all(
        format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
    );
    let _ = client.shutdown(Shutdown::Both);
}

/// Cuts a tunnel the proxy already answered with 200 and counts it, logging why within the
/// proxy's bound. Inputs: what the proxy's threads share, the client, the host and the
/// reason. Output: none.
fn cut(shared: &Shared, client: &TcpStream, host: &str, reason: &str) {
    shared.log(&format!(
        "model_proxy_cut agent={} host={} reason={reason}",
        shared.agent,
        shown(host)
    ));
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
/// proxy resolve the host, connect and pass bytes both ways. Inputs: the client and what
/// the proxy's threads share. Output: none; the tunnel ends when either side closes.
fn tunnel(client: TcpStream, shared: &Shared) {
    let deadline = Instant::now() + HANDSHAKE_TIME;
    let Ok((head, rest)) = read_head(&client, deadline) else {
        refuse(
            shared,
            &client,
            "403 Forbidden",
            &format!(
                "model_proxy_refused agent={} reason=unreadable_request",
                shared.agent
            ),
        );
        return;
    };
    let first = head.lines().next().unwrap_or_default();
    let Some(host) = connect_target(first).filter(|target| shared.hosts.contains(target)) else {
        refuse(
            shared,
            &client,
            "403 Forbidden",
            &refusal(shared.agent, first),
        );
        return;
    };
    if (&client).write_all(ESTABLISHED).is_err() {
        return;
    }
    let Ok((hello, record)) = read_record(&client, rest, deadline) else {
        cut(shared, &client, host.name(), "no ClientHello");
        return;
    };
    if !server_name(&hello[..record]).is_some_and(|name| name.eq_ignore_ascii_case(host.name())) {
        cut(shared, &client, host.name(), "server name differs");
        return;
    }
    if shared.stop.load(Ordering::SeqCst) {
        cut(shared, &client, host.name(), "proxy stopped");
        return;
    }
    let upstream = match open_upstream(&host, deadline, shared.any_address) {
        Ok(upstream) => upstream,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            cut(shared, &client, host.name(), "resolves to this machine");
            return;
        }
        Err(_) => {
            cut(shared, &client, host.name(), "unreachable");
            return;
        }
    };
    if (&upstream).write_all(&hello).is_err() {
        return;
    }
    pump(client, upstream, &shared.stop);
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

/// Reads the server name a TLS ClientHello carries in its server_name extension. The record
/// must hold exactly one ClientHello, parsed to its last byte. A hello that carries an
/// Encrypted Client Hello or the older encrypted server name is refused, since its outer
/// name need not be the host the client reaches (S4B-1). Input: one whole TLS record.
/// Output: the name, or none for another message, a malformed or trailing byte, no name,
/// more than one host name or server_name extension, or an encrypted name.
pub fn server_name(record: &[u8]) -> Option<&str> {
    let length = usize::from(u16::from_be_bytes([*record.get(3)?, *record.get(4)?]));
    if *record.first()? != 0x16 || record.len() != 5 + length {
        return None;
    }
    let body = &record[5..];
    if *body.first()? != 0x01 {
        return None;
    }
    let size = (usize::from(*body.get(1)?) << 16)
        | (usize::from(*body.get(2)?) << 8)
        | usize::from(*body.get(3)?);
    if body.len() != 4 + size {
        return None;
    }
    let hello = &body[4..];
    let mut at = 2 + 32;
    let session = usize::from(*hello.get(at)?);
    if session > 32 {
        return None;
    }
    at += 1 + session;
    at += 2 + usize::from(u16::from_be_bytes([*hello.get(at)?, *hello.get(at + 1)?]));
    at += 1 + usize::from(*hello.get(at)?);
    let total = usize::from(u16::from_be_bytes([*hello.get(at)?, *hello.get(at + 1)?]));
    if hello.len() != at + 2 + total {
        return None;
    }
    let mut extensions = &hello[at + 2..];
    let mut found = None;
    let mut named = false;
    while !extensions.is_empty() {
        let kind = u16::from_be_bytes([*extensions.first()?, *extensions.get(1)?]);
        let size = usize::from(u16::from_be_bytes([
            *extensions.get(2)?,
            *extensions.get(3)?,
        ]));
        let data = extensions.get(4..4 + size)?;
        extensions = &extensions[4 + size..];
        match kind {
            ENCRYPTED_CLIENT_HELLO | ENCRYPTED_SERVER_NAME => return None,
            SERVER_NAME if named => return None,
            SERVER_NAME => {
                named = true;
                found = Some(host_name(data)?);
            }
            _ => {}
        }
    }
    found
}

/// Reads the one host name of a server_name extension's list, which must end at its last
/// byte. Input: the extension's data. Output: the name, or none for a malformed list, no
/// host name or more than one.
fn host_name(data: &[u8]) -> Option<&str> {
    let listed = usize::from(u16::from_be_bytes([*data.first()?, *data.get(1)?]));
    if data.len() != 2 + listed {
        return None;
    }
    let mut names = &data[2..];
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
    found
}

/// Resolves one host on the engine's side and connects to the first address that answers
/// before the deadline, skipping each address [`dialable`] refuses or [`on_this_machine`]
/// finds held unless `any_address` says otherwise. Each socket is made inside the spawn
/// gate, not held while connecting. Inputs: the host, the deadline and that choice. Output:
/// the blocking connection, or PermissionDenied when every address the name gave was refused.
fn open_upstream(host: &ModelHost, deadline: Instant, any_address: bool) -> io::Result<TcpStream> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let connected = runtime.block_on(async {
        tokio::time::timeout(left, async {
            let resolved: Vec<std::net::SocketAddr> =
                tokio::net::lookup_host((host.name(), host.port()))
                    .await?
                    .collect();
            let allowed: Vec<std::net::SocketAddr> = resolved
                .iter()
                .copied()
                .filter(|address| {
                    any_address || (dialable(address.ip()) && !on_this_machine(address.ip()))
                })
                .collect();
            if allowed.is_empty() && !resolved.is_empty() {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            spawn_gate::connect_tcp(&allowed[..]).await
        })
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??
        .into_std()
    });
    runtime.shutdown_background();
    let stream = connected?;
    stream.set_nonblocking(false)?;
    Ok(stream)
}

/// Tells whether a tunnel may dial `address`, one a model host's name resolved to: not
/// loopback, unspecified, link-local or in 0.0.0.0/8, also as an IPv4 address inside an
/// IPv6 one. The engine dials outside the sandbox, so a host's DNS must not steer it to this
/// machine (CS-4); private network addresses stay allowed for an owner's endpoint on another
/// machine of the local network. Input: the address. Output: true when the proxy may dial it.
fn dialable(address: IpAddr) -> bool {
    let v4 = |ip: Ipv4Addr| {
        !(ip.is_loopback() || ip.is_unspecified() || ip.is_link_local() || ip.octets()[0] == 0)
    };
    match address {
        IpAddr::V4(ip) => v4(ip),
        IpAddr::V6(ip) => {
            !(ip.is_loopback() || ip.is_unspecified() || ip.is_unicast_link_local())
                && ip.to_ipv4().is_none_or(v4)
        }
    }
}

/// Tells whether one of this machine's interfaces holds `address`, such as its own LAN
/// address, which reaches the owner's services from outside the sandbox as loopback does
/// (S4B-I1). A socket binds only to an address an interface holds, and binding sends nothing.
/// An IPv4 address inside an IPv6 one is tried as IPv4. Any other bind error counts as held,
/// so a failed check refuses the dial. Input: the address. Output: true when held or unsure.
fn on_this_machine(address: IpAddr) -> bool {
    let held = |ip: IpAddr| match spawn_gate::bind_udp_std((ip, 0)) {
        Ok(_) => true,
        Err(error) => error.kind() != io::ErrorKind::AddrNotAvailable,
    };
    match address.to_canonical() {
        IpAddr::V6(ip) => held(IpAddr::V6(ip)) || ip.to_ipv4().is_some_and(|v4| held(v4.into())),
        v4 => held(v4),
    }
}

fn own_address_reason(_values: &[Option<String>; 2]) -> &'static str {
    OWN_ADDRESS
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
