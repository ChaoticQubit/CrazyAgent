#[cfg(unix)]
use rustix::event::{PollFd, PollFlags};
#[cfg(unix)]
use rustix::io::Errno;
#[cfg(unix)]
use rustix::process::{Pid, WaitId, WaitIdOptions};
use std::io;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd};
use std::process::{Child, ExitStatus};
use std::time::Instant;

#[cfg(test)]
pub(crate) mod tests;

/// A wait armed on one child that is not yet reaped, so the caller learns of the exit as it
/// happens instead of on a timed poll. The caller keeps the [`Child`] and does the one reap
/// through [`ExitWatch::reap`], only after an exit or after its own kill. A watch dropped
/// without a reap reaps nothing and never blocks the dropper; its waiter thread, if any,
/// ends when the child exits, its answer written to a closed pipe and dropped.
pub struct ExitWatch {
    arm: Arm,
}

/// How a watch learns of the exit: nothing to wait for, a Linux process descriptor, a
/// waiter thread, or the process handle the Windows [`Child`] holds.
enum Arm {
    Reaped,
    #[cfg(target_os = "linux")]
    Pidfd(std::os::fd::OwnedFd),
    #[cfg(unix)]
    Waiter(Waiter),
    #[cfg(windows)]
    Handle,
}

/// The read end a waiter thread writes its one answer to, and the answer once read.
#[cfg(unix)]
struct Waiter {
    answer: std::io::PipeReader,
    heard: Option<Heard>,
    #[cfg(test)]
    release: Option<std::sync::mpsc::Sender<()>>,
}

/// What a waiter thread's `waitid` returned: the child exited, it was already gone
/// (`ECHILD`), or the call failed; a waiter that ended without writing reads as gone.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Heard {
    Exited,
    Gone,
    Failed,
}

/// What woke a wait that also watches a descriptor: the child exited, the descriptor is
/// readable, or the deadline passed.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Woke {
    Exited,
    Readable,
    Deadline,
}

/// Arms a wait on `child`. A child std already reaped arms nothing, so no call ever names a
/// process number another process may since have taken. Input: the child, not yet waited
/// on by this caller. Output: the watch, or the error reading or arming it.
pub fn watch(child: &mut Child) -> io::Result<ExitWatch> {
    if child.try_wait()?.is_some() {
        return Ok(ExitWatch { arm: Arm::Reaped });
    }
    #[cfg(test)]
    tests::hooks::armed();
    Ok(ExitWatch { arm: arm(child)? })
}

/// The process number of a child that is not yet reaped. Input: the child. Output: its
/// number.
#[cfg(unix)]
fn pid_of(child: &Child) -> Pid {
    let raw = i32::try_from(child.id()).expect("a process number fits an i32");
    Pid::from_raw(raw).expect("a child's process number is positive")
}

/// Arms a Linux process descriptor for the child, or a waiter thread when the system
/// refuses one. Input: the child. Output: the arm, or the error starting the waiter.
#[cfg(target_os = "linux")]
fn arm(child: &Child) -> io::Result<Arm> {
    let pid = pid_of(child);
    match pidfd_open(pid) {
        Ok(fd) => Ok(Arm::Pidfd(fd)),
        Err(error) => {
            fallback_noted(error);
            waiter(pid)
        }
    }
}

/// Opens a process descriptor for `pid`, or fails as a test forces it to. Input: the
/// process number. Output: the descriptor, or the error.
#[cfg(target_os = "linux")]
fn pidfd_open(pid: Pid) -> Result<std::os::fd::OwnedFd, Errno> {
    #[cfg(test)]
    if let Some(error) = tests::hooks::forced_pidfd_error() {
        return Err(error);
    }
    rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())
}

/// How many times this process has said its process descriptors fell back to a waiter
/// thread: at most once.
#[cfg(target_os = "linux")]
static FALLBACK_SAID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Says, once per process, that a process descriptor was refused and a waiter thread waits
/// instead, so a system that blocks them is visible. Input: the refusal. Output: none.
#[cfg(target_os = "linux")]
fn fallback_noted(error: Errno) {
    use std::sync::atomic::Ordering;
    if FALLBACK_SAID
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        eprintln!("exit_wait: pidfd_open refused ({error}), exits are waited on a waiter thread");
    }
    assert!(FALLBACK_SAID.load(Ordering::Acquire) == 1);
}

/// Arms a waiter thread for the child where no process descriptor exists. Input: the
/// child. Output: the arm, or the error starting the waiter.
#[cfg(all(unix, not(target_os = "linux")))]
fn arm(child: &Child) -> io::Result<Arm> {
    waiter(pid_of(child))
}

/// Arms nothing more: the Windows child already holds its process handle. Input: the
/// child. Output: the arm.
#[cfg(windows)]
fn arm(_child: &Child) -> io::Result<Arm> {
    Ok(Arm::Handle)
}

/// Starts one thread that waits in `waitid(EXITED | NOWAIT)` for `pid` and writes its one
/// answer to a pipe. NOWAIT leaves the child unreaped, so its number stays its own until
/// the caller's reap, which reads the answer first. Input: the process number. Output: the
/// arm holding the read end, or the error making the pipe or the thread.
#[cfg(unix)]
fn waiter(pid: Pid) -> io::Result<Arm> {
    let (answer, mut writer) = crate::spawn_gate::guarded(std::io::pipe)?;
    #[cfg(test)]
    let (release, hooks) = tests::hooks::for_waiter();
    std::thread::Builder::new()
        .name("exit-wait".into())
        .spawn(move || {
            #[cfg(test)]
            hooks.before_waitid();
            let heard = loop {
                match rustix::process::waitid(
                    WaitId::Pid(pid),
                    WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
                ) {
                    Ok(_) => break Heard::Exited,
                    Err(Errno::INTR) => {}
                    Err(Errno::CHILD) => break Heard::Gone,
                    Err(_) => break Heard::Failed,
                }
            };
            let _ = writer.write_all(&[heard.byte()]);
        })?;
    Ok(Arm::Waiter(Waiter {
        answer,
        heard: None,
        #[cfg(test)]
        release,
    }))
}

#[cfg(unix)]
impl Heard {
    /// The byte a waiter writes for this answer. Output: the byte.
    fn byte(self) -> u8 {
        match self {
            Heard::Exited => b'x',
            Heard::Gone => b'g',
            Heard::Failed => b'f',
        }
    }

    /// The answer a byte stands for, or gone for none or any other byte. Input: the byte
    /// read, if any. Output: the answer.
    fn of(byte: Option<u8>) -> Heard {
        match byte {
            Some(b'x') => Heard::Exited,
            Some(b'f') => Heard::Failed,
            _ => Heard::Gone,
        }
    }
}

#[cfg(unix)]
impl Waiter {
    /// Reads the waiter's one answer, waiting for it when `block`; a closed pipe reads as
    /// gone, so a waiter that died cannot hold the reader. Input: whether to wait. Output:
    /// the answer, or none when not waiting and none was written yet.
    fn hear(&mut self, block: bool) -> Option<Heard> {
        if self.heard.is_none() && block {
            let mut byte = [0u8; 1];
            let read = loop {
                match self.answer.read(&mut byte) {
                    Ok(count) => break count,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break 0,
                }
            };
            self.heard = Some(Heard::of((read == 1).then_some(byte[0])));
        }
        self.heard
    }
}

impl ExitWatch {
    /// Waits until the child has exited or `deadline` passes, reaping nothing; it may be
    /// called again, and never arms a second waiter. Inputs: the watched child and the
    /// instant to stop at. Output: true once it exited, false at the deadline, or the wait
    /// error.
    pub fn exited_by(&mut self, child: &Child, deadline: Instant) -> io::Result<bool> {
        #[cfg(unix)]
        {
            Ok(self.woke_by(child, None, deadline)? == Woke::Exited)
        }
        #[cfg(windows)]
        {
            match self.arm {
                Arm::Reaped => Ok(true),
                Arm::Handle => crazyagent_win_sandbox::wait_for_exit(child, deadline),
            }
        }
    }

    /// Waits until the child has exited, `also` is readable, or `deadline` passes, reaping
    /// nothing. Inputs: the watched child, the descriptor to watch beside it, if any, and the
    /// instant to stop at. Output: what woke the wait, or the poll error.
    #[cfg(unix)]
    pub fn woke_by(
        &mut self,
        _child: &Child,
        also: Option<BorrowedFd<'_>>,
        deadline: Instant,
    ) -> io::Result<Woke> {
        let exit_fd = match &self.arm {
            Arm::Reaped => return Ok(Woke::Exited),
            #[cfg(target_os = "linux")]
            Arm::Pidfd(fd) => fd.as_fd(),
            Arm::Waiter(waiter) if waiter.heard.is_some() => return Ok(Woke::Exited),
            Arm::Waiter(waiter) => waiter.answer.as_fd(),
        };
        let mut fds = vec![PollFd::from_borrowed_fd(exit_fd, PollFlags::IN)];
        if let Some(also) = also {
            fds.push(PollFd::from_borrowed_fd(also, PollFlags::IN));
        }
        if crate::fd_wait::ready(&mut fds, Some(deadline))? == 0 {
            return Ok(Woke::Deadline);
        }
        let exited = !fds[0].revents().is_empty();
        let readable = fds.get(1).is_some_and(|fd| !fd.revents().is_empty());
        drop(fds);
        if exited {
            if let Arm::Waiter(waiter) = &mut self.arm {
                waiter.hear(true);
            }
            return Ok(Woke::Exited);
        }
        assert!(readable);
        Ok(Woke::Readable)
    }

    /// Reaps the child once: first waits for a waiter thread's answer, so its `waitid`
    /// always comes before the reap and never names a reused number, then `Child::wait`.
    /// Call it only after an exit or after a kill. Input: the watched child. Output: its
    /// status, or the error `Child::wait` gives.
    pub fn reap(self, child: &mut Child) -> io::Result<ExitStatus> {
        self.reap_heard(child).1
    }

    /// [`ExitWatch::reap`], also giving what the waiter thread answered, if one waited.
    /// Input: the watched child. Output: the answer and the status or error.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn reap_heard(
        self,
        child: &mut Child,
    ) -> (Option<HeardAnswer>, io::Result<ExitStatus>) {
        let heard = self.settle();
        #[cfg(test)]
        tests::hooks::reaped(heard);
        (heard, child.wait())
    }

    /// Waits for a waiter thread's answer, releasing a test's hold on it first. Output: the
    /// answer, or none when no thread waited.
    fn settle(self) -> Option<HeardAnswer> {
        #[cfg(unix)]
        if let Arm::Waiter(mut waiter) = self.arm {
            #[cfg(test)]
            if let Some(release) = waiter.release.take() {
                let _ = release.send(());
            }
            return waiter.hear(true);
        }
        None
    }
}

/// What [`ExitWatch::reap_heard`] reports a waiter answered: [`Heard`] on Unix, nothing on
/// Windows, where no thread waits.
#[cfg(unix)]
pub(crate) type HeardAnswer = Heard;

/// What [`ExitWatch::reap_heard`] reports a waiter answered: nothing on Windows, where no
/// thread waits.
#[cfg(windows)]
pub(crate) type HeardAnswer = std::convert::Infallible;
