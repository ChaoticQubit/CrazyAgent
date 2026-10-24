use crate::credentials::SecretString;
use crate::engine::EngineError;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use subtle::ConstantTimeEq;

pub const TOKEN_PREFIX: &str = "crazyagent-relay-";
pub const LOCAL_PLACEHOLDER: &str = "crazyagent-local";
const TOKEN_HEX: usize = 64;
const CURL: &str = "/usr/bin/curl";
const CONNECTIONS_MAX: usize = 16;
const LINE_BYTES_MAX: u64 = 16 * 1024;
const HEADER_BYTES_MAX: usize = 64 * 1024;
const HEADERS_MAX: usize = 100;
const BODY_BYTES_MAX: usize = 64 << 20;
const RESPONSE_BYTES_MAX: u64 = 512 << 20;
const TARGET_BYTES_MAX: usize = 8 * 1024;
const CLIENT_TIME: Duration = Duration::from_secs(60);
const UPSTREAM_CONNECT_SECS: &str = "15";
const UPSTREAM_TIME_SECS: &str = "1800";
const POLL: Duration = Duration::from_millis(50);
const REQUEST_DROPPED: [&str; 14] = [
    "host",
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authorization",
    "transfer-encoding",
    "content-length",
    "expect",
    "te",
    "trailer",
    "upgrade",
    "accept-encoding",
    "x-api-key",
    "authorization",
];
const RESPONSE_DROPPED: [&str; 6] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "trailer",
    "upgrade",
];

/// Tells whether a model key value is a CrazyAgent placeholder rather than a provider key:
/// the local placeholder for keyless endpoints, or a relay token
/// `crazyagent-relay-<64 lowercase hex>`. Input: the value. Output: true for a placeholder.
pub fn is_placeholder(value: &str) -> bool {
    value == LOCAL_PLACEHOLDER
        || value.strip_prefix(TOKEN_PREFIX).is_some_and(|hex| {
            hex.len() == TOKEN_HEX
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

fn random_hex(bytes: usize) -> Result<String, EngineError> {
    let mut buffer = vec![0_u8; bytes];
    getrandom::fill(&mut buffer).map_err(|_| failure("model_relay_unavailable"))?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Where a relay forwards: the provider origin, the base path the agent is confined to
/// (empty for the whole origin) and the only scheme curl may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Upstream {
    origin: String,
    path: String,
    scheme: &'static str,
}

/// Checks a provider base URL a relay may forward to: https anywhere, or plain http only
/// to a loopback server, with no user, password, query or fragment.
/// Input: the base URL. Output: the upstream, or `model_relay_upstream_invalid`.
pub(crate) fn upstream(base_url: &str) -> Result<Upstream, EngineError> {
    let invalid = || failure("model_relay_upstream_invalid");
    let url = url::Url::parse(base_url).map_err(|_| invalid())?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => return Err(invalid()),
    };
    let scheme = match url.scheme() {
        "https" => "https",
        "http" if loopback => "http",
        _ => return Err(invalid()),
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    let path = url.path().trim_end_matches('/').to_owned();
    if path.len() > TARGET_BYTES_MAX / 2 || !path.bytes().all(target_byte) {
        return Err(invalid());
    }
    Ok(Upstream {
        origin: url.origin().ascii_serialization(),
        path,
        scheme,
    })
}

fn target_byte(byte: u8) -> bool {
    (0x21..=0x7e).contains(&byte) && !matches!(byte, b'"' | b'\\' | b'#')
}

struct Shared {
    upstream: Upstream,
    key: SecretString,
    token: String,
    scratch: PathBuf,
    active: AtomicUsize,
    stop: AtomicBool,
}

/// An engine-owned model key relay on 127.0.0.1 for one agent launch. The agent's harness
/// gets the relay's address and a random placeholder token instead of the provider key;
/// the relay checks the token, swaps in the real key from engine memory and forwards the
/// request to the provider through the system curl, streaming the reply back. The key
/// never reaches the agent's environment, arguments or files. Dropping the relay stops it.
pub struct Relay {
    port: u16,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
}

impl Relay {
    /// Starts a relay for one provider base URL and key.
    /// Inputs: the base URL (https, or http to loopback) and the key, kept only in memory.
    /// Output: the running relay, or `model_relay_upstream_invalid`, `model_relay_unavailable`.
    pub fn start(base_url: &str, key: SecretString) -> Result<Self, EngineError> {
        let upstream = upstream(base_url)?;
        let token = format!("{TOKEN_PREFIX}{}", random_hex(TOKEN_HEX / 2)?);
        assert!(is_placeholder(&token));
        let scratch = std::env::temp_dir().join(format!("crazyagent-relay-{}", random_hex(8)?));
        private_scratch(&scratch).map_err(|_| failure("model_relay_unavailable"))?;
        let bound = crate::spawn_gate::bind_tcp_std(("127.0.0.1", 0)).and_then(|listener| {
            listener.set_nonblocking(true)?;
            let port = listener.local_addr()?.port();
            Ok((listener, port))
        });
        let Ok((listener, port)) = bound else {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(failure("model_relay_unavailable"));
        };
        let shared = Arc::new(Shared {
            upstream,
            key,
            token,
            scratch,
            active: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        });
        let accepting = Arc::clone(&shared);
        let accept = std::thread::Builder::new()
            .name("model-relay".into())
            .spawn(move || accept_loop(&listener, &accepting))
            .map_err(|_| {
                let _ = std::fs::remove_dir_all(&shared.scratch);
                failure("model_relay_unavailable")
            })?;
        assert_ne!(port, 0);
        Ok(Self {
            port,
            shared,
            accept: Some(accept),
        })
    }

    /// The loopback port the relay listens on, for the agent's sandbox network rule.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The placeholder the harness sends as its key: `crazyagent-relay-<64 hex>`.
    pub fn token(&self) -> &str {
        &self.shared.token
    }

    /// The base URL the harness uses instead of the provider's:
    /// `http://127.0.0.1:<port>` followed by the provider's base path.
    pub fn base_url(&self) -> String {
        format!(
            "http://127.0.0.1:{}{}",
            self.port, self.shared.upstream.path
        )
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        let _ = std::fs::remove_dir_all(&self.shared.scratch);
    }
}

struct Active<'a>(&'a AtomicUsize);

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>) {
    while !shared.stop.load(Ordering::Acquire) {
        let (stream, peer) = match crate::spawn_gate::accept_tcp_std(listener) {
            Ok(accepted) => accepted,
            Err(_) => {
                std::thread::sleep(POLL);
                continue;
            }
        };
        if !peer.ip().is_loopback() || stream.set_nonblocking(false).is_err() {
            continue;
        }
        let _ = stream.set_read_timeout(Some(CLIENT_TIME));
        let _ = stream.set_write_timeout(Some(CLIENT_TIME));
        if shared.active.fetch_add(1, Ordering::AcqRel) >= CONNECTIONS_MAX {
            shared.active.fetch_sub(1, Ordering::AcqRel);
            let mut stream = stream;
            let _ = refuse(&mut stream, 503);
            continue;
        }
        let serving = Arc::clone(shared);
        let spawned = std::thread::Builder::new()
            .name("model-relay-request".into())
            .spawn(move || {
                let _active = Active(&serving.active);
                serve(stream, &serving);
            });
        if spawned.is_err() {
            shared.active.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// One request the harness sent: method, origin-form target, headers in order with
/// lowercase names, and the whole body.
#[derive(Debug)]
pub(crate) struct Incoming {
    pub(crate) method: String,
    pub(crate) target: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

/// Which header carried the placeholder, so the real key goes back in the same form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyHeader {
    XApiKey,
    Bearer,
}

fn serve(mut stream: TcpStream, shared: &Shared) {
    let outcome = match stream.try_clone() {
        Ok(reading) => exchange(&mut BufReader::new(reading), &mut stream, shared),
        Err(_) => Err(500),
    };
    if let Err(status) = outcome {
        let _ = refuse(&mut stream, status);
    }
    let _ = stream.shutdown(Shutdown::Both);
}

fn refuse(stream: &mut TcpStream, status: u16) -> std::io::Result<()> {
    let body = format!("{{\"error\":\"model_relay_{status}\"}}");
    stream.write_all(
        format!(
            "HTTP/1.1 {status} Relay\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
}

fn exchange(reader: &mut impl BufRead, stream: &mut TcpStream, shared: &Shared) -> Result<(), u16> {
    let incoming = read_request(reader)?;
    let scheme = authorized(&incoming, &shared.token).ok_or(401_u16)?;
    confined(&incoming.target, &shared.upstream)?;
    let body_file = if incoming.body.is_empty() {
        None
    } else {
        Some(BodyFile::write(&shared.scratch, &incoming.body).map_err(|_| 500_u16)?)
    };
    let mut config = upstream_config(
        &shared.upstream,
        &incoming,
        scheme,
        &shared.key,
        body_file.as_ref().map(|file| file.0.as_path()),
    )
    .map_err(|_| 400_u16)?;
    let mut child =
        crate::spawn_gate::spawn(&mut upstream_command(&shared.upstream)).map_err(|_| 502_u16)?;
    let written = child.stdin.take().map(|mut input| input.write_all(&config));
    config.fill(0);
    if !matches!(written, Some(Ok(()))) {
        stop(&mut child);
        return Err(502);
    }
    let Some(output) = child.stdout.take() else {
        stop(&mut child);
        return Err(502);
    };
    let relayed = relay_reply(&mut BufReader::new(output), stream);
    stop(&mut child);
    drop(body_file);
    relayed
}

fn stop(child: &mut Child) {
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
    }
    let _ = child.wait();
}

struct BodyFile(PathBuf);

impl BodyFile {
    fn write(scratch: &Path, body: &[u8]) -> std::io::Result<Self> {
        let name = random_hex(16).map_err(|_| std::io::Error::other("random"))?;
        let path = scratch.join(format!("request-{name}"));
        let mut file = create_private(&path)?;
        let file_path = Self(path);
        file.write_all(body)?;
        Ok(file_path)
    }
}

/// Creates the relay's scratch folder for request bodies, private to the engine user.
/// Input: the new folder path. Output: Ok once it exists with mode 0700.
#[cfg(unix)]
fn private_scratch(path: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

/// Refuses the relay's scratch folder where it cannot be made private to the engine
/// user. Input: the folder path, unused. Output: an `Unsupported` error, which the
/// relay reports as `model_relay_unavailable`.
#[cfg(not(unix))]
fn private_scratch(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "model_relay_unavailable",
    ))
}

/// Creates one request body file, readable only by the engine user, refusing a link
/// placed at its path. Input: the new file path. Output: the open file.
#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

/// Refuses a request body file where it cannot be made private to the engine user.
/// Input: the file path, unused. Output: an `Unsupported` error.
#[cfg(not(unix))]
fn create_private(_path: &Path) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "model_relay_unavailable",
    ))
}

impl Drop for BodyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn read_line(reader: &mut impl BufRead, total: &mut usize) -> Result<String, u16> {
    let mut line = Vec::new();
    reader
        .take(LINE_BYTES_MAX)
        .read_until(b'\n', &mut line)
        .map_err(|_| 400_u16)?;
    *total += line.len();
    if !line.ends_with(b"\n") || *total > HEADER_BYTES_MAX {
        return Err(431);
    }
    line.pop();
    if line.ends_with(b"\r") {
        line.pop();
    }
    String::from_utf8(line).map_err(|_| 400_u16)
}

fn header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

/// Reads one HTTP/1.x request from the harness: GET or POST, an origin-form target, at
/// most 100 headers in 64 KiB and a body of at most 64 MiB by Content-Length or chunks.
/// Output: the request, or the HTTP status to refuse it with.
pub(crate) fn read_request(reader: &mut impl BufRead) -> Result<Incoming, u16> {
    let mut total = 0;
    let line = read_line(reader, &mut total)?;
    let mut words = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return Err(400);
    };
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return Err(505);
    }
    if !matches!(method, "GET" | "POST") {
        return Err(405);
    }
    if !target.starts_with('/')
        || target.len() > TARGET_BYTES_MAX
        || !target.bytes().all(target_byte)
    {
        return Err(400);
    }
    let mut headers = Vec::new();
    loop {
        let line = read_line(reader, &mut total)?;
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').ok_or(400_u16)?;
        let value = value.trim();
        if !header_name(name) || headers.len() >= HEADERS_MAX || value.chars().any(char::is_control)
        {
            return Err(400);
        }
        headers.push((name.to_ascii_lowercase(), value.to_owned()));
    }
    let find = |wanted: &str| {
        headers
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, value)| value.as_str())
    };
    let body =
        if find("transfer-encoding").is_some_and(|value| value.eq_ignore_ascii_case("chunked")) {
            read_chunked(reader)?
        } else if find("transfer-encoding").is_some() {
            return Err(501);
        } else {
            let length: usize = find("content-length")
                .map(|value| value.parse().map_err(|_| 400_u16))
                .transpose()?
                .unwrap_or(0);
            if length > BODY_BYTES_MAX {
                return Err(413);
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).map_err(|_| 400_u16)?;
            body
        };
    Ok(Incoming {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
        body,
    })
}

fn read_chunked(reader: &mut impl BufRead) -> Result<Vec<u8>, u16> {
    let mut body = Vec::new();
    loop {
        let line = read_line(reader, &mut 0)?;
        let size = line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size, 16).map_err(|_| 400_u16)?;
        if size == 0 {
            let mut trailers = 0;
            while !read_line(reader, &mut trailers)?.is_empty() {}
            return Ok(body);
        }
        assert!(body.len() <= BODY_BYTES_MAX);
        if size > BODY_BYTES_MAX - body.len() {
            return Err(413);
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..]).map_err(|_| 400_u16)?;
        if !read_line(reader, &mut 0)?.is_empty() {
            return Err(400);
        }
    }
}

/// Finds the placeholder the harness sent: `x-api-key: <token>` or
/// `authorization: Bearer <token>`, compared in constant time. When a harness sends both,
/// as Claude Code's key helper does, `x-api-key` wins, since provider keys go there.
/// Output: which header carried it, or none when the token is missing or wrong.
pub(crate) fn authorized(incoming: &Incoming, token: &str) -> Option<KeyHeader> {
    let matches = |value: &str| bool::from(value.as_bytes().ct_eq(token.as_bytes()));
    let presented = |wanted: &'static str| {
        incoming
            .headers
            .iter()
            .filter(move |(name, _)| name == wanted)
            .map(|(_, value)| value.as_str())
    };
    if presented("x-api-key").any(matches) {
        return Some(KeyHeader::XApiKey);
    }
    presented("authorization")
        .filter_map(|value| value.strip_prefix("Bearer "))
        .any(matches)
        .then_some(KeyHeader::Bearer)
}

/// Keeps the agent inside the provider base path: the target's path must be the base path
/// or below it, with no `..` segment. Output: Ok, or 404.
pub(crate) fn confined(target: &str, upstream: &Upstream) -> Result<(), u16> {
    let path = target.split('?').next().unwrap_or_default();
    let inside = upstream.path.is_empty()
        || path == upstream.path
        || path
            .strip_prefix(&upstream.path)
            .is_some_and(|rest| rest.starts_with('/'));
    if !inside
        || path
            .split('/')
            .any(|segment| segment == ".." || segment == ".")
    {
        return Err(404);
    }
    Ok(())
}

fn quoted(value: &str) -> Result<String, EngineError> {
    if value.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        return Err(failure("model_relay_request_invalid"));
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// Builds the curl config for one forwarded request, which goes only to curl's stdin: the
/// provider URL, method, the harness's headers minus hop-by-hop, encoding and key headers,
/// the real key in the header form the harness used, and the body file.
/// Output: the config bytes, or `model_relay_request_invalid`.
pub(crate) fn upstream_config(
    upstream: &Upstream,
    incoming: &Incoming,
    scheme: KeyHeader,
    key: &SecretString,
    body: Option<&Path>,
) -> Result<Vec<u8>, EngineError> {
    let mut config = format!(
        "url = {}\nrequest = {}\nheader = \"Expect:\"\n",
        quoted(&format!("{}{}", upstream.origin, incoming.target))?,
        quoted(&incoming.method)?
    );
    for (name, value) in &incoming.headers {
        if REQUEST_DROPPED.contains(&name.as_str()) {
            continue;
        }
        config.push_str(&format!(
            "header = {}\n",
            quoted(&format!("{name}: {value}"))?
        ));
    }
    let key_header = match scheme {
        KeyHeader::XApiKey => format!("x-api-key: {}", key.expose()),
        KeyHeader::Bearer => format!("Authorization: Bearer {}", key.expose()),
    };
    config.push_str(&format!("header = {}\n", quoted(&key_header)?));
    if let Some(body) = body {
        let body = body
            .to_str()
            .ok_or_else(|| failure("model_relay_request_invalid"))?;
        config.push_str(&format!("data-binary = {}\n", quoted(&format!("@{body}"))?));
    }
    Ok(config.into_bytes())
}

/// Builds the curl command for the provider without running it: no `.curlrc`, no
/// redirects, only the upstream's scheme, streaming output with the reply headers first,
/// and a cleared environment. Its arguments and environment hold no URL, header or key.
pub(crate) fn upstream_command(upstream: &Upstream) -> Command {
    let mut command = Command::new(CURL);
    command
        .arg("-q")
        .args(["--silent", "--no-buffer", "--include", "--globoff"])
        .args(["--proto", &format!("={}", upstream.scheme)])
        .args(["--max-redirs", "0"])
        .args(["--connect-timeout", UPSTREAM_CONNECT_SECS])
        .args(["--max-time", UPSTREAM_TIME_SECS])
        .args(["--config", "-"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

fn relay_reply(output: &mut impl BufRead, stream: &mut TcpStream) -> Result<(), u16> {
    let mut total = 0;
    let (status, headers) = loop {
        let line = read_line(output, &mut total).map_err(|_| 502_u16)?;
        let status: u16 = line
            .strip_prefix("HTTP/")
            .and_then(|rest| rest.split(' ').nth(1))
            .and_then(|code| code.parse().ok())
            .filter(|code| (100..=599).contains(code))
            .ok_or(502_u16)?;
        let mut headers = Vec::new();
        loop {
            let line = read_line(output, &mut total).map_err(|_| 502_u16)?;
            if line.is_empty() {
                break;
            }
            let (name, value) = line.split_once(':').ok_or(502_u16)?;
            if headers.len() < HEADERS_MAX
                && header_name(name)
                && !RESPONSE_DROPPED.contains(&name.to_ascii_lowercase().as_str())
            {
                headers.push(format!("{name}:{value}"));
            }
        }
        if status >= 200 {
            break (status, headers);
        }
    };
    let mut head = format!("HTTP/1.1 {status} Relay\r\n");
    for header in headers {
        head.push_str(&header);
        head.push_str("\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    if stream.write_all(head.as_bytes()).is_err() {
        return Ok(());
    }
    let mut buffer = [0_u8; 16 * 1024];
    let mut sent = 0_u64;
    let mut body = output.take(RESPONSE_BYTES_MAX);
    loop {
        let read = match body.read(&mut buffer) {
            Ok(0) | Err(_) => return Ok(()),
            Ok(read) => read,
        };
        sent += read as u64;
        if stream.write_all(&buffer[..read]).is_err() || stream.flush().is_err() {
            return Ok(());
        }
        assert!(sent <= RESPONSE_BYTES_MAX);
    }
}

#[cfg(all(test, unix))]
mod tests;
