use super::acl::{ACL_BYTES_MAX, Local};
use super::owner::{sid_bytes, user_sid as token_user};
use super::{Words, final_name, last_error};
use crate::private::{Facts, judge, list};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_LOCK_VIOLATION, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, InitializeSecurityDescriptor, LABEL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
    SECURITY_DESCRIPTOR, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
    SetSecurityDescriptorOwner,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateDirectoryW, CreateFileW,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
    FILE_SHARE_WRITE, GetFileInformationByHandle, LOCKFILE_EXCLUSIVE_LOCK,
    LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, OPEN_EXISTING, READ_CONTROL, UnlockFileEx,
};
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;

const REMOTE_PREFIX: &str = r"\Device\Mup\";

/// The engine's own user SID, read from its process token. Output: the SID bytes, or the
/// Windows error.
pub fn user_sid() -> io::Result<Vec<u8>> {
    token_user().map_err(code)
}

/// Opens the private folder at `path`, creating it first when it is missing with the user
/// as owner and the protected list of [`list`], so no entry from the folder above is ever on
/// it. The handle lists the folder, so Windows counts it in sharing, and does not share
/// delete, so the folder cannot be renamed while it is held. Input: the absolute path.
/// Output: the held folder, or why it is not private.
pub fn dir(path: &Path) -> io::Result<File> {
    let user = user_sid()?;
    let name = wide_path(path)?;
    let mut planned = Planned::new(&user, true)?;
    let attributes = planned.attributes();
    if unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } == 0 {
        let error = last_error();
        if error != ERROR_ALREADY_EXISTS {
            return Err(code(error));
        }
    }
    let folder = open(
        &name,
        FILE_LIST_DIRECTORY | READ_CONTROL | FILE_READ_ATTRIBUTES,
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS,
        None,
    )?;
    private_as(&folder, &user, true)?;
    Ok(folder)
}

/// Opens the private file at `path` for reading and writing, never following a link: with
/// `create` it must not exist yet and is made with the user as owner and the protected list
/// of [`list`]; without it, it must exist. Inputs: the absolute path and whether to create.
/// Output: the open file, `AlreadyExists` on a second create, or why it is not private.
pub fn file(path: &Path, create: bool) -> io::Result<File> {
    let user = user_sid()?;
    let name = wide_path(path)?;
    let mut planned = if create {
        Some(Planned::new(&user, false)?)
    } else {
        None
    };
    let disposition = if create { CREATE_NEW } else { OPEN_EXISTING };
    let opened = open(
        &name,
        GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
        disposition,
        0,
        planned.as_mut(),
    )?;
    private_as(&opened, &user, false)?;
    Ok(opened)
}

/// Reads what W1's check judges from an open folder or file. Input: the handle. Output: the
/// facts, or the Windows error.
pub fn facts(object: &File) -> io::Result<Facts> {
    read(object).map(|(facts, _)| facts)
}

/// Refuses an open folder or file that is not private to the engine's user ([`judge`]),
/// the check every store runs on what it opens. Input: the handle. Output: Ok, or a
/// `PermissionDenied` error with the reason.
pub fn check(object: &File) -> io::Result<()> {
    let user = user_sid()?;
    let (facts, _) = read(object)?;
    judge(&facts, &user).map_err(refused)
}

/// Takes the exclusive lock on the whole of an open file at once, or fails at once when any
/// other handle holds a lock on it, the twin of a non-blocking exclusive `flock`. Input: the
/// file. Output: Ok, or the Windows error (`ERROR_LOCK_VIOLATION` while another holds it).
pub fn lock(file: &File) -> io::Result<()> {
    take(file, LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY)
}

/// Releases a lock this handle took on the whole file, so it ends now and not when Windows
/// gets round to it after the handle closes. Input: the file. Output: Ok, or the Windows
/// error.
pub fn unlock(file: &File) -> io::Result<()> {
    let mut overlapped = OVERLAPPED::default();
    let released =
        unsafe { UnlockFileEx(file.as_raw_handle(), 0, u32::MAX, u32::MAX, &mut overlapped) };
    if released == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Says whether no handle holds an exclusive lock on the file, the twin of the shared probe
/// `engine_lock_released` takes: a shared lock taken at once and released at once, so the
/// probe never keeps a lock of its own. Input: the file. Output: the answer, or the error.
pub fn free(file: &File) -> io::Result<bool> {
    match take(file, LOCKFILE_FAIL_IMMEDIATELY) {
        Ok(()) => unlock(file).map(|()| true),
        Err(error) if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Locks the whole file with `flags`. Relies on the handle staying open for the call and on
/// the zeroed overlapped block naming offset 0 for this synchronous handle. Inputs: the file
/// and the lock flags. Output: Ok, or the Windows error.
fn take(file: &File, flags: u32) -> io::Result<()> {
    let mut overlapped = OVERLAPPED::default();
    let taken = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            flags,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if taken == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Refuses an opened object that is a folder where a file is wanted or the reverse, or that
/// [`judge`] does not find private. Inputs: the handle, the user's SID and whether a folder
/// is wanted. Output: Ok, or a `PermissionDenied` error with the reason.
fn private_as(object: &File, user: &[u8], folder: bool) -> io::Result<()> {
    let (facts, attributes) = read(object)?;
    if (attributes & FILE_ATTRIBUTE_DIRECTORY != 0) != folder {
        return Err(refused(if folder {
            "a private folder is not a folder"
        } else {
            "a private file is a folder"
        }));
    }
    judge(&facts, user).map_err(refused)
}

/// Reads an open object's owner, lists, attributes, link count and real place. Relies on the
/// handle staying open for every call. Input: the handle. Output: the facts and the raw
/// attributes, or the Windows error.
fn read(object: &File) -> io::Result<(Facts, u32)> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(object.as_raw_handle(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let Security { owner, list, label } = security(object)?;
    let place = String::from_utf16_lossy(&final_name(object)?);
    let remote = place
        .get(..REMOTE_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(REMOTE_PREFIX));
    let attributes = information.dwFileAttributes;
    let facts = Facts {
        owner,
        list,
        label,
        reparse: attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        links: information.nNumberOfLinks,
        remote,
    };
    Ok((facts, attributes))
}

/// What [`security`] reads from one object: its owner SID, its access list and its system
/// list holding the mandatory label, each list None when the object has none.
struct Security {
    owner: Vec<u8>,
    list: Option<Vec<u8>>,
    label: Option<Vec<u8>>,
}

/// Reads the owner SID, the access list and the mandatory label list of an open object.
/// Relies on the handle staying open for the call, and on the owner and lists pointing into
/// the descriptor the call allocated, which stays alive until all three are copied. Input:
/// the handle. Output: what was read, or the error.
fn security(object: &File) -> io::Result<Security> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            object.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            &mut sacl,
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(code(status));
    }
    let descriptor = Local(descriptor);
    let owner = sid_bytes(owner).map_err(code)?;
    let list = unsafe { copied(dacl) };
    let label = unsafe { copied(sacl) };
    drop(descriptor);
    Ok(Security { owner, list, label })
}

/// Copies an access list Windows returned, or none for a null pointer.
/// Safety: `list`, when not null, must point at a list whose size field is right, inside
/// memory that stays alive for the call.
unsafe fn copied(list: *const ACL) -> Option<Vec<u8>> {
    if list.is_null() {
        return None;
    }
    let size = usize::from(unsafe { (*list).AclSize });
    assert!((8..=ACL_BYTES_MAX).contains(&size));
    Some(unsafe { std::slice::from_raw_parts(list.cast::<u8>(), size) }.to_vec())
}

/// Opens `name` with a share mode that never includes delete and the link flag, so a link
/// or junction there is opened itself, never what it leads to. Relies on `name` being
/// NUL-terminated UTF-16 and the descriptor, when given, outliving the call. Inputs: the
/// name, access, disposition, extra flags and the descriptor to create with.
fn open(
    name: &[u16],
    access: u32,
    disposition: u32,
    flags: u32,
    planned: Option<&mut Planned>,
) -> io::Result<File> {
    assert_eq!(name.last(), Some(&0));
    let attributes = planned.map(Planned::attributes);
    let pointer = attributes
        .as_ref()
        .map_or(std::ptr::null(), std::ptr::from_ref);
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            pointer,
            disposition,
            flags | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// The security descriptor a private folder or file is created with: the user as owner and
/// the protected list of [`list`]. The owner and list live in their own buffers, which do
/// not move when this value does, so the descriptor's pointers stay valid.
struct Planned {
    owner: Words,
    list: Words,
    descriptor: SECURITY_DESCRIPTOR,
}

impl Planned {
    /// Builds the descriptor. Inputs: the user's SID and whether it is for a folder. Output:
    /// the descriptor, or the Windows error.
    fn new(user: &[u8], folder: bool) -> io::Result<Self> {
        let mut planned = Self {
            owner: Words::from_bytes(user),
            list: Words::from_bytes(&list(user, folder)),
            descriptor: SECURITY_DESCRIPTOR::default(),
        };
        let descriptor: PSECURITY_DESCRIPTOR = (&raw mut planned.descriptor).cast();
        let owner: PSID = planned.owner.words.as_mut_ptr().cast();
        let dacl: *const ACL = planned.list.words.as_ptr().cast();
        let built = unsafe {
            InitializeSecurityDescriptor(descriptor, SECURITY_DESCRIPTOR_REVISION) != 0
                && SetSecurityDescriptorOwner(descriptor, owner, 0) != 0
                && SetSecurityDescriptorDacl(descriptor, 1, dacl, 0) != 0
                && SetSecurityDescriptorControl(descriptor, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
                    != 0
        };
        if !built {
            return Err(io::Error::last_os_error());
        }
        Ok(planned)
    }

    /// The attributes that hand this descriptor to a create call, with no handle
    /// inheritance. The value points into `self`, which must not move until the call ends.
    fn attributes(&mut self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: (&raw mut self.descriptor).cast(),
            bInheritHandle: 0,
        }
    }
}

/// The path as NUL-terminated UTF-16, refused unless it is absolute with no zero inside, so
/// a root-relative or drive-relative name never resolves against the current drive or
/// folder. Input: the path. Output: the units, or `InvalidInput`.
fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    if !path.is_absolute() || wide[..wide.len() - 1].contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a private path must be absolute",
        ));
    }
    Ok(wide)
}

fn code(error: u32) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
}

fn refused(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}
