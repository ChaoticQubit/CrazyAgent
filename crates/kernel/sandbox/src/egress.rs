use kernel_security::error::StoreError;
use kernel_security::net::{Egress, RawReply, Sealed};
use std::io::{self, Read, Write};
use std::net::{IpAddr, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The most body bytes one exchange reads, and the size curl may download.
pub const BODY_BYTES_MAX: usize = 4 << 20;
const SCOPES_LINE_MAX: usize = 1024;
const TRAILER_BYTES_MAX: usize = SCOPES_LINE_MAX + 16;
const REQUEST_TIME: Duration = Duration::from_secs(30);

/// The longest one host lookup may take before its request is refused.
pub const LOOKUP_TIME: Duration = Duration::from_secs(5);

/// The most lookup threads alive at once, answered or not; one more is refused, so stalled
/// lookups never pile up threads.
pub const LOOKUPS_MAX: usize = 8;

/// The lookup threads alive now, counted from before the spawn until each thread ends.
static LOOKUPS: AtomicUsize = AtomicUsize::new(0);

/// curl's `--max-time`, 25 s: what is left of the 30 s request time after the lookup's 5 s
/// bound, so the lookup and curl together never pass 30 s.
pub const CURL_TIME: Duration = Duration::from_secs(REQUEST_TIME.as_secs() - LOOKUP_TIME.as_secs());

/// The one reply header the curl exchange reads back.
const SCOPES_HEADER: &str = "x-oauth-scopes";

/// What came back: the HTTP status, at most 4 MiB of body, and the scopes a classic
/// GitHub token holds when GitHub named them in `X-OAuth-Scopes`.
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub scopes: Option<Vec<String>>,
}

/// Why an exchange gave no reply: curl could not finish it, or its output was not a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExchangeError {
    Unreachable,
    ReplyInvalid,
}

/// Runs a built curl command, writes its config to curl's stdin, wipes the config and
/// reads the reply within [`CURL_TIME`] plus 5 s for curl to end. Inputs: the command and its config. Output:
/// the reply, or [`ExchangeError::Unreachable`].
pub fn exchange(mut command: Command, mut config: Vec<u8>) -> Result<Reply, ExchangeError> {
    let spawned = kernel_processes::spawn_gate::spawn(&mut command);
    let mut child = match spawned {
        Ok(child) => child,
        Err(_) => {
            kernel_security::secret::wipe(&mut config);
            return Err(ExchangeError::Unreachable);
        }
    };
    let mut stdin = child.stdin.take().expect("curl stdin is piped");
    let written = stdin.write_all(&config);
    kernel_security::secret::wipe(&mut config);
    drop(stdin);
    let stdout = child.stdout.take().expect("curl stdout is piped");
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout
            .take((BODY_BYTES_MAX + TRAILER_BYTES_MAX) as u64)
            .read_to_end(&mut bytes);
        (read, bytes)
    });
    let deadline = Instant::now() + CURL_TIME + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|_| ExchangeError::Unreachable)? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(ExchangeError::Unreachable);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let (read, bytes) = reader.join().map_err(|_| ExchangeError::Unreachable)?;
    if written.is_err() || read.is_err() || !status.success() {
        return Err(ExchangeError::Unreachable);
    }
    split(bytes)
}

/// Splits curl's output into the body, the `X-OAuth-Scopes` line and the status line
/// that `WRITE_OUT` puts after it. Input: the output. Output: the reply, or
/// [`ExchangeError::ReplyInvalid`] for a body past 4 MiB or a missing or bad status.
fn split(mut bytes: Vec<u8>) -> Result<Reply, ExchangeError> {
    let invalid = || ExchangeError::ReplyInvalid;
    if bytes.len() > BODY_BYTES_MAX + TRAILER_BYTES_MAX {
        return Err(invalid());
    }
    let cut = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .ok_or_else(invalid)?;
    let code = std::str::from_utf8(&bytes[cut + 1..]).map_err(|_| invalid())?;
    let status: u16 = code.parse().map_err(|_| invalid())?;
    if !(100..=599).contains(&status) {
        return Err(invalid());
    }
    bytes.truncate(cut);
    let line = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .ok_or_else(invalid)?;
    let scopes = scopes(&bytes[line + 1..]);
    bytes.truncate(line);
    if bytes.len() > BODY_BYTES_MAX {
        return Err(invalid());
    }
    Ok(Reply {
        status,
        body: bytes,
        scopes,
    })
}

/// Reads the `X-OAuth-Scopes` line: the comma-separated scopes of a classic token.
/// Input: the line curl wrote. Output: the scopes, or None for an empty line (a token
/// kind GitHub names no scopes for), a curl too old to print headers, which writes the
/// variable as it is, or any line that is not a list of scope names.
fn scopes(line: &[u8]) -> Option<Vec<String>> {
    if line.is_empty() || line.len() > SCOPES_LINE_MAX {
        return None;
    }
    let scopes: Vec<String> = std::str::from_utf8(line)
        .ok()?
        .split(',')
        .map(str::trim)
        .map(str::to_owned)
        .collect();
    let named = scopes.iter().all(|scope| {
        !scope.is_empty()
            && scope
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b':' || byte == b'_')
    });
    named.then_some(scopes)
}

/// Egress through the system curl and [`exchange`], with a static per-plugin host
/// allowlist.
pub struct Curl {
    hosts: &'static [(&'static str, &'static [&'static str])],
}

impl Curl {
    /// Egress over one host table. Input: (plugin, hosts) rows. Output: the egress.
    pub fn with_hosts(hosts: &'static [(&'static str, &'static [&'static str])]) -> Self {
        Self { hosts }
    }
}

/// The key a `[net].hosts` entry names an origin by: the host, with `:port` only for a
/// port other than the scheme's default. Inputs: the scheme, host and port. Output: the key.
fn host_key(scheme: &str, host: &str, port: u16) -> String {
    let default = match scheme {
        "https" => 443,
        "http" => 80,
        _ => 0,
    };
    if port == default {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    }
}

impl Egress for Curl {
    fn allows(
        &self,
        caller: &'static str,
        scheme: &str,
        host: &str,
        port: u16,
    ) -> Result<(), StoreError> {
        let key = host_key(scheme, host, port);
        let listed = self
            .hosts
            .iter()
            .any(|(plugin, hosts)| *plugin == caller && hosts.contains(&key.as_str()));
        if listed {
            Ok(())
        } else {
            Err(StoreError::code("net_host_refused"))
        }
    }

    fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, StoreError> {
        resolve_with(host, port, system_lookup)
    }

    fn send(&self, sealed: Sealed) -> Result<RawReply, StoreError> {
        if sealed
            .reply_headers()
            .iter()
            .any(|name| !name.eq_ignore_ascii_case(SCOPES_HEADER))
        {
            return Err(StoreError::code("net_reply_header_unsupported"));
        }
        let loopback = matches!(sealed.host(), "127.0.0.1" | "[::1]" | "localhost");
        let reply = exchange(curl(loopback), sealed.config().to_vec())
            .map_err(|error| StoreError::code(net_code(error)))?;
        drop(sealed);
        let headers = match reply.scopes {
            Some(scopes) => vec![(SCOPES_HEADER.to_owned(), scopes.join(", "))],
            None => Vec::new(),
        };
        Ok(RawReply {
            status: reply.status,
            headers,
            body: reply.body,
        })
    }
}

/// Resolves a plugin request's host through one lookup bounded by [`lookup_within`].
/// Inputs: the host, port and lookup. Output: every address, or `net_host_unresolved` for a
/// failed lookup or one past [`LOOKUP_TIME`].
fn resolve_with(
    host: &str,
    port: u16,
    lookup: impl FnOnce(&str, u16) -> io::Result<Vec<IpAddr>> + Send + 'static,
) -> Result<Vec<IpAddr>, StoreError> {
    lookup_within(host, port, lookup).map_err(|_| StoreError::code("net_host_unresolved"))
}

/// Runs one host lookup on a thread of its own and waits at most [`LOOKUP_TIME`] for it,
/// so a stalled resolver never holds a request. A lookup still running then keeps its
/// thread, counted, until the resolver returns; with [`LOOKUPS_MAX`] threads alive a new
/// lookup is refused at once, with no spawn. Inputs: the host, port and lookup. Output: the
/// answer, or a `TimedOut` failure past the bound or at the cap.
pub fn lookup_within(
    host: &str,
    port: u16,
    lookup: impl FnOnce(&str, u16) -> io::Result<Vec<IpAddr>> + Send + 'static,
) -> io::Result<Vec<IpAddr>> {
    assert!(!host.is_empty());
    if LOOKUPS.fetch_add(1, Ordering::SeqCst) >= LOOKUPS_MAX {
        LOOKUPS.fetch_sub(1, Ordering::SeqCst);
        return Err(io::Error::from(io::ErrorKind::TimedOut));
    }
    let alive = Alive;
    let host = host.to_owned();
    let (answer, answered) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("host-lookup".to_owned())
        .spawn(move || {
            let _alive = alive;
            let _ = answer.send(lookup(&host, port));
        })?;
    answered
        .recv_timeout(LOOKUP_TIME)
        .unwrap_or_else(|_| Err(io::Error::from(io::ErrorKind::TimedOut)))
}

/// One counted lookup thread: dropping it, when the thread ends, unwinds, or never spawns,
/// takes it off [`LOOKUPS`].
struct Alive;

impl Drop for Alive {
    fn drop(&mut self) {
        let before = LOOKUPS.fetch_sub(1, Ordering::SeqCst);
        assert!(before > 0 && before <= LOOKUPS_MAX);
    }
}

/// Looks a host name up once through the system resolver, with no bound of its own; callers
/// go through [`lookup_within`]. Inputs: the host and port. Output: every address it
/// resolved to, or the lookup failure.
pub fn system_lookup(host: &str, port: u16) -> io::Result<Vec<IpAddr>> {
    Ok((host, port)
        .to_socket_addrs()?
        .map(|found| found.ip())
        .collect())
}

/// The `net.request` code of an exchange that gave no reply. Input: the reason. Output:
/// `net_unreachable` or `net_reply_invalid`.
fn net_code(error: ExchangeError) -> &'static str {
    match error {
        ExchangeError::ReplyInvalid => "net_reply_invalid",
        ExchangeError::Unreachable => "net_unreachable",
    }
}

/// The curl command of one request, with today's GitHub exchange flags: no `.curlrc`, no
/// redirects, one allowed scheme, [`CURL_TIME`] and a bounded size, the config only on stdin and an
/// empty environment. Input: whether the host is loopback, which allows plain http only.
/// Output: the command, whose arguments and environment hold no URL, header or secret.
fn curl(loopback: bool) -> Command {
    let mut command = Command::new("/usr/bin/curl");
    command
        .arg("-q")
        .args(["--silent", "--show-error", "--globoff"])
        .args(["--proto", if loopback { "=http,https" } else { "=https" }])
        .args(["--max-redirs", "0"])
        .args(["--connect-timeout", "10"])
        .args(["--max-time", &CURL_TIME.as_secs().to_string()])
        .args(["--max-filesize", &BODY_BYTES_MAX.to_string()])
        .args(["--write-out", "\n%header{x-oauth-scopes}\n%{http_code}"])
        .args(["--config", "-"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

#[cfg(test)]
mod tests;
