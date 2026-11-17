use super::job::{assign_job, create_job};
use super::{BoundaryError, last_error, windows};
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Child;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

const RESUME_FAILED: u32 = u32::MAX;
const ENDED_EXIT_CODE: u32 = 1;

/// A started process and every process it starts, held in a kill-on-close job of their own
/// that the process cannot leave, ended together when this is dropped or the engine exits.
/// It sets no process limit, so a boundary helper inside it nests its own job freely.
#[derive(Debug)]
pub struct ProcessTree {
    job: OwnedHandle,
}

impl ProcessTree {
    /// Puts a process started with `CREATE_SUSPENDED` in a new kill-on-close job, then
    /// resumes its threads, so nothing it starts ever runs outside the job. Input: the
    /// suspended child. Output: the held tree, or the failed step; the caller kills the child
    /// after a failure, which the job has already ended once the child joined it.
    pub fn hold(child: &Child) -> Result<Self, BoundaryError> {
        let id = child.id();
        assert_ne!(id, 0);
        let process =
            child
                .as_handle()
                .try_clone_to_owned()
                .map_err(|error| BoundaryError::Windows {
                    step: "open the server process",
                    code: error.raw_os_error().map_or(0, |code| code as u32),
                })?;
        let job = create_job().map_err(windows("create the server's job"))?;
        kill_on_close(&job).map_err(windows("limit the server's job"))?;
        assign_job(&job, &process).map_err(windows("put the server in its job"))?;
        let tree = Self { job };
        let resumed = resume_threads(id).map_err(windows("resume the server"))?;
        if resumed == 0 {
            return Err(BoundaryError::Refused("the server had no thread to resume"));
        }
        Ok(tree)
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        unsafe { TerminateJobObject(self.job.as_raw_handle(), ENDED_EXIT_CODE) };
    }
}

/// Sets kill-on-close only. Relies on `job` being a live job handle and on the information
/// struct outliving the call with its exact size passed.
fn kill_on_close(job: &OwnedHandle) -> Result<(), u32> {
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
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

/// Resumes every thread the process owns, found in a snapshot of the system's threads.
/// Relies on the snapshot handle staying open while the entry struct, sized as Windows
/// expects, is filled. Input: the process id, held alive by its open handle. Output: how
/// many threads it resumed, or the Windows error code.
fn resume_threads(id: u32) -> Result<usize, u32> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut resumed = 0;
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != 0;
    while found {
        if entry.th32OwnerProcessID == id {
            resume_thread(entry.th32ThreadID)?;
            resumed += 1;
        }
        found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != 0;
    }
    Ok(resumed)
}

/// Resumes one thread by id. Relies on the opened handle being null on failure or a new
/// handle this caller owns alone.
fn resume_thread(id: u32) -> Result<(), u32> {
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, id) };
    if thread.is_null() {
        return Err(last_error());
    }
    let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
    if unsafe { ResumeThread(thread.as_raw_handle()) } == RESUME_FAILED {
        return Err(last_error());
    }
    Ok(())
}
