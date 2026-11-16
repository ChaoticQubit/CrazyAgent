//! The helper's socket loop (threat model sections 4, 5 and 9): one connection at a time, the
//! caller checked at accept and again before acting, leases expired each second, readings
//! every 30 s, SIGTERM puts everything back, and an idle helper exits after 60 s.

use crate::caller::{self, AuditToken, PRODUCTION, Policy, Refused};
use crate::ffi;
use crate::floor::{Battery, Pressure};
use crate::helper::{Config, Helper, Readings};
use crate::lease::Holder;
use crate::lid;
use crate::pmset::Pmset;
use crate::protocol::{self, Reply};
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::thread;
use std::time::{Duration, Instant};

pub const SOCKET: &str = "/var/run/dev.crazyagent.power.sock";
pub const LISTENERS: &str = "Listeners";
pub const READ_WAIT: Duration = Duration::from_secs(2);
pub const READINGS_EVERY_MS: u64 = 30_000;
pub const IDLE_EXIT_MS: u64 = 60_000;
pub const WAKE_MS: u64 = 1_000;

const ERROR_PAUSE: Duration = Duration::from_millis(100);

/// A connection whose caller passed the first check, with the deadline its request must arrive
/// by: `READ_WAIT` from accept.
#[derive(Debug)]
pub struct Accepted {
    stream: UnixStream,
    token: AuditToken,
    holder: Holder,
    until: Instant,
}

impl Accepted {
    /// Takes a connection and checks its caller. Output: the accepted connection, or why the
    /// caller was refused (logged with the peer pid when the token was read; the connection
    /// then closes with no reply).
    pub fn take(stream: UnixStream, policy: &Policy) -> Result<Self, Refused> {
        let until = Instant::now() + READ_WAIT;
        let token = bounded(&stream)
            .map_err(|_| Refused::NoToken)
            .and_then(|()| caller::token_of(&stream))
            .inspect_err(|refused| ffi::log(&format!("caller refused: {refused:?}")))?;
        let holder = caller::check(&token, policy)
            .inspect_err(|refused| log_refused(&token, refused))?;
        Ok(Self {
            stream,
            token,
            holder,
            until,
        })
    }

    /// Reads the request by the deadline, checks the caller again, lets the helper answer and
    /// writes the reply. Output: the reply sent, or why the caller was refused at the second
    /// check.
    pub fn serve(
        self,
        policy: &Policy,
        helper: &mut Helper,
        now_ms: u64,
    ) -> Result<Reply, Refused> {
        let Self {
            mut stream,
            token,
            holder,
            until,
        } = self;
        let read = protocol::read_request(&mut Deadline {
            stream: &stream,
            until,
        });
        let reply = match read {
            Err(reason) => Reply::Refused { reason },
            Ok(request) => {
                caller::check(&token, policy)
                    .inspect_err(|refused| log_refused(&token, refused))?;
                helper.handle(holder, request, now_ms, lid::read())
            }
        };
        if let Err(error) = protocol::write_reply(&mut stream, &reply) {
            ffi::log(&format!("reply not written: {error}"));
        }
        Ok(reply)
    }
}

/// Logs a refused caller with the pid its audit token names and the reason, nothing from its
/// request (threat model section 5).
fn log_refused(token: &AuditToken, refused: &Refused) {
    ffi::log(&format!("caller pid {} refused: {refused:?}", token.pid()));
}

/// Reads an accepted request within one deadline from accept: each read waits at most what is
/// left of it, and none starts once it passed or SIGTERM arrived, so a caller that trickles
/// bytes is cut off `READ_WAIT` after accept (threat model section 4).
struct Deadline<'a> {
    stream: &'a UnixStream,
    until: Instant,
}

impl Read for Deadline<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() || ffi::term_requested() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        assert!(left <= READ_WAIT);
        self.stream.set_read_timeout(Some(left))?;
        let mut stream = self.stream;
        stream.read(buffer)
    }
}

/// Makes an accepted connection blocking with read and write timeouts. `Deadline` shortens the
/// read timeout to what is left of `READ_WAIT` from accept; the reply, at most 1024 bytes into
/// an empty send buffer, is one write bounded by the write timeout.
fn bounded(stream: &UnixStream) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(READ_WAIT))?;
    stream.set_write_timeout(Some(READ_WAIT))
}

/// Serves `listener` until SIGTERM or idle. Inputs: the listener, the configuration and the
/// caller policy. Output: the exit code.
pub fn run(listener: UnixListener, config: Config, policy: Policy) -> i32 {
    ffi::catch_term();
    if let Err(error) = listener.set_nonblocking(true) {
        ffi::log(&format!("listener not usable: {error}"));
        return 1;
    }
    let pmset = Pmset::new(&config.pmset);
    let mut helper = Helper::start(config);
    let mut readings_due_ms = 0;
    let mut active_ms = ffi::monotonic_ms();
    loop {
        if ffi::term_requested() {
            helper.terminate(lid::read());
            ffi::log("SIGTERM: every journaled value put back");
            return 0;
        }
        let served = wake(&listener, &policy, &mut helper);
        let now_ms = ffi::monotonic_ms();
        tick(&mut helper, &pmset, now_ms, &mut readings_due_ms);
        if served || !helper.idle() {
            active_ms = now_ms;
        }
        if now_ms.saturating_sub(active_ms) >= IDLE_EXIT_MS {
            assert!(helper.idle());
            ffi::log("idle: exit");
            return 0;
        }
    }
}

/// Waits up to `WAKE_MS` for a connection and serves at most one. Output: whether a caller
/// connected.
fn wake(listener: &UnixListener, policy: &Policy, helper: &mut Helper) -> bool {
    match ffi::wait_readable(listener.as_fd(), WAKE_MS) {
        Ok(true) => {}
        Ok(false) => return false,
        Err(error) => {
            ffi::log(&format!("poll failed: {error}"));
            thread::sleep(ERROR_PAUSE);
            return false;
        }
    }
    let stream = match listener.accept() {
        Ok((stream, _)) => stream,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return false,
        Err(error) => {
            ffi::log(&format!("accept failed: {error}"));
            thread::sleep(ERROR_PAUSE);
            return false;
        }
    };
    if let Ok(accepted) = Accepted::take(stream, policy) {
        let _logged_if_refused = accepted.serve(policy, helper, ffi::monotonic_ms());
    }
    true
}

/// Expires leases and, when readings are wanted and due, takes one. The first wake after a
/// lease starts reads at once, then every `READINGS_EVERY_MS`.
fn tick(helper: &mut Helper, pmset: &Pmset, now_ms: u64, readings_due_ms: &mut u64) {
    if helper.has_expired(now_ms) {
        helper.expire(now_ms, lid::read());
    }
    if !helper.wants_readings() {
        *readings_due_ms = now_ms;
        return;
    }
    if now_ms < *readings_due_ms {
        return;
    }
    helper.sense(now_ms, readings(pmset));
    *readings_due_ms = now_ms + READINGS_EVERY_MS;
    assert!(*readings_due_ms > now_ms);
}

/// One reading: the battery from `pmset -g batt`, the thermal pressure level and the lid.
fn readings(pmset: &Pmset) -> Readings {
    let battery = pmset.battery().unwrap_or_else(|error| {
        ffi::log(&format!("pmset -g batt failed: {error:?}"));
        Battery::Unread
    });
    let pressure = ffi::thermal_pressure().map_or(Pressure::Unread, Pressure::Level);
    Readings {
        battery,
        pressure,
        lid: lid::read(),
    }
}

/// The installed helper: refuses to run as anyone but root, takes launchd's socket and serves
/// with the production configuration and policy. Output: the exit code.
pub fn main_production() -> u8 {
    if ffi::euid() != 0 {
        ffi::log("the power helper runs as root only");
        return 2;
    }
    let listener = match ffi::launch_socket(LISTENERS) {
        Ok(socket) => UnixListener::from(socket),
        Err(error) => {
            ffi::log(&format!("no launchd socket: {error}"));
            return 3;
        }
    };
    let code = run(listener, Config::production(), PRODUCTION);
    u8::try_from(code).unwrap_or(1)
}
