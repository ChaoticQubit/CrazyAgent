//! The helper's socket loop (threat model sections 4, 5 and 9): one connection at a time, the
//! caller checked at accept and again before acting, leases expired each second, readings
//! every 30 s, SIGTERM puts everything back, and an idle helper exits after 60 s.

use crate::caller::{AuditToken, Policy, Refused};
use crate::helper::{Config, Helper};
use crate::lease::Holder;
use crate::protocol::Reply;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

pub const SOCKET: &str = "/var/run/dev.crazyagent.power.sock";
pub const LISTENERS: &str = "Listeners";
pub const READ_WAIT: Duration = Duration::from_secs(2);
pub const READINGS_EVERY_MS: u64 = 30_000;
pub const IDLE_EXIT_MS: u64 = 60_000;
pub const WAKE_MS: u64 = 1_000;

/// A connection whose caller passed the first check.
#[derive(Debug)]
pub struct Accepted {
    stream: UnixStream,
    token: AuditToken,
    holder: Holder,
}

impl Accepted {
    /// Takes a connection and checks its caller. Output: the accepted connection, or why the
    /// caller was refused (the connection then closes with no reply).
    pub fn take(stream: UnixStream, policy: &Policy) -> Result<Self, Refused> {
        let _ = (stream, policy);
        Err(Refused::NoToken)
    }

    /// Reads the request, checks the caller again, lets the helper answer and writes the reply.
    /// Output: the reply sent, or why the caller was refused at the second check.
    pub fn serve(
        self,
        policy: &Policy,
        helper: &mut Helper,
        now_ms: u64,
    ) -> Result<Reply, Refused> {
        let _ = (policy, helper, now_ms, self.stream, self.token, self.holder);
        Err(Refused::NoToken)
    }
}

/// Serves `listener` until SIGTERM or idle. Inputs: the listener, the configuration and the
/// caller policy. Output: the exit code.
pub fn run(listener: UnixListener, config: Config, policy: Policy) -> i32 {
    let _ = (listener, config, policy);
    1
}

/// The installed helper: refuses to run as anyone but root, takes launchd's socket and serves
/// with the production configuration and policy. Output: the exit code.
pub fn main_production() -> u8 {
    1
}
