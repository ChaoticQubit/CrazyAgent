use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use windows_sys::Win32::Foundation::{
    GetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType};
use windows_sys::Win32::System::Console::{STD_INPUT_HANDLE, SetStdHandle};

/// Takes this process's standard input when it is a pipe, as the sign-in custodian's token
/// pipe is: its inherit flag is cleared and NUL becomes the standard input, so no child gets
/// the pipe, as an extra handle or through `Stdio::inherit`. It must run before anything
/// reads standard input, since std buffers `Stdin` and bytes read before the swap stay there.
/// Output: the pipe, owned by the caller alone; none when stdin is absent or not a pipe.
pub fn take_stdin_pipe() -> io::Result<Option<File>> {
    let handle = io::stdin().as_raw_handle();
    if !present(handle) || unsafe { GetFileType(handle) } != FILE_TYPE_PIPE {
        return Ok(None);
    }
    not_inherited(handle)?;
    let nul = File::open("NUL")?;
    if unsafe { SetStdHandle(STD_INPUT_HANDLE, nul.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    std::mem::forget(nul);
    assert_ne!(io::stdin().as_raw_handle(), handle);
    Ok(Some(unsafe { File::from_raw_handle(handle) }))
}

/// Clears the inherit flag on this process's standard output and standard error, so a child
/// gets either only through a `Stdio` its spawn names: std starts every child with handle
/// inheritance on (rv-sec2 #728 C1, path (a)). Output: Ok, also for an absent handle, or the
/// Windows error.
pub fn output_not_inherited() -> io::Result<()> {
    for handle in [io::stdout().as_raw_handle(), io::stderr().as_raw_handle()] {
        if present(handle) {
            not_inherited(handle)?;
        }
    }
    Ok(())
}

/// Whether a standard handle names an object: neither null nor the invalid value.
fn present(handle: RawHandle) -> bool {
    !handle.is_null() && handle != INVALID_HANDLE_VALUE
}

/// Clears one handle's inherit flag and checks that it is clear. Relies on `handle` being a
/// live handle of this process. Output: Ok, or the Windows error.
fn not_inherited(handle: RawHandle) -> io::Result<()> {
    if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut flags = 0;
    assert!(unsafe { GetHandleInformation(handle, &mut flags) } != 0);
    assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
    Ok(())
}
