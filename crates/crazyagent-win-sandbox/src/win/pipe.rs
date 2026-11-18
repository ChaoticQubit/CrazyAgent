use super::acl::{ACL_BYTES_MAX, Local};
use super::owner::{sid_bytes, token_user};
use super::private::user_sid;
use super::{Words, last_error};
use crate::dacl::{SYSTEM, allowing, label};
use crate::pipe::{NO_TOKEN, Peer, Pin, Reverted, Served, after_revert};
use std::io;
use std::os::windows::io::{
    AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, IntoRawHandle, OwnedHandle,
};
use tokio::net::windows::named_pipe::NamedPipeServer;
use windows_sys::Win32::Foundation::{
    ERROR_NO_TOKEN, ERROR_SUCCESS, FILETIME, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
use windows_sys::Win32::Security::{
    ACL, GetTokenInformation, InitializeSecurityDescriptor, LABEL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, RevertToSelf, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
    SECURITY_DESCRIPTOR, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
    SetSecurityDescriptorOwner, TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
    TokenIntegrityLevel, TokenIsAppContainer, TokenRestrictedSids,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, GetNamedPipeServerProcessId, ImpersonateNamedPipeClient, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, GetProcessId, GetProcessTimes, OpenProcess,
    OpenProcessToken, OpenThreadToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

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
const _: () = assert!(ERROR_NO_TOKEN == NO_TOKEN);

/// Creates one instance of the local named pipe `name` for the engine (DESIGN-0256 D8): byte
/// mode, overlapped, remote clients refused, at most `instances` of the name, and a protected
/// list, owned by the user, giving read and write to the user and SYSTEM only. With `first`,
/// a name that already exists fails with access denied and is never joined. Inputs: the name
/// under `\\.\pipe\` (one plain part), first or not, the cap. Output: the handle, or the error.
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

/// Creates one instance of an engine pipe as [`create`] does and hands it to Tokio, whose
/// reactor it joins (DESIGN-0256 D8), so the engine's listener needs no unsafe code. Must
/// run inside a Tokio runtime. Inputs: as for [`create`]. Output: the server end, or the
/// Windows error.
pub fn instance(name: &str, first: bool, instances: u32) -> io::Result<NamedPipeServer> {
    let created = create(name, first, instances)?;
    unsafe { NamedPipeServer::from_raw_handle(created.into_raw_handle()) }
}

/// Reads who is at the client end of a connected engine pipe instance (DESIGN-0140 D9 step
/// 1, NOTE-0145 A2): impersonates the client, opens the thread token as itself and reverts,
/// with nothing that can panic in between. A failed revert, or a thread token still held
/// after it, aborts the process; a probe that cannot tell refuses the check (#551 F1).
/// Synchronous, for a blocking thread. Input: the server end. Output: the peer, or the error.
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
    let token = (opened != 0).then(|| unsafe { OwnedHandle::from_raw_handle(token) });
    match left_after_revert() {
        Reverted::Held => std::process::abort(),
        Reverted::Unread(error) => return Err(io::Error::from_raw_os_error(error as i32)),
        Reverted::Clean => {}
    }
    let token = token.ok_or_else(|| io::Error::from_raw_os_error(failure as i32))?;
    peer_of(&token)
}

/// Probes this thread's own token once the check has reverted, closing any token it opens.
/// Output: what [`crate::pipe::after_revert`] makes of the probe.
fn left_after_revert() -> Reverted {
    let mut token: HANDLE = std::ptr::null_mut();
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } != 0;
    let error = if opened { 0 } else { last_error() };
    if opened {
        drop(unsafe { OwnedHandle::from_raw_handle(token) });
    }
    after_revert(opened, error)
}

/// Reads who serves a connected engine pipe, before the client writes a byte (DESIGN-0256
/// D10): the server's process ID off the pipe, that process opened for limited queries and
/// pinned by its creation time, its token, and the pipe's integrity label. Input: the client
/// end. Output: what [`crate::pipe::judge_server`] judges, or the Windows error.
pub fn server(pipe: BorrowedHandle<'_>) -> io::Result<Served> {
    let mut pid = 0_u32;
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let pin = pin_of(process.as_handle())?;
    assert_eq!(pin.pid, pid, "the opened process is the one the pipe named");
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    Ok(Served {
        pin,
        peer: peer_of(&token)?,
        label: label_of(pipe)?,
    })
}

/// Pins a process by its ID and creation time (DESIGN-0256 D10 step 1), the engine its own
/// child or itself. Input: a process handle with at least limited query access. Output: the
/// pin, or the Windows error.
pub fn pin_of(process: BorrowedHandle<'_>) -> io::Result<Pin> {
    let pid = unsafe { GetProcessId(process.as_raw_handle()) };
    if pid == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut times = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    if unsafe { GetProcessTimes(process.as_raw_handle(), created, exited, kernel, user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let created = (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime);
    assert_ne!(created, 0, "a running process has a creation time");
    Ok(Pin { pid, created })
}

/// Pins this process, the engine serving its own pipes, for the D7 pipes file its clients
/// read the expected server from (DESIGN-0256 D10 step 1). Output: the pin, or the error.
pub fn own_pin() -> io::Result<Pin> {
    let pin = pin_of(unsafe { BorrowedHandle::borrow_raw(GetCurrentProcess()) })?;
    assert_eq!(pin.pid, std::process::id());
    Ok(pin)
}

/// The integrity level of a pipe's mandatory label (DESIGN-0256 D10 step 3). Input: a
/// handle to the pipe with read-control access. Output: the level, None when the pipe
/// carries no label, or why the label could not be read.
fn label_of(pipe: BorrowedHandle<'_>) -> io::Result<Option<u32>> {
    let mut sacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            pipe.as_raw_handle(),
            SE_KERNEL_OBJECT,
            LABEL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sacl,
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = Local(descriptor);
    let level = if sacl.is_null() {
        None
    } else {
        let size = usize::from(unsafe { (*sacl).AclSize });
        assert!((8..=ACL_BYTES_MAX).contains(&size));
        let list = unsafe { std::slice::from_raw_parts(sacl.cast::<u8>(), size) };
        label(list)
            .map_err(|reason| io::Error::new(io::ErrorKind::InvalidData, reason))?
            .map(|(level, _)| level)
    };
    drop(descriptor);
    Ok(level)
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
