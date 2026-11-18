use super::acl::{ACL_BYTES_MAX, Local};
use super::owner::{sid_bytes, user_sid as token_user};
use super::{Words, final_name, last_error};
use crate::dacl::{inherited_label, label, trusted_owner};
use crate::private::{Facts, Store, judge, judge_folder, label_list, list, secret_file_label};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_LOCK_VIOLATION, ERROR_NO_MORE_FILES, ERROR_SUCCESS, GENERIC_READ,
    GENERIC_WRITE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, InitializeSecurityDescriptor, LABEL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, SetSecurityDescriptorOwner, SetSecurityDescriptorSacl,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateDirectoryW, CreateFileW,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_FULL_DIR_INFO, FILE_ID_INFO, FILE_LIST_DIRECTORY,
    FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FileFullDirectoryInfo,
    FileFullDirectoryRestartInfo, FileIdInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    OPEN_EXISTING, READ_CONTROL, UnlockFileEx, WRITE_DAC, WRITE_OWNER,
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
    made_dir(path, None)
}

/// Opens a secret store folder or the database folder at `path` as [`dir`] does, with that
/// store's label of [`label_list`]: a new folder is made with it, and one already there
/// takes it once judged private, whatever made it first (#524 r3, leader 06:32). Inputs: the
/// absolute path and the store. Output: the held folder, or why it is not private.
pub fn store_dir(path: &Path, store: Store) -> io::Result<File> {
    made_dir(path, Some(store))
}

/// The work of [`dir`] and [`store_dir`]: creates the folder when missing, with the store's
/// label when one is named, then opens and judges it, and gives a judged store folder its
/// label. Inputs: the absolute path and the store, if any. Output: the held folder, or why
/// it is not private.
fn made_dir(path: &Path, store: Option<Store>) -> io::Result<File> {
    let user = user_sid()?;
    let name = wide_path(path)?;
    let mut planned = Planned::new(&user, true, store.map(label_list))?;
    let attributes = planned.attributes();
    if unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } == 0 {
        let error = last_error();
        if error != ERROR_ALREADY_EXISTS {
            return Err(code(error));
        }
    }
    let relabel = if store.is_some() { WRITE_OWNER } else { 0 };
    let folder = open(
        &name,
        FILE_LIST_DIRECTORY | READ_CONTROL | FILE_READ_ATTRIBUTES | relabel,
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS,
        None,
    )?;
    private_as(&folder, &user, true)?;
    if let Some(store) = store {
        store_labelled(&folder, store)?;
    }
    Ok(folder)
}

/// Gives a store folder its store's label when the one it carries reads otherwise, as on a
/// folder made before the store was labelled. Called only on a folder already judged private;
/// medium is the engine's own level, so no privilege is needed, and only the label part of
/// the system list changes. Inputs: the held folder, opened with WRITE_OWNER, and the store.
/// Output: Ok, or the Windows error.
fn store_labelled(folder: &File, store: Store) -> io::Result<()> {
    let wanted = label_list(store);
    let readings = |sacl: &[u8]| (label(sacl), inherited_label(sacl));
    let held = security(folder)?.label;
    if held.as_deref().map(readings) == Some(readings(wanted.as_slice())) {
        return Ok(());
    }
    let words = Words::from_bytes(&wanted);
    let status = unsafe {
        SetSecurityInfo(
            folder.as_raw_handle(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
            words.words.as_ptr().cast(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(code(status));
    }
    Ok(())
}

/// Makes an empty folder of this user private again, the twin of a Unix chmod to 0700: opened
/// as itself, it must be local, trusted-owned and empty, as files in it would keep their own
/// lists; nothing changes unless it is. Its list becomes [`list`]'s, judged as [`dir`]
/// judges. Input: the absolute path. Output: the held folder, or why it was refused.
pub fn repair(path: &Path) -> io::Result<File> {
    let user = user_sid()?;
    let name = wide_path(path)?;
    let folder = open(
        &name,
        FILE_LIST_DIRECTORY | READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES,
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS,
        None,
    )?;
    let (facts, attributes) = read(&folder)?;
    if attributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || facts.reparse
        || facts.remote
        || !trusted_owner(&facts.owner, &user)
    {
        return Err(refused("only a local folder of this user is made private"));
    }
    if holds_entries(&folder)? {
        return Err(refused(
            "a folder that already holds files is not made private, as they keep their own lists",
        ));
    }
    let wanted = Words::from_bytes(&list(&user, true));
    let status = unsafe {
        SetSecurityInfo(
            folder.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            wanted.words.as_ptr().cast(),
            std::ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(code(status));
    }
    private_as(&folder, &user, true)?;
    if holds_entries(&folder)? {
        return Err(refused(
            "a file appeared in a folder while it was made private",
        ));
    }
    Ok(folder)
}

/// Whether an open folder holds any entry other than `.` and `..`, listed from its handle so
/// no path is followed. Relies on the handle listing the folder and on Windows writing whole
/// records inside the buffer, each at an offset it names. Input: the folder. Output: the
/// answer, or the Windows error.
fn holds_entries(folder: &File) -> io::Result<bool> {
    const DOT: u16 = b'.' as u16;
    let mut buffer = vec![0u64; 1024];
    let bytes = buffer.len() * size_of::<u64>();
    let next_at = std::mem::offset_of!(FILE_FULL_DIR_INFO, NextEntryOffset);
    let length_at = std::mem::offset_of!(FILE_FULL_DIR_INFO, FileNameLength);
    let name_at = std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName);
    let mut class = FileFullDirectoryRestartInfo;
    loop {
        let listed = unsafe {
            GetFileInformationByHandleEx(
                folder.as_raw_handle(),
                class,
                buffer.as_mut_ptr().cast(),
                bytes as u32,
            )
        };
        if listed == 0 {
            let error = last_error();
            return if error == ERROR_NO_MORE_FILES {
                Ok(false)
            } else {
                Err(code(error))
            };
        }
        class = FileFullDirectoryInfo;
        let base = buffer.as_ptr().cast::<u8>();
        let mut at = 0;
        loop {
            assert!(at + name_at <= bytes);
            let next = unsafe { base.add(at + next_at).cast::<u32>().read_unaligned() } as usize;
            let length = unsafe { base.add(at + length_at).cast::<u32>().read_unaligned() };
            let units = length as usize / 2;
            assert!(at + name_at + units * 2 <= bytes);
            let unit = |index: usize| unsafe {
                base.add(at + name_at + index * 2)
                    .cast::<u16>()
                    .read_unaligned()
            };
            if !matches!(units, 1 | 2) || (0..units).any(|index| unit(index) != DOT) {
                return Ok(true);
            }
            if next == 0 {
                break;
            }
            at += next;
        }
    }
}

/// Opens the private file at `path` for reading and writing, never following a link: with
/// `create` it must not exist yet and is made with the user as owner and the protected list
/// of [`list`]; without it, it must exist. Inputs: the absolute path and whether to create.
/// Output: the open file, `AlreadyExists` on a second create, or why it is not private.
pub fn file(path: &Path, create: bool) -> io::Result<File> {
    let user = user_sid()?;
    let name = wide_path(path)?;
    let mut planned = if create {
        Some(Planned::new(&user, false, None)?)
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

/// Creates the private file at `path` as [`file`] does with `create`, carrying from creation
/// the medium no-write-up no-read-up label of [`secret_file_label`] on the file itself, so
/// in any folder no lower integrity process reads or holds it (rv-sec3 #555 N1). Input: the
/// absolute path, which must not exist. Output: the open file, or why it is not private.
pub fn secret_file(path: &Path) -> io::Result<File> {
    let user = user_sid()?;
    let name = wide_path(path)?;
    let mut planned = Planned::new(&user, false, Some(secret_file_label()))?;
    let opened = open(
        &name,
        GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
        CREATE_NEW,
        0,
        Some(&mut planned),
    )?;
    private_as(&opened, &user, false)?;
    Ok(opened)
}

/// Opens the private file at `path` for reading only, never following a link, and refuses it
/// unless [`judge`] finds it private. Like [`file`] it never shares delete, so the file
/// cannot be renamed or replaced while it is read. Input: the absolute path. Output: the open
/// file, or why it is not private.
pub fn read_only(path: &Path) -> io::Result<File> {
    let user = user_sid()?;
    let name = wide_path(path)?;
    let opened = open(&name, GENERIC_READ | READ_CONTROL, OPEN_EXISTING, 0, None)?;
    private_as(&opened, &user, false)?;
    Ok(opened)
}

/// The identity a store pins for an open private object: its volume serial number and its
/// 64-bit file id, the twin of Unix `(dev, ino)`. A file id that needs all 128 bits, as on
/// ReFS, is refused, so two files never share one identity. Input: the handle. Output: the
/// pair, or the error.
pub fn identity(object: &File) -> io::Result<(u64, u64)> {
    let (volume, id) = id(object)?;
    let (low, high) = id.split_at(8);
    if high.iter().any(|byte| *byte != 0) {
        return Err(refused("a private path's file id needs more than 64 bits"));
    }
    let mut bytes = [0; 8];
    bytes.copy_from_slice(low);
    Ok((volume, u64::from_le_bytes(bytes)))
}

/// Writes a private folder's entries to disk after a create, rename or delete in it, the
/// twin of `sync_all` on a Unix folder: the folder is opened for writing, never following a
/// link, and flushed. Input: the absolute folder path. Output: Ok, or the Windows error.
pub fn sync_folder(path: &Path) -> io::Result<()> {
    let name = wide_path(path)?;
    open(
        &name,
        GENERIC_WRITE,
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS,
        None,
    )?
    .sync_all()
}

/// Reads what W1's check judges from an open folder or file. Input: the handle. Output: the
/// facts, or the Windows error.
pub fn facts(object: &File) -> io::Result<Facts> {
    read(object).map(|(facts, _)| facts)
}

/// Refuses an open folder or file that is not private to the engine's user ([`judge`], and
/// [`judge_folder`] for a folder), the check every store runs on what it opens. Input: the
/// handle. Output: Ok, or a `PermissionDenied` error with the reason.
pub fn check(object: &File) -> io::Result<()> {
    let user = user_sid()?;
    let (facts, attributes) = read(object)?;
    judged(&facts, &user, attributes & FILE_ATTRIBUTE_DIRECTORY != 0)
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

/// Says whether two open handles name the same file, by volume serial number and 128-bit
/// file id, the twin of comparing `dev` and `ino` on Unix. Inputs: both handles. Output:
/// the answer, or the Windows error.
pub fn same(one: &File, other: &File) -> io::Result<bool> {
    Ok(id(one)? == id(other)?)
}

/// Reads an open object's volume serial number and 128-bit file id. Relies on the handle
/// staying open for the call and on the buffer being one whole `FILE_ID_INFO`. Input: the
/// handle. Output: both, or the Windows error.
fn id(object: &File) -> io::Result<(u64, [u8; 16])> {
    let mut information = FILE_ID_INFO::default();
    let read = unsafe {
        GetFileInformationByHandleEx(
            object.as_raw_handle(),
            FileIdInfo,
            (&raw mut information).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        information.VolumeSerialNumber,
        information.FileId.Identifier,
    ))
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
/// [`judged`] does not find private. Inputs: the handle, the user's SID and whether a folder
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
    judged(&facts, user, folder)
}

/// Judges an object's facts as a folder ([`judge_folder`]) or a file ([`judge`]).
/// Inputs: the facts, the user's SID and whether the object is a folder. Output: Ok, or a
/// `PermissionDenied` error with the reason.
fn judged(facts: &Facts, user: &[u8], folder: bool) -> io::Result<()> {
    let verdict = if folder {
        judge_folder(facts, user)
    } else {
        judge(facts, user)
    };
    verdict.map_err(refused)
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

/// The security descriptor a private folder or file is created with: the user as owner, the
/// protected list of [`list`] and, when one is named, a label in an unprotected system list,
/// as a protected one needs SeSecurityPrivilege (error 1314). The buffers do not move when
/// this value does, so the descriptor's pointers stay valid.
struct Planned {
    owner: Words,
    list: Words,
    label: Option<Words>,
    descriptor: SECURITY_DESCRIPTOR,
}

impl Planned {
    /// Builds the descriptor. Inputs: the user's SID, whether it is for a folder and the
    /// system list holding its label, if any. Output: the descriptor, or the Windows error.
    fn new(user: &[u8], folder: bool, label: Option<Vec<u8>>) -> io::Result<Self> {
        assert!(label.as_ref().is_none_or(|sacl| sacl.len() > 8));
        let mut planned = Self {
            owner: Words::from_bytes(user),
            list: Words::from_bytes(&list(user, folder)),
            label: label.map(|sacl| Words::from_bytes(&sacl)),
            descriptor: SECURITY_DESCRIPTOR::default(),
        };
        let descriptor: PSECURITY_DESCRIPTOR = (&raw mut planned.descriptor).cast();
        let owner: PSID = planned.owner.words.as_mut_ptr().cast();
        let dacl: *const ACL = planned.list.words.as_ptr().cast();
        let sacl: Option<*const ACL> = planned
            .label
            .as_ref()
            .map(|label| label.words.as_ptr().cast());
        let protected = SE_DACL_PROTECTED;
        let built = unsafe {
            InitializeSecurityDescriptor(descriptor, SECURITY_DESCRIPTOR_REVISION) != 0
                && SetSecurityDescriptorOwner(descriptor, owner, 0) != 0
                && SetSecurityDescriptorDacl(descriptor, 1, dacl, 0) != 0
                && sacl.is_none_or(|sacl| SetSecurityDescriptorSacl(descriptor, 1, sacl, 0) != 0)
                && SetSecurityDescriptorControl(descriptor, protected, protected) != 0
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
