#[cfg(target_os = "macos")]
use super::{KEEPER_POLL, KEEPER_WAIT};
#[cfg(target_os = "macos")]
use kernel_processes::terminal::{AgentId, Host};
use std::path::Path;
#[cfg(target_os = "macos")]
use std::time::Instant;

/// Asks a pane's first process to stop with terminate and waits up to `KEEPER_WAIT` for the
/// pane to end, after the host kept its last screen, since the pane ends before any kill.
/// A [`crate::sandbox::KEEPER`] then has the run's warden kill every process in the sandbox
/// and ends only once the warden confirmed that or found signals not confined, so the pane
/// ending in time confirms it. Inputs: the host, the pane and its first process, proven the
/// pane's within `GROUP_FRESH`. Output: whether the pane is gone.
#[cfg(target_os = "macos")]
pub(crate) fn stop_keeper(host: &dyn Host, agent: &AgentId, pid: u32) -> bool {
    let gone = || host.alive().is_ok_and(|alive| !alive.contains(agent));
    if gone() {
        return true;
    }
    let Some(process) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    host.keep_last(agent);
    let _ = rustix::process::kill_process(process, rustix::process::Signal::TERM);
    let deadline = Instant::now() + KEEPER_WAIT;
    while !gone() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(KEEPER_POLL);
    }
    true
}

/// Which way [`hold_keeper`] moves a pane's sandbox: stopped while no engine holds its
/// model proxy, or running again once one does (DEC-158).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    Freeze,
    Thaw,
}

/// Asks a [`crate::sandbox::KEEPER`] given a freeze record to have its warden stop or
/// continue every process in the run's sandbox, and waits up to `KEEPER_WAIT` for the
/// keeper to write the warden's answer to that record (DEC-158). The record is removed
/// first, so only this answer confirms. Inputs: the keeper's process id, which the caller
/// proved current, the record and the way. Output: whether the warden confirmed.
#[cfg(target_os = "macos")]
pub fn hold_keeper(pid: u32, record: &Path, hold: Hold) -> bool {
    assert!(record.is_absolute());
    match std::fs::remove_file(record) {
        Ok(()) => {}
        Err(problem) if problem.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return false,
    }
    let Some(process) = i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 1)
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    let (signal, answer) = match hold {
        Hold::Freeze => (rustix::process::Signal::USR1, b'S'),
        Hold::Thaw => (rustix::process::Signal::USR2, b'C'),
    };
    if rustix::process::kill_process(process, signal).is_err() {
        return false;
    }
    let deadline = Instant::now() + KEEPER_WAIT;
    loop {
        if keeper_answer(record) == Some(answer) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(KEEPER_POLL);
    }
}

/// Freezes nothing where no keeper runs: no agent pane runs off macOS (DEC-094), so a
/// kept pane there is ended rather than kept unfrozen (DEC-158 Q2). Inputs: unused.
/// Output: false.
#[cfg(not(target_os = "macos"))]
pub fn hold_keeper(_pid: u32, _record: &Path, _hold: Hold) -> bool {
    false
}

/// Reads the one-byte answer a keeper wrote to its freeze record, never following a link
/// and only from this user's plain file. Input: the record. Output: the byte, or none.
#[cfg(target_os = "macos")]
fn keeper_answer(record: &Path) -> Option<u8> {
    use rustix::fs::{Mode, OFlags};
    let file = rustix::fs::open(
        record,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .ok()?;
    let stat = rustix::fs::fstat(&file).ok()?;
    let plain =
        rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::RegularFile;
    if !plain || stat.st_uid != rustix::process::getuid().as_raw() || stat.st_size != 1 {
        return None;
    }
    let mut byte = [0u8; 1];
    (rustix::io::read(&file, &mut byte[..]).ok()? == 1).then_some(byte[0])
}
