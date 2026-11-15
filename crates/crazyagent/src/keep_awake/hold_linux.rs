use super::hold::{Held, Hold, HoldError, REASON, wanted};
use super::state::Want;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use std::os::fd::OwnedFd;
use std::time::Duration;

const BUS_WAIT: Duration = Duration::from_secs(2);
const SERVICE: &str = "org.freedesktop.login1";
const OBJECT: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";

/// The Linux idle hold: one logind inhibitor lock for idle and sleep, held as the
/// descriptor logind hands out. logind drops the lock when the descriptor closes, so it
/// ends with the engine process.
#[derive(Debug, Default)]
pub(super) struct LinuxHold {
    lock: Option<OwnedFd>,
}

/// Asks logind for the lock over the system bus, waiting at most `BUS_WAIT` for the
/// answer, and drops the bus connection again. Output: the lock descriptor, or
/// `Unavailable` with the step that failed.
fn take() -> Result<OwnedFd, HoldError> {
    let bus = zbus::blocking::connection::Builder::system()
        .map_err(|problem| HoldError::unavailable("system bus", problem))?
        .method_timeout(BUS_WAIT)
        .build()
        .map_err(|problem| HoldError::unavailable("system bus", problem))?;
    let reply = bus
        .call_method(
            Some(SERVICE),
            OBJECT,
            Some(MANAGER),
            "Inhibit",
            &("idle:sleep", "CrazyAgent", REASON, "block"),
        )
        .map_err(|problem| HoldError::unavailable("logind inhibit", problem))?;
    let lock: zbus::zvariant::OwnedFd = reply
        .body()
        .deserialize()
        .map_err(|problem| HoldError::unavailable("logind inhibit", problem))?;
    Ok(lock.into())
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

impl Hold for LinuxHold {
    fn apply(&mut self, want: &Want) -> Result<Held, HoldError> {
        let lost = self.lock.as_ref().is_some_and(gone);
        if lost {
            self.lock = None;
        }
        if wanted(want).is_none() {
            self.release();
            return Ok(Held::NONE);
        }
        if lost {
            return Err(HoldError::Lost);
        }
        if self.lock.is_none() {
            self.lock = Some(take()?);
        }
        assert!(self.lock.is_some());
        Ok(Held {
            idle: true,
            lid: false,
            display: false,
        })
    }

    fn release(&mut self) {
        self.lock = None;
        assert!(self.lock.is_none());
    }
}
