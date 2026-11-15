use super::hold::{Held, Hold, HoldError, REASON, wanted};
use super::state::Want;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
use std::os::fd::{AsFd, OwnedFd};
use std::time::Duration;

const BUS_WAIT: Duration = Duration::from_secs(2);
const REPLY_FDS_MAX: usize = 16;
const SERVICE: &str = "org.freedesktop.login1";
const OBJECT: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";
const WHAT_IDLE: &str = "idle:sleep";
const WHAT_LID: &str = "idle:sleep:handle-lid-switch";

/// The Linux hold: one logind inhibitor lock, for idle and sleep, or for those and the
/// lid switch, held as the descriptor logind hands out. logind drops the lock when the
/// descriptor closes, so it ends with the engine process. Once the lid lock is gone,
/// logind itself looks at a closed lid again and applies the computer's own lid rule
/// (DEC-167 addendum 9), so the hold asks for no suspend.
#[derive(Debug, Default)]
pub(super) struct LinuxHold {
    lock: Option<OwnedFd>,
    lid: bool,
}

/// Marks one open descriptor close-on-exec and checks that the mark is set. Input: the
/// descriptor. Output: none, or `Unavailable` when the system refuses the flag.
pub(super) fn close_on_exec(descriptor: impl AsFd) -> Result<(), HoldError> {
    let descriptor = descriptor.as_fd();
    let flags = fcntl_getfd(descriptor)
        .map_err(|problem| HoldError::unavailable("lock descriptor", problem))?;
    fcntl_setfd(descriptor, flags | FdFlags::CLOEXEC)
        .map_err(|problem| HoldError::unavailable("lock descriptor", problem))?;
    let marked = fcntl_getfd(descriptor)
        .map_err(|problem| HoldError::unavailable("lock descriptor", problem))?;
    assert!(marked.contains(FdFlags::CLOEXEC));
    Ok(())
}

/// Asks logind for the lock on an open bus connection and drops the connection. The bus
/// hands the descriptor over without close-on-exec, so this runs inside the spawn gate:
/// every descriptor of the reply is marked before the gate opens and a child can start.
/// Inputs: the connection, whose calls wait at most `BUS_WAIT`, and what the lock covers.
/// Output: the lock descriptor, or `Unavailable` with the step that failed.
fn inhibit(bus: zbus::blocking::Connection, what: &str) -> Result<OwnedFd, HoldError> {
    assert!(what == WHAT_IDLE || what == WHAT_LID);
    let reply = bus
        .call_method(
            Some(SERVICE),
            OBJECT,
            Some(MANAGER),
            "Inhibit",
            &(what, "CrazyAgent", REASON, "block"),
        )
        .map_err(|problem| HoldError::unavailable("logind inhibit", problem))?;
    let handed = reply.data().fds();
    let mut marked = Ok(());
    for descriptor in handed.iter().take(REPLY_FDS_MAX) {
        marked = marked.and(close_on_exec(descriptor));
    }
    marked?;
    if handed.len() != 1 {
        let count = format!("{} descriptors in the reply", handed.len());
        return Err(HoldError::unavailable("logind inhibit", count));
    }
    let lock: zbus::zvariant::OwnedFd = reply
        .body()
        .deserialize()
        .map_err(|problem| HoldError::unavailable("logind inhibit", problem))?;
    let lock: OwnedFd = lock.into();
    close_on_exec(&lock)?;
    drop(reply);
    drop(bus);
    Ok(lock)
}

/// Takes a lock: connects to the system bus outside the spawn gate, since the connect
/// has no time limit of its own, then asks logind inside the gate, where the call waits at
/// most `BUS_WAIT`. Input: what the lock covers. Output: the lock descriptor, or
/// `Unavailable` with the step that failed.
fn take(what: &'static str) -> Result<OwnedFd, HoldError> {
    let bus = zbus::blocking::connection::Builder::system()
        .map_err(|problem| HoldError::unavailable("system bus", problem))?
        .method_timeout(BUS_WAIT)
        .build()
        .map_err(|problem| HoldError::unavailable("system bus", problem))?;
    crate::spawn_gate::guarded(move || inhibit(bus, what))
}

/// Tells whether logind let go of the lock: its end of the descriptor hung up or failed.
/// A poll that itself fails counts as still held; the next look asks again. Input: the
/// lock. Output: true when the lock is gone.
fn gone(lock: &OwnedFd) -> bool {
    let mut fds = [PollFd::new(lock, PollFlags::empty())];
    let at_once = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let polled = poll(&mut fds, Some(&at_once));
    let dropped = PollFlags::ERR | PollFlags::HUP | PollFlags::NVAL;
    polled.is_ok() && fds[0].revents().intersects(dropped)
}

impl LinuxHold {
    /// Brings the lock to what is wanted. Each new lock is taken before the old one
    /// closes, so there is no moment without a lock. When logind refuses the lock with the
    /// lid switch, the refusal is logged and the idle lock stays, or is taken, in its
    /// place. Input: whether the lid is wanted. Output: none, or the error of the idle
    /// lock, which nothing replaces.
    fn cover(&mut self, lid: bool) -> Result<(), HoldError> {
        if lid {
            match take(WHAT_LID) {
                Ok(lock) => {
                    self.lock = Some(lock);
                    self.lid = true;
                    return Ok(());
                }
                Err(problem) => eprintln!("keep awake: no lid-closed lock: {problem}"),
            }
            if self.lock.is_some() && !self.lid {
                return Ok(());
            }
        }
        self.lock = Some(take(WHAT_IDLE)?);
        self.lid = false;
        Ok(())
    }
}

impl Hold for LinuxHold {
    fn apply(&mut self, want: &Want) -> Result<Held, HoldError> {
        let lost = self.lock.as_ref().is_some_and(gone);
        if lost {
            self.release();
        }
        if wanted(want).is_none() {
            self.release();
            return Ok(Held::NONE);
        }
        if lost {
            return Err(HoldError::Lost);
        }
        let lid = matches!(want, Want::IdleAndLid { .. });
        if self.lock.is_none() || lid != self.lid {
            self.cover(lid)?;
        }
        assert!(self.lock.is_some());
        assert!(lid || !self.lid);
        Ok(Held {
            idle: true,
            lid: self.lid,
            display: false,
        })
    }

    fn release(&mut self) {
        self.lock = None;
        self.lid = false;
        assert!(self.lock.is_none());
    }
}
