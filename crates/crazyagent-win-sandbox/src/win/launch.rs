use super::job::Job;
use super::{BoundaryError, Words, last_error, wide, windows};
use crate::quote::command_line;
use crate::request::Request;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use windows_sys::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::{SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES};
use windows_sys::Win32::System::SystemServices::SE_GROUP_ENABLED;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

const STANDARD_HANDLES: usize = 3;
const RESUME_FAILED: u32 = u32::MAX;

/// A started process and its first thread.
struct Child {
    process: OwnedHandle,
    thread: OwnedHandle,
}

/// Starts the program suspended in the container with its capabilities and only the helper's
/// standard handles, puts it in the job, then resumes it and waits for it to exit. A child that
/// cannot join the job or resume is terminated before it runs. Inputs: the request, the
/// container and capability SIDs and the job. Output: the exit code, or why it did not start.
pub(super) fn start(
    request: &Request,
    container: &Words,
    capabilities: &[Words],
    job: &Job,
) -> Result<u32, BoundaryError> {
    let program = wide(&request.program);
    let args: Vec<Vec<u16>> = request
        .args
        .iter()
        .map(|arg| arg.encode_wide().collect())
        .collect();
    let mut line = command_line(&program[..program.len() - 1], &args)
        .map_err(|error| BoundaryError::Refused(error.reason()))?;
    line.push(0);
    let standard = standard_handles();
    let child = spawn_suspended(&program, &mut line, container, capabilities, &standard)
        .map_err(windows("start the agent"))?;
    if let Err(code) = job.assign(&child.process) {
        terminate(&child.process);
        return Err(BoundaryError::Windows {
            step: "put the agent in its job",
            code,
        });
    }
    if let Err(code) = resume(&child.thread) {
        terminate(&child.process);
        return Err(BoundaryError::Windows {
            step: "resume the agent",
            code,
        });
    }
    drop(child.thread);
    wait(&child.process).map_err(windows("wait for the agent"))?;
    exit_code(&child.process).map_err(windows("read the agent's exit code"))
}

fn standard_handles() -> [Option<RawHandle>; STANDARD_HANDLES] {
    let handles = [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ];
    handles.map(|handle| {
        let present = !handle.is_null() && handle != INVALID_HANDLE_VALUE;
        (present && inheritable(handle).is_ok()).then_some(handle)
    })
}

/// Marks one of the helper's own standard handles inheritable so the child can receive it
/// through the handle list. Relies on `handle` being a live standard handle of this process.
fn inheritable(handle: HANDLE) -> Result<(), u32> {
    let set = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    if set == 0 {
        return Err(last_error());
    }
    Ok(())
}

/// A process attribute list, deleted when dropped. Its buffer never moves once initialized.
struct AttributeList {
    buffer: Vec<usize>,
}

impl AttributeList {
    /// Initializes a list with room for `count` attributes. Relies on the first call only
    /// reporting the size, and on the buffer holding at least that many bytes, pointer aligned.
    fn new(count: u32) -> Result<Self, u32> {
        let mut size = 0usize;
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size) };
        if size == 0 {
            return Err(last_error());
        }
        let mut buffer = vec![0usize; size.div_ceil(size_of::<usize>())];
        let initialized = unsafe {
            InitializeProcThreadAttributeList(buffer.as_mut_ptr().cast(), count, 0, &mut size)
        };
        if initialized == 0 {
            return Err(last_error());
        }
        Ok(Self { buffer })
    }
}

impl Drop for AttributeList {
    /// Deletes the list. Relies on it having been initialized and on no process creation using
    /// it any more.
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.buffer.as_mut_ptr().cast()) };
    }
}

/// Creates the child suspended in the AppContainer. Relies on: `program` and `line` being
/// NUL-terminated UTF-16 (the line writable, as `CreateProcessW` may change it); the container
/// and capability SIDs, the capability array, the security capabilities and the handle array
/// being declared before the attribute list, so they stay alive and unmoved until it is
/// deleted; and the handle list naming only live, inheritable, distinct handles.
fn spawn_suspended(
    program: &[u16],
    line: &mut [u16],
    container: &Words,
    capabilities: &[Words],
    standard: &[Option<RawHandle>; STANDARD_HANDLES],
) -> Result<Child, u32> {
    assert_eq!(program.last(), Some(&0));
    assert_eq!(line.last(), Some(&0));
    let mut entries: Vec<SID_AND_ATTRIBUTES> = capabilities
        .iter()
        .map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid.words.as_ptr().cast_mut().cast(),
            Attributes: SE_GROUP_ENABLED as u32,
        })
        .collect();
    let security = SECURITY_CAPABILITIES {
        AppContainerSid: container.words.as_ptr().cast_mut().cast(),
        Capabilities: if entries.is_empty() {
            std::ptr::null_mut()
        } else {
            entries.as_mut_ptr()
        },
        CapabilityCount: entries.len() as u32,
        Reserved: 0,
    };
    let mut inherited: Vec<HANDLE> = Vec::with_capacity(STANDARD_HANDLES);
    for handle in standard.iter().flatten() {
        if !inherited.contains(handle) {
            inherited.push(*handle);
        }
    }
    let mut list = AttributeList::new(if inherited.is_empty() { 1 } else { 2 })?;
    let list_pointer = list.buffer.as_mut_ptr().cast();
    let updated = unsafe {
        UpdateProcThreadAttribute(
            list_pointer,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            (&raw const security).cast(),
            size_of::<SECURITY_CAPABILITIES>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if updated == 0 {
        return Err(last_error());
    }
    if !inherited.is_empty() {
        let updated = unsafe {
            UpdateProcThreadAttribute(
                list_pointer,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                inherited.as_ptr().cast(),
                inherited.len() * size_of::<HANDLE>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if updated == 0 {
            return Err(last_error());
        }
    }
    let null: HANDLE = std::ptr::null_mut();
    let startup = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: size_of::<STARTUPINFOEXW>() as u32,
            lpReserved: null.cast(),
            lpDesktop: null.cast(),
            lpTitle: null.cast(),
            dwX: 0,
            dwY: 0,
            dwXSize: 0,
            dwYSize: 0,
            dwXCountChars: 0,
            dwYCountChars: 0,
            dwFillAttribute: 0,
            dwFlags: STARTF_USESTDHANDLES,
            wShowWindow: 0,
            cbReserved2: 0,
            lpReserved2: null.cast(),
            hStdInput: standard[0].unwrap_or(null),
            hStdOutput: standard[1].unwrap_or(null),
            hStdError: standard[2].unwrap_or(null),
        },
        lpAttributeList: list_pointer,
    };
    let mut info = PROCESS_INFORMATION {
        hProcess: null,
        hThread: null,
        dwProcessId: 0,
        dwThreadId: 0,
    };
    let created = unsafe {
        CreateProcessW(
            program.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            i32::from(!inherited.is_empty()),
            CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT,
            std::ptr::null(),
            std::ptr::null(),
            (&raw const startup).cast(),
            &mut info,
        )
    };
    let error = last_error();
    drop(list);
    if created == 0 {
        return Err(error);
    }
    assert!(!info.hProcess.is_null() && !info.hThread.is_null());
    Ok(unsafe {
        Child {
            process: OwnedHandle::from_raw_handle(info.hProcess),
            thread: OwnedHandle::from_raw_handle(info.hThread),
        }
    })
}

/// Resumes the child's suspended first thread. Relies on `thread` being the live thread handle
/// `CreateProcessW` returned.
fn resume(thread: &OwnedHandle) -> Result<(), u32> {
    if unsafe { ResumeThread(thread.as_raw_handle()) } == RESUME_FAILED {
        return Err(last_error());
    }
    Ok(())
}

/// Ends a child that must not run. Relies on `process` being the live process handle
/// `CreateProcessW` returned; a failure leaves the suspended child to the job or to exit.
fn terminate(process: &OwnedHandle) {
    unsafe { TerminateProcess(process.as_raw_handle(), 1) };
}

/// Waits until the child exits. Relies on `process` being a live process handle.
fn wait(process: &OwnedHandle) -> Result<(), u32> {
    if unsafe { WaitForSingleObject(process.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
        return Err(last_error());
    }
    Ok(())
}

/// Reads the exited child's exit code. Relies on `process` being a live process handle with
/// query rights.
fn exit_code(process: &OwnedHandle) -> Result<u32, u32> {
    let mut code = 0u32;
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
        return Err(last_error());
    }
    Ok(code)
}
