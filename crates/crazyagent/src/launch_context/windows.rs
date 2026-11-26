use super::*;

/// Confirms on Windows that a launch lock is the lock name directly in the held home
/// (CodeRabbit CR-1252): both real places are read from their handles, so a lock the home's
/// path reached in another folder, as after a junction above the home was pointed elsewhere
/// between the hold and the lock, is refused. Inputs: the lock and the held home. Output:
/// Ok, or `launch_context_unsafe`.
#[cfg(windows)]
pub(super) fn locked_in(lock: &File, held: &File) -> Result<(), LaunchError> {
    const SEPARATOR: u16 = b'\\' as u16;
    let unsafe_lock = |_: std::io::Error| failure("launch_context_unsafe");
    let mut wanted = crazyagent_win_sandbox::final_name(held).map_err(unsafe_lock)?;
    if wanted.last() != Some(&SEPARATOR) {
        wanted.push(SEPARATOR);
    }
    wanted.extend(LOCK.encode_utf16());
    let found = crazyagent_win_sandbox::final_name(lock).map_err(unsafe_lock)?;
    if found != wanted {
        return Err(failure("launch_context_unsafe"));
    }
    Ok(())
}

/// Opens and locks the agent home's launch lock through W1 (V244-WIN-MASTER S9a): made a
/// private file when it is missing, else opened, never through a link, a second name or a
/// list that is not private, then locked without waiting. A name a launched agent left as a
/// link, a folder or an open file stays refused until S9b clears it handle-relative; no
/// Windows launch reuses a home before then (D6, D8). Input: the home H1's check passed.
/// Output: the locked file, `launch_context_unsafe` or `launch_home_busy`.
#[cfg(windows)]
pub(super) fn lock(home: &Path) -> Result<File, LaunchError> {
    use crazyagent_win_sandbox::private;
    const LOCK_VIOLATION: i32 = 33;
    let path = home.join(LOCK);
    let file = match private::file(&path, true) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            private::file(&path, false)
        }
        made => made,
    }
    .map_err(|_| failure("launch_context_unsafe"))?;
    match private::lock(&file) {
        Ok(()) => Ok(file),
        Err(error) if error.raw_os_error() == Some(LOCK_VIOLATION) => {
            Err(failure("launch_home_busy"))
        }
        Err(_) => Err(failure("launch_context_unsafe")),
    }
}
