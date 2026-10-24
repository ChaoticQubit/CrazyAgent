use super::{BoundaryError, last_error, windows};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};

/// A job object only the helper holds: every process in it dies when the helper exits or is
/// killed, children cannot break away, and at most the profile's number run at once.
pub(super) struct Job {
    handle: OwnedHandle,
}

impl Job {
    /// Creates the job with kill-on-close and the active process limit. Input: 1 to 4096
    /// processes. Output: the job, or the failed step.
    pub(super) fn new(active_processes: u32) -> Result<Self, BoundaryError> {
        assert!(active_processes >= 1);
        let handle = create_job().map_err(windows("create the agent's job"))?;
        limit_job(&handle, active_processes).map_err(windows("limit the agent's job"))?;
        Ok(Self { handle })
    }

    /// Puts a started, still suspended process in the job. Input: the process handle.
    /// Output: Ok, or the Windows error code.
    pub(super) fn assign(&self, process: &OwnedHandle) -> Result<(), u32> {
        assign_job(&self.handle, process)
    }
}

/// Creates an unnamed job object with default security. Relies on the returned handle being
/// either null on failure or a new handle this caller owns alone.
fn create_job() -> Result<OwnedHandle, u32> {
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(last_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// Sets kill-on-close, die on unhandled exception and the active process limit. Relies on
/// `job` being a live job handle and on the information struct outliving the call with its
/// exact size passed.
fn limit_job(job: &OwnedHandle, active_processes: u32) -> Result<(), u32> {
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    info.BasicLimitInformation.ActiveProcessLimit = active_processes;
    let set = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const info).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if set == 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Assigns a process to the job. Relies on both handles being live for the call, the process
/// handle carrying the rights `CreateProcessW` gives its caller.
fn assign_job(job: &OwnedHandle, process: &OwnedHandle) -> Result<(), u32> {
    let assigned =
        unsafe { AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) };
    if assigned == 0 {
        return Err(last_error());
    }
    Ok(())
}
