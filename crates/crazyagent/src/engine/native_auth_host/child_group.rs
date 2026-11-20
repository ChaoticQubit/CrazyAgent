use std::io;
use std::process::{Child, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(5);
pub(super) const DROP_TIME: Duration = Duration::from_secs(2);
const GROUP_SETTLE_TIME: Duration = Duration::from_millis(500);

/// What custody holds a sign-in login child's whole tree by: its dedicated process group on
/// Unix.
#[cfg(unix)]
pub(super) type ChildGroup = rustix::process::Pid;

/// What custody holds a sign-in login child's whole tree by: the kill-on-close job of its
/// own the child was put in while still suspended, on Windows.
#[cfg(windows)]
pub(super) type ChildGroup = crazyagent_win_sandbox::ProcessTree;

/// Sleeps one poll step, never past the deadline. Input: the deadline. Output: none.
pub(super) fn sleep_until(deadline: Instant) {
    let remaining = deadline.saturating_duration_since(Instant::now());
    thread::sleep(POLL_INTERVAL.min(remaining));
}

/// The group a just-started login child leads, while it is its own dedicated group. Input:
/// the child. Output: the group, or none.
#[cfg(unix)]
pub(super) fn dedicated_group(child: &Child) -> Option<ChildGroup> {
    let group = i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)?;
    (rustix::process::getpgid(Some(group)) == Ok(group)).then_some(group)
}

/// Waits until the process group of a dedicated child that was already reaped is gone.
/// macOS can still find such a group for some milliseconds and answer EPERM, no member it
/// may signal, before it answers ESRCH; a member it may signal answers at once and is a
/// survivor. Input: the group. Output: whether the group was gone within the settle bound.
#[cfg(unix)]
pub(super) fn reaped_group_gone(group: &ChildGroup) -> bool {
    let group = *group;
    assert!(group.as_raw_nonzero().get() > 1);
    let deadline = Instant::now() + GROUP_SETTLE_TIME;
    loop {
        match rustix::process::test_kill_process_group(group) {
            Err(rustix::io::Errno::SRCH) => return true,
            Err(rustix::io::Errno::PERM) if Instant::now() < deadline => sleep_until(deadline),
            _ => return false,
        }
    }
}

/// Kills a direct child and waits for it, within [`DROP_TIME`] and the deadline. Inputs: the
/// child and the deadline. Output: its status, none when it outlived the deadline, or the
/// kill error.
pub(in crate::engine) fn cleanup_direct_child(
    child: &mut Child,
    deadline: Instant,
) -> io::Result<Option<ExitStatus>> {
    if let Some(status) = child.try_wait()? {
        return Ok(Some(status));
    }
    let started = Instant::now();
    if started >= deadline {
        return Ok(None);
    }
    let deadline = deadline.min(started + DROP_TIME);
    assert!(deadline > started);
    assert!(deadline.duration_since(started) <= DROP_TIME);
    if let Err(error) = child.kill() {
        return match child.try_wait()? {
            Some(status) => Ok(Some(status)),
            None => Err(error),
        };
    }
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        sleep_until(deadline);
    }
}

/// Kills a login child's dedicated group and waits until the child is reaped and the group
/// is gone, within [`DROP_TIME`] and the deadline. Inputs: the child, its group and the
/// deadline. Output: whether both happened in time, or the failed step.
#[cfg(unix)]
pub(super) fn cleanup_owned_group(
    child: &mut Child,
    group: &ChildGroup,
    deadline: Instant,
) -> io::Result<bool> {
    use rustix::process::{Pid, Signal};
    let group = *group;
    let direct_id = child.id();
    let direct = Pid::from_raw(i32::try_from(direct_id).map_err(io::Error::other)?)
        .ok_or_else(|| io::Error::other("invalid direct child identity"))?;
    if group != direct {
        return Err(io::Error::other("child group is not dedicated"));
    }
    let started = Instant::now();
    let deadline = deadline.min(started + DROP_TIME);
    let mut signalled = false;
    while Instant::now() < deadline {
        let reaped = child.try_wait()?.is_some();
        match rustix::process::test_kill_process_group(group) {
            Err(rustix::io::Errno::SRCH) if reaped => return Ok(true),
            Err(rustix::io::Errno::SRCH) => {}
            Ok(()) if !signalled => {
                match rustix::process::kill_process_group(group, Signal::KILL) {
                    Ok(()) | Err(rustix::io::Errno::PERM) | Err(rustix::io::Errno::SRCH) => {}
                    Err(error) => return Err(error.into()),
                }
                signalled = true;
            }
            Ok(()) | Err(rustix::io::Errno::PERM) => {}
            Err(error) => return Err(error.into()),
        }
        assert_eq!(group, direct);
        assert_eq!(child.id(), direct_id);
        sleep_until(deadline);
    }
    Ok(false)
}

/// Cleans up a login child custody did not retain: its own group by its id when it leads
/// one, else the child alone. Inputs: the child and the deadline. Output: whether it and its
/// group ended in time, or the failed step.
#[cfg(unix)]
pub(super) fn cleanup_unretained(child: &mut Child, deadline: Instant) -> io::Result<bool> {
    match i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        Some(group) => cleanup_owned_group(child, &group, deadline),
        None => cleanup_direct_child(child, deadline).map(|status| status.is_some()),
    }
}

/// Whether the tree of a login child that was already reaped has no process left, read from
/// its job without ending anything, so a grandchild that outlived the child reads as a
/// survivor. Not built yet (N4a red rows). Input: the tree. Output: the answer.
#[cfg(windows)]
pub(super) fn reaped_group_gone(group: &ChildGroup) -> bool {
    let _ = (group, GROUP_SETTLE_TIME);
    true
}

/// Ends a login child's whole tree and waits until the child is reaped and no process of
/// the tree is left. Not built yet (N4a red rows): it ends the child alone. Inputs: the
/// child, its tree and the deadline. Output: whether both happened in time, or the failed
/// step.
#[cfg(windows)]
pub(super) fn cleanup_owned_group(
    child: &mut Child,
    group: &ChildGroup,
    deadline: Instant,
) -> io::Result<bool> {
    let _ = group;
    cleanup_direct_child(child, deadline).map(|status| status.is_some())
}
