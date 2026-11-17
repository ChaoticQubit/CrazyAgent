use rustix::event::{PollFd, Timespec, poll};
use rustix::io::Errno;
use std::io;
use std::time::{Duration, Instant};

/// The longest one poll(2) call waits. A longer wait takes several calls, so a far deadline
/// never overflows the call's millisecond timeout.
const CALL_WAIT_MAX: Duration = Duration::from_secs(3600);

/// The most descriptors one wait watches.
const FDS_MAX: usize = 8;

/// Waits in poll(2) until one of `fds` is ready or `until` passes, and waits again for the
/// time then left after a signal interrupts the call. Inputs: the descriptors with the events
/// each waits for, and the instant to stop at, or none to wait until one is ready. Output:
/// how many descriptors are ready, 0 once `until` has passed, or the poll error.
pub(crate) fn ready(fds: &mut [PollFd<'_>], until: Option<Instant>) -> io::Result<usize> {
    assert!(!fds.is_empty());
    assert!(fds.len() <= FDS_MAX);
    loop {
        let wait = until.map(|until| {
            let left = until.saturating_duration_since(Instant::now());
            Timespec::try_from(left.min(CALL_WAIT_MAX))
                .expect("a wait of at most an hour fits a timespec")
        });
        match poll(fds, wait.as_ref()) {
            Ok(0) => {
                let Some(until) = until else {
                    unreachable!("poll(2) without a timeout returned with nothing ready");
                };
                if Instant::now() >= until {
                    return Ok(0);
                }
            }
            Ok(count) => {
                assert!(count <= fds.len());
                return Ok(count);
            }
            Err(Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
}
