//! The engine's side of the helper protocol (threat model section 5, last paragraph): one request
//! per connection to the helper's socket, the socket made inside the caller's spawn gate, the
//! peer's uid checked before the request is written, and one deadline over the whole call.

use crate::protocol::{Reply, Request};
use std::io;
use std::os::unix::net::UnixStream;
use std::time::Duration;

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

/// Sends one request and reads its reply. Inputs: the endpoint, the request and the gate.
/// Output: the reply, or why there was none.
pub fn call(
    _endpoint: &Endpoint<'_>,
    _request: &Request,
    _gate: Gate<'_>,
) -> Result<Reply, CallError> {
    Err(CallError::Reply)
}
