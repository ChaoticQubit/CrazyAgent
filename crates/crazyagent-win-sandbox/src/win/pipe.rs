use super::Words;
use super::private::user_sid;
use crate::dacl::{SYSTEM, allowing};
use std::io;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{
    InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
    SECURITY_DESCRIPTOR, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
    SetSecurityDescriptorOwner,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;

/// The local pipe namespace every engine pipe name starts with.
const PREFIX: &str = r"\\.\pipe\";
/// The longest pipe name Windows takes, in characters, prefix included.
const NAME_MAX: usize = 256;
/// The buffer size each way, as Tokio's named pipe server uses.
const BUFFER_BYTES: u32 = 65_536;
/// The most instances one name may allow below Windows' unlimited value.
pub const INSTANCES_MAX: u32 = 254;

/// Creates one instance of the local named pipe `name` for the engine (DESIGN-0256 D8):
/// byte mode, overlapped for Tokio, remote clients refused, at most `instances` instances
/// of the name, and a protected list giving read and write to the user and SYSTEM only,
/// with the user as owner. With `first` the create fails with access denied when any
/// instance of the name exists, so a name someone else made first is never joined.
/// Inputs: the full name under `\\.\pipe\` with one plain part, whether this is the first
/// instance and the instance cap. Output: the server handle, or the Windows error.
pub fn create(name: &str, first: bool, instances: u32) -> io::Result<OwnedHandle> {
    let part = name.strip_prefix(PREFIX).unwrap_or_default();
    if part.is_empty() || name.len() > NAME_MAX || part.contains(['\\', '/', '\0']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a pipe name is one plain part under \\\\.\\pipe\\",
        ));
    }
    if !(1..=INSTANCES_MAX).contains(&instances) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a pipe allows from 1 to 254 instances",
        ));
    }
    let user = user_sid()?;
    let mut owner = Words::from_bytes(&user);
    let list = Words::from_bytes(&allowing(
        &[user.as_slice(), &SYSTEM[..]],
        0,
        GENERIC_READ | GENERIC_WRITE,
    ));
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    let pointer: PSECURITY_DESCRIPTOR = (&raw mut descriptor).cast();
    let built = unsafe {
        InitializeSecurityDescriptor(pointer, SECURITY_DESCRIPTOR_REVISION) != 0
            && SetSecurityDescriptorOwner(pointer, owner.words.as_mut_ptr().cast(), 0) != 0
            && SetSecurityDescriptorDacl(pointer, 1, list.words.as_ptr().cast(), 0) != 0
            && SetSecurityDescriptorControl(pointer, SE_DACL_PROTECTED, SE_DACL_PROTECTED) != 0
    };
    if !built {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: pointer.cast(),
        bInheritHandle: 0,
    };
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut open = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            open,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            instances,
            BUFFER_BYTES,
            BUFFER_BYTES,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}
