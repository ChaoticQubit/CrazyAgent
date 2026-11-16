//! The engine's side of the helper protocol (threat model section 5, last paragraph): one request
//! per connection to the helper's socket, the socket made inside the caller's spawn gate, the
//! peer's uid checked before the request is written, and one deadline over the whole call.

use crate::ffi;
use crate::protocol::{self, REPLY_BYTES_MAX, Reply, Request};
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

const CHUNK_BYTES: usize = 256;

/// Where the helper listens, the uid it must run as, and how long one call may take in all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint<'a> {
    pub socket: &'a str,
    pub peer_uid: u32,
    pub wait: Duration,
}

/// The installed helper: launchd's socket under `/private/var/run`, root, and 12 s for a call
/// whose helper may run `pmset -g` and a `pmset` write, 5 s each at most.
pub const PRODUCTION: Endpoint<'static> = Endpoint {
    socket: "/private/var/run/dev.crazyagent.power.sock",
    peer_uid: 0,
    wait: Duration::from_secs(12),
};

/// Why a call got no reply: the connect failed, the peer runs as another uid, the write or
/// read failed or ran out of time, or the reply line was not one bounded reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallError {
    Connect(io::ErrorKind),
    PeerUid(u32),
    Exchange(io::ErrorKind),
    Reply,
}

/// The caller's spawn gate: runs the connect it is handed while no child of the caller starts,
/// since macOS cannot create a socket close-on-exec in one step.
pub type Gate<'g> =
    &'g dyn Fn(&mut dyn FnMut() -> io::Result<UnixStream>) -> io::Result<UnixStream>;

/// Sends one request and reads its reply. The socket is made and connected only through the
/// gate, a peer whose uid is not the endpoint's gets nothing, and the connect, the write and
/// the read share one deadline. Inputs: the endpoint, the request and the gate. Output: the
/// reply, or why there was none.
pub fn call(
    endpoint: &Endpoint<'_>,
    request: &Request,
    gate: Gate<'_>,
) -> Result<Reply, CallError> {
    assert!(endpoint.socket.starts_with('/'));
    assert!(!endpoint.wait.is_zero());
    let until = Instant::now() + endpoint.wait;
    let mut connect = || UnixStream::connect(endpoint.socket);
    let stream = gate(&mut connect).map_err(|error| CallError::Connect(error.kind()))?;
    let uid = ffi::peer_uid(stream.as_fd()).map_err(exchange)?;
    if uid != endpoint.peer_uid {
        return Err(CallError::PeerUid(uid));
    }
    let line = protocol::encode_request(request);
    stream.set_write_timeout(Some(left(until)?)).map_err(exchange)?;
    (&stream).write_all(&line).map_err(exchange)?;
    let answer = read_line(&stream, until)?;
    assert!(answer.len() <= REPLY_BYTES_MAX);
    protocol::decode_reply(&answer).ok_or(CallError::Reply)
}

fn exchange(error: io::Error) -> CallError {
    CallError::Exchange(error.kind())
}

/// The time left before the deadline. Input: the deadline. Output: the time, or `TimedOut`
/// once it is spent.
fn left(until: Instant) -> Result<Duration, CallError> {
    let left = until.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(CallError::Exchange(io::ErrorKind::TimedOut));
    }
    Ok(left)
}

/// Reads the reply up to its newline, at most `REPLY_BYTES_MAX` bytes, each read waiting only
/// what is left of the deadline. Every pass reads at least one byte or ends, so the loop runs
/// at most `REPLY_BYTES_MAX` times plus interrupted reads, which the deadline bounds. Inputs:
/// the stream and the deadline. Output: the bytes, or `Reply` for a line too long or cut off.
fn read_line(stream: &UnixStream, until: Instant) -> Result<Vec<u8>, CallError> {
    let mut line = Vec::with_capacity(REPLY_BYTES_MAX);
    let mut chunk = [0_u8; CHUNK_BYTES];
    while line.last() != Some(&b'\n') {
        let room = REPLY_BYTES_MAX - line.len();
        if room == 0 {
            return Err(CallError::Reply);
        }
        stream.set_read_timeout(Some(left(until)?)).map_err(exchange)?;
        let read = match (&*stream).read(&mut chunk[..room.min(CHUNK_BYTES)]) {
            Ok(0) => return Err(CallError::Reply),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(exchange(error)),
        };
        line.extend_from_slice(&chunk[..read]);
    }
    assert!(line.len() <= REPLY_BYTES_MAX);
    Ok(line)
}
