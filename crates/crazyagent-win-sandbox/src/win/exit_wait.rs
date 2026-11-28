use std::os::windows::io::AsRawHandle;
use std::time::Instant;
use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::WaitForSingleObject;

/// The longest one `WaitForSingleObject` call waits, an hour, well below `INFINITE`, so a
/// far deadline is waited in parts and never turns into a wait without end.
const CALL_WAIT_MAX_MS: u32 = 3_600_000;

/// Waits until the process `child` holds has exited or `deadline` passes, without reaping
/// it: the caller reaps it with `Child::wait`. The handle the child holds keeps the process
/// its own, so no other process is ever waited on. Inputs: the child and the instant to
/// stop at. Output: true once it exited, false at the deadline, or the wait error.
pub fn wait_for_exit(child: &std::process::Child, deadline: Instant) -> std::io::Result<bool> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let millis = left.as_nanos().div_ceil(1_000_000);
        let millis = u32::try_from(millis).map_or(CALL_WAIT_MAX_MS, |ms| ms.min(CALL_WAIT_MAX_MS));
        assert!(millis <= CALL_WAIT_MAX_MS);
        match unsafe { WaitForSingleObject(child.as_raw_handle(), millis) } {
            WAIT_OBJECT_0 => return Ok(true),
            WAIT_TIMEOUT if Instant::now() >= deadline => return Ok(false),
            WAIT_TIMEOUT => {}
            _ => return Err(std::io::Error::last_os_error()),
        }
    }
}
