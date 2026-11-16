//! The loopback address the owner's browser comes back to after a connector sign-in: one
//! listener on `127.0.0.1` at a port of its own, which takes exactly one answer carrying the
//! sign-in's `state`, refuses every other request without ending the wait, and closes after
//! `CALLBACK_WAIT`.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The path the browser comes back to.
pub const CALLBACK_PATH: &str = "/callback";
/// How long a sign-in waits for the browser.
pub const CALLBACK_WAIT: Duration = Duration::from_secs(5 * 60);
/// How many refused requests close a callback before its answer came; a connection that
/// sends nothing counts as one.
pub const REFUSED_MAX: usize = 32;
/// The page the browser shows once the answer came.
pub const DONE_PAGE: &str = "You can close this tab.";
/// The page the browser shows when the answer says the sign-in was refused.
pub const FAILED_PAGE: &str = "Sign-in did not finish. You can close this tab.";

const REFUSED_PAGE: &str = "Bad request.";
const HEAD_BYTES_MAX: usize = 8 * 1024;
const READ_WAIT: Duration = Duration::from_secs(5);
const ACCEPT_POLL: Duration = Duration::from_millis(20);
const STOP_POLL: Duration = Duration::from_millis(100);

/// How a wait for the browser ended. Its debug text never holds the code.
#[derive(Clone, PartialEq, Eq)]
pub enum Waited {
    /// The answer carried the sign-in's state and a code, with the issuer when sent.
    Code { code: String, iss: Option<String> },
    /// The answer carried the sign-in's state and an `error`.
    Refused,
    /// No answer came within the wait.
    Expired,
    /// `REFUSED_MAX` requests were refused before the answer came.
    Flooded,
    /// The owner cancelled the sign-in.
    Stopped,
}

impl std::fmt::Debug for Waited {
    /// Names how the wait ended and the issuer, never the code, which is a one-time secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code { iss, .. } => f
                .debug_struct("Code")
                .field("code", &"<redacted>")
                .field("iss", iss)
                .finish(),
            Self::Refused => f.write_str("Refused"),
            Self::Expired => f.write_str("Expired"),
            Self::Flooded => f.write_str("Flooded"),
            Self::Stopped => f.write_str("Stopped"),
        }
    }
}

/// One sign-in's loopback listener and the address the browser is sent back to.
pub struct Callback {
    listener: TcpListener,
    redirect: url::Url,
}

impl Callback {
    /// Binds a listener of its own on `127.0.0.1` at a port the system picks, so no other
    /// socket shares it. Output: the callback, or why it could not bind.
    pub fn open() -> io::Result<Self> {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let listener = crate::spawn_gate::bind_tcp_std_exclusive(address)?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        assert!(port != 0, "the system picked a port");
        let redirect = url::Url::parse(&format!("http://127.0.0.1:{port}{CALLBACK_PATH}"))
            .map_err(io::Error::other)?;
        Ok(Self { listener, redirect })
    }

    /// The address the browser is sent back to: `http://127.0.0.1:<port>/callback`.
    pub fn redirect(&self) -> &url::Url {
        &self.redirect
    }

    /// Waits for the browser's answer carrying `state`, at most `limit`, until `stop` is set;
    /// the port closes when it returns. Inputs: the sign-in's state, the limit and the stop
    /// flag. Output: how the wait ended.
    pub fn wait(self, state: &str, limit: Duration, stop: &AtomicBool) -> Waited {
        assert!(!state.is_empty(), "a sign-in has a state");
        assert!(limit <= CALLBACK_WAIT, "{limit:?}");
        let deadline = Instant::now() + limit;
        let mut refused = 0;
        loop {
            if stop.load(Ordering::SeqCst) {
                return Waited::Stopped;
            }
            let now = Instant::now();
            if now >= deadline {
                return Waited::Expired;
            }
            let Ok((stream, _)) = self.listener.accept() else {
                std::thread::sleep(ACCEPT_POLL);
                continue;
            };
            if let Some(waited) = answer(stream, state, deadline - now, stop) {
                return waited;
            }
            refused += 1;
            assert!(refused <= REFUSED_MAX);
            if refused == REFUSED_MAX {
                return Waited::Flooded;
            }
        }
    }
}

/// Reads one request and answers it: a page for the one that ends the wait, a plain 400 for
/// any other. The connection has one budget, at most `READ_WAIT` and never past the time
/// left, for its head and the drain after its answer. A connection that sends nothing, and
/// one cut short by `stop`, closes with no answer, so a browser's spare connection never
/// reads a refusal. Inputs: the connection, the sign-in's state, the time left and the stop
/// flag. Output: how the wait ended, or none for a refused request.
fn answer(mut stream: TcpStream, state: &str, left: Duration, stop: &AtomicBool) -> Option<Waited> {
    let until = Instant::now() + left.min(READ_WAIT);
    let ready = stream.set_nonblocking(false).is_ok()
        && stream.set_write_timeout(Some(STOP_POLL)).is_ok();
    if !ready {
        return None;
    }
    let mut bytes = Vec::with_capacity(1024);
    let read = head(&mut stream, &mut bytes, until, stop);
    let waited = read.and_then(|head| ended(&head, state));
    if bytes.is_empty() || (waited.is_none() && stop.load(Ordering::SeqCst)) {
        assert!(!bytes.is_empty() || waited.is_none());
        return None;
    }
    let page = match &waited {
        None => None,
        Some(Waited::Code { .. }) => Some(DONE_PAGE),
        Some(_) => Some(FAILED_PAGE),
    };
    reply(&mut stream, page, until, stop);
    waited
}

/// Reads once from a connection within its budget, waiting at most `STOP_POLL` at a time so
/// a set `stop` is seen that soon. Inputs: the connection, the buffer, the budget's end and
/// the stop flag. Output: the bytes read, 0 at the connection's end, or none once the budget
/// ran out, `stop` was set or the read failed.
fn read_by(
    stream: &mut TcpStream,
    chunk: &mut [u8],
    until: Instant,
    stop: &AtomicBool,
) -> Option<usize> {
    loop {
        let now = Instant::now();
        if stop.load(Ordering::SeqCst) || now >= until {
            return None;
        }
        let slice = (until - now).min(STOP_POLL).max(Duration::from_millis(1));
        stream.set_read_timeout(Some(slice)).ok()?;
        match stream.read(chunk) {
            Ok(read) => return Some(read),
            Err(error) if again(error.kind()) => continue,
            Err(_) => return None,
        }
    }
}

/// Whether a read that failed with `kind` only timed out or was interrupted, so it may be
/// tried again within its budget.
fn again(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

/// Reads a request head up to its blank line into `bytes`, within the connection's budget.
/// Inputs: the connection, an empty buffer, the budget's end and the stop flag. Output: the
/// head, or none when it passes `HEAD_BYTES_MAX`, is not UTF-8, the connection ends, the
/// budget runs out or `stop` is set.
fn head(
    stream: &mut TcpStream,
    bytes: &mut Vec<u8>,
    until: Instant,
    stop: &AtomicBool,
) -> Option<String> {
    assert!(bytes.is_empty());
    let mut chunk = [0u8; 1024];
    while !bytes.windows(4).any(|four| four == b"\r\n\r\n") {
        if bytes.len() >= HEAD_BYTES_MAX {
            return None;
        }
        let read = read_by(stream, &mut chunk, until, stop)?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    if bytes.len() > HEAD_BYTES_MAX {
        return None;
    }
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

/// How a request head ends the wait: only a GET whose target is visible ASCII, exactly
/// `CALLBACK_PATH` and a query naming each field once and carrying the sign-in's state, then
/// an `error`, or a code and the issuer when sent. Inputs: the head and the state. Output:
/// how the wait ended, or none.
fn ended(head: &str, state: &str) -> Option<Waited> {
    let line = head.split("\r\n").next()?;
    let mut parts = line.split(' ');
    let (Some("GET"), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if !version.starts_with("HTTP/1.") || !target.bytes().all(|byte| byte.is_ascii_graphic()) {
        return None;
    }
    let (path, query) = target.split_once('?')?;
    if path != CALLBACK_PATH || query.contains('#') {
        return None;
    }
    let text = format!("http://127.0.0.1{CALLBACK_PATH}?{query}");
    let address = url::Url::parse(&text).ok()?;
    if address.path() != CALLBACK_PATH {
        return None;
    }
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in address.query_pairs() {
        let old = fields.insert(key.into_owned(), value.into_owned());
        if old.is_some() {
            return None;
        }
    }
    if !fields.get("state").is_some_and(|sent| same(sent, state)) {
        return None;
    }
    if fields.contains_key("error") {
        return Some(Waited::Refused);
    }
    let code = fields.remove("code").filter(|code| !code.is_empty())?;
    let iss = fields.remove("iss");
    Some(Waited::Code { code, iss })
}

/// Whether two texts are equal, read in a time that depends only on their lengths.
fn same(sent: &str, state: &str) -> bool {
    let differ = sent
        .bytes()
        .zip(state.bytes())
        .fold(0u8, |differ, (one, other)| differ | (one ^ other));
    sent.len() == state.len() && differ == 0
}

/// Answers one request with a plain page, closes its sending side and reads what is left,
/// at most `HEAD_BYTES_MAX` bytes within the connection's budget, so the browser reads the
/// page before the connection goes. Inputs: the connection, the page, none for a refused
/// request, the budget's end and the stop flag.
fn reply(stream: &mut TcpStream, page: Option<&str>, until: Instant, stop: &AtomicBool) {
    let (status, body) = match page {
        Some(page) => ("200 OK", page),
        None => ("400 Bad Request", REFUSED_PAGE),
    };
    let length = body.len();
    let text = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {length}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\r\n{body}"
    );
    let _ = stream.write_all(text.as_bytes());
    let _ = stream.shutdown(Shutdown::Write);
    let mut chunk = [0u8; 1024];
    let mut left = HEAD_BYTES_MAX;
    while left > 0 {
        match read_by(stream, &mut chunk, until, stop) {
            Some(0) | None => break,
            Some(read) => left = left.saturating_sub(read),
        }
    }
}

#[cfg(test)]
#[path = "callback_tests.rs"]
mod tests;
