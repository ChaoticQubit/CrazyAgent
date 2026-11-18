#[cfg(unix)]
use rustix::event::{PollFd, PollFlags, Timespec, poll};
#[cfg(unix)]
use rustix::io::Errno;
use std::io;
use std::net::TcpListener;
use std::time::{Duration, Instant};

/// The longest one poll(2) call waits. A longer wait takes several calls, so a far deadline
/// never overflows the call's millisecond timeout.
#[cfg(unix)]
const CALL_WAIT_MAX: Duration = Duration::from_secs(3600);

/// The most descriptors one wait watches.
#[cfg(unix)]
const FDS_MAX: usize = 8;

/// The most reads one [`Wake::drain`] makes, so a flood of wakes cannot hold the waiter.
#[cfg(unix)]
const DRAIN_READS_MAX: usize = 64;

/// One end of a pair that wakes a thread waiting in poll(2): its owner calls
/// [`Wake::rouse`] and the thread waits on the other end, then drains it. Windows has no such
/// pair here, so its waiters sleep instead.
pub(crate) struct Wake(#[cfg(unix)] std::os::unix::net::UnixStream);

/// Makes a wake pair with both ends non-blocking. Output: the end to rouse and the end to
/// wait on, or the socket error.
#[cfg(unix)]
pub(crate) fn wake_pair() -> io::Result<(Wake, Wake)> {
    let (rouse, woken) = crate::spawn_gate::unix_pair_std()?;
    rouse.set_nonblocking(true)?;
    woken.set_nonblocking(true)?;
    Ok((Wake(rouse), Wake(woken)))
}

/// Makes the stand-in pair for a system with no poll(2) here. Output: both ends.
#[cfg(not(unix))]
pub(crate) fn wake_pair() -> io::Result<(Wake, Wake)> {
    Ok((Wake(), Wake()))
}

impl Wake {
    /// Wakes the thread waiting on the other end by writing one byte. A full buffer already
    /// holds a wake and a closed end has no waiter, so neither is an error (Rust programs
    /// ignore SIGPIPE, so a closed end only fails the write); any other failure leaves the
    /// waiter to its own wait bound. Input: the end. Output: none.
    pub(crate) fn rouse(&self) {
        #[cfg(unix)]
        {
            use std::io::Write;
            let _ = (&self.0).write(&[1]);
        }
    }

    /// Reads away the bytes [`Wake::rouse`] wrote, at most [`DRAIN_READS_MAX`] reads, so the
    /// end stops being ready until the next wake. Input: the end. Output: none.
    #[cfg(unix)]
    pub(crate) fn drain(&self) {
        use std::io::Read;
        let mut bytes = [0_u8; 64];
        for _ in 0..DRAIN_READS_MAX {
            match (&self.0).read(&mut bytes) {
                Ok(count) if count == bytes.len() => {}
                Ok(_) | Err(_) => return,
            }
        }
    }
}

#[cfg(unix)]
impl std::os::fd::AsFd for Wake {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}

/// Waits until one of `listeners` has a client or `woken` is roused, at most until `until`,
/// then drains `woken`. Sleeps `fallback` instead where there is no poll(2) here or when the
/// poll fails, so the wait never becomes a busy loop. Inputs: the listeners, the wake end,
/// the instant to stop at and the fallback sleep. Output: none.
pub(crate) fn wait_for_clients(
    listeners: &[&TcpListener],
    woken: &Wake,
    until: Instant,
    fallback: Duration,
) {
    assert!(!listeners.is_empty());
    assert!(fallback > Duration::ZERO);
    #[cfg(unix)]
    {
        let mut fds: Vec<PollFd<'_>> = listeners
            .iter()
            .map(|listener| PollFd::new(*listener, PollFlags::IN))
            .collect();
        fds.push(PollFd::new(woken, PollFlags::IN));
        if ready(&mut fds, Some(until)).is_err() {
            std::thread::sleep(fallback);
        }
        woken.drain();
    }
    #[cfg(not(unix))]
    {
        let _ = (woken, until);
        std::thread::sleep(fallback);
    }
}

/// Waits in poll(2) until one of `fds` is ready or `until` passes, and waits again for the
/// time then left after a signal interrupts the call. Inputs: the descriptors with the events
/// each waits for, and the instant to stop at, or none to wait until one is ready. Output:
/// how many descriptors are ready, 0 once `until` has passed, or the poll error.
#[cfg(unix)]
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
