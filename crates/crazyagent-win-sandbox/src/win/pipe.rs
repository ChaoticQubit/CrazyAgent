use super::owner::{sid_bytes, token_user};
use super::private::user_sid;
use super::{Words, last_error};
use crate::dacl::{SYSTEM, allowing};
use crate::pipe::Peer;
use std::io;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::{
    ERROR_NO_TOKEN, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR, RevertToSelf,
    SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, SetSecurityDescriptorOwner, TOKEN_INFORMATION_CLASS,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel, TokenIsAppContainer,
    TokenRestrictedSids,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, ImpersonateNamedPipeClient, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

/// The local pipe namespace every engine pipe name starts with.
const PREFIX: &str = r"\\.\pipe\";
/// The longest pipe name Windows takes, in characters, prefix included.
const NAME_MAX: usize = 256;
/// The buffer size each way, as Tokio's named pipe server uses.
const BUFFER_BYTES: u32 = 65_536;
/// The most instances one name may allow below Windows' unlimited value.
pub const INSTANCES_MAX: u32 = 254;
/// The most bytes a token's restricting SID list may take.
const RESTRICTED_BYTES_MAX: usize = 64 * 1024;
/// The most bytes a token's integrity label may take: the label and one SID.
const LABEL_BYTES_MAX: usize = 256;
/// The authority of an integrity level SID, S-1-16.
const LABEL_AUTHORITY: [u8; 6] = [0, 0, 0, 0, 0, 16];

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

/// Reads who is at the client end of a connected engine pipe instance (DESIGN-0140 D9 step
/// 1, NOTE-0145 A2): impersonates the client, opens the thread token as itself and reverts,
/// with nothing that can panic in between; a failed revert aborts the process. Synchronous,
/// for a blocking thread. Input: the server end, after the client's first bytes were read.
/// Output: the peer, or the Windows error.
pub fn client(pipe: BorrowedHandle<'_>) -> io::Result<Peer> {
    if unsafe { ImpersonateNamedPipeClient(pipe.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut token: HANDLE = std::ptr::null_mut();
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    let failure = last_error();
    if unsafe { RevertToSelf() } == 0 {
        std::process::abort();
    }
    assert!(thread_token_gone(), "the peer check leaves no thread token");
    if opened == 0 {
        return Err(io::Error::from_raw_os_error(failure as i32));
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    peer_of(&token)
}

/// Whether this thread acts as the process again, holding no token of its own. Output:
/// true when opening the thread token fails with ERROR_NO_TOKEN.
fn thread_token_gone() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } != 0 {
        drop(unsafe { OwnedHandle::from_raw_handle(token) });
        return false;
    }
    last_error() == ERROR_NO_TOKEN
}

/// What a token says about its holder for D9 and D10: its user SID, whether it is an
/// AppContainer token, whether it has restricting SIDs (what `IsTokenRestricted` reports,
/// read so that a failed read refuses) and its integrity level. Input: a token opened with
/// `TOKEN_QUERY`. Output: the peer, or the Windows error.
fn peer_of(token: &OwnedHandle) -> io::Result<Peer> {
    let user = token_user(token).map_err(|code| io::Error::from_raw_os_error(code as i32))?;
    let mut container = 0_u32;
    let mut needed = 0_u32;
    let read = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenIsAppContainer,
            (&raw mut container).cast(),
            size_of::<u32>() as u32,
            &mut needed,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    let restricting = information(
        token,
        TokenRestrictedSids,
        size_of::<u32>(),
        RESTRICTED_BYTES_MAX,
    )?;
    Ok(Peer {
        user,
        app_container: container != 0,
        restricted: unsafe { *restricting.as_ptr().cast::<u32>() } != 0,
        integrity: integrity_of(token)?,
    })
}

/// A token's integrity level: the last part of its mandatory label SID. Input: a token
/// opened with `TOKEN_QUERY`. Output: the level, or why it could not be read.
fn integrity_of(token: &OwnedHandle) -> io::Result<u32> {
    let label = information(
        token,
        TokenIntegrityLevel,
        size_of::<TOKEN_MANDATORY_LABEL>(),
        LABEL_BYTES_MAX,
    )?;
    let label = unsafe { &*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>() };
    let sid =
        sid_bytes(label.Label.Sid).map_err(|code| io::Error::from_raw_os_error(code as i32))?;
    if sid.len() != 12 || sid[1] != 1 || sid[2..8] != LABEL_AUTHORITY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a token's integrity label is not one integrity level",
        ));
    }
    Ok(u32::from_le_bytes([sid[8], sid[9], sid[10], sid[11]]))
}

/// One token information class read into a buffer aligned for any structure it holds.
/// Inputs: a token opened with `TOKEN_QUERY`, the class, and the fewest and most bytes the
/// answer may take. Output: the buffer, or why it could not be read.
fn information(
    token: &OwnedHandle,
    class: TOKEN_INFORMATION_CLASS,
    least: usize,
    most: usize,
) -> io::Result<Vec<u64>> {
    assert!(0 < least && least <= most);
    let mut needed = 0_u32;
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if !(least..=most).contains(&(needed as usize)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a token answer is out of bounds",
        ));
    }
    let mut buffer = vec![0_u64; (needed as usize).div_ceil(size_of::<u64>())];
    let read = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}
