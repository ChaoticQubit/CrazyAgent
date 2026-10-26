use super::{BoundaryError, Words, wide, windows};
use crate::dacl::{ALL_APPLICATION_PACKAGES, holds, reads};
use crate::plan::{Access, Ace, DENY_ALL, DENY_WRITE, KEEP_ENTRIES, MODIFY, READ_EXECUTE};
use std::io::ErrorKind;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    DENY_ACCESS, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE,
    SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN,
    TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACE_FLAGS, ACL, DACL_SECURITY_INFORMATION, NO_INHERITANCE, PSECURITY_DESCRIPTOR,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_ALL_ACCESS, FILE_APPEND_DATA,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, WRITE_DAC,
    WRITE_OWNER,
};

const _: () = assert!(READ_EXECUTE == FILE_GENERIC_READ | FILE_GENERIC_EXECUTE);
const _: () =
    assert!(MODIFY == FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE);
const _: () = assert!(
    DENY_WRITE
        == FILE_WRITE_DATA
            | FILE_APPEND_DATA
            | FILE_WRITE_EA
            | FILE_DELETE_CHILD
            | FILE_WRITE_ATTRIBUTES
            | DELETE
            | WRITE_DAC
            | WRITE_OWNER
);
const _: () = assert!(DENY_ALL == FILE_ALL_ACCESS);
const _: () = assert!(
    KEEP_ENTRIES
        == FILE_ADD_FILE
            | FILE_ADD_SUBDIRECTORY
            | FILE_DELETE_CHILD
            | DELETE
            | WRITE_DAC
            | WRITE_OWNER
);

const ACL_BYTES_MAX: usize = 64 * 1024;

/// Puts each planned entry on its path for the container, skipping an entry the path already
/// holds so later launches leave large folders alone. A granted root must exist and must not be
/// a link or junction; a protected path that is missing, a link or a junction gets no entry,
/// since the kept folder above it stops the container creating or swapping it. A read root the
/// owner may not change is accepted only when every container can already read it, as with
/// `C:\Program Files`. Inputs: the container SID and the plan. Output: Ok, or the refusal or
/// failed step.
pub(super) fn apply(container: &Words, aces: &[Ace]) -> Result<(), BoundaryError> {
    let sid = container.bytes();
    for ace in aces {
        let meta = match std::fs::symlink_metadata(Path::new(&ace.path)) {
            Ok(meta) => meta,
            Err(error) if error.kind() == ErrorKind::NotFound && ace.access.denies() => continue,
            Err(_) => return Err(BoundaryError::Refused("a granted folder cannot be read")),
        };
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            if ace.access.denies() {
                continue;
            }
            return Err(BoundaryError::Refused(
                "a granted folder is a link or junction",
            ));
        }
        let folder = meta.is_dir();
        let path = wide(&ace.path);
        let current = read_dacl(&path)
            .map_err(windows("read an access list"))?
            .ok_or(BoundaryError::Refused(
                "a granted folder has no access list",
            ))?;
        let current = current.bytes();
        if holds(&current, &sid, ace.access, folder) {
            continue;
        }
        if ace.access == Access::ReadExecute && reads(&current, &ALL_APPLICATION_PACKAGES) {
            continue;
        }
        let inheritance = if folder && ace.access.inherits() {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        };
        let merged = merge(&current, container, ace.access, inheritance)
            .map_err(windows("build an access list"))?;
        write_dacl(&path, &merged).map_err(windows("change an access list"))?;
    }
    Ok(())
}

/// Frees memory the system allocated with `LocalAlloc` for this caller when dropped.
struct Local(HLOCAL);

impl Drop for Local {
    /// Frees the block. Relies on it being a `LocalAlloc` block owned by this value alone.
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

/// Reads a copy of the path's access list, or None when it has none. Relies on `path` being
/// NUL-terminated UTF-16 that outlives the call, on the list pointing into the security
/// descriptor the call allocated, and on that descriptor staying alive until the copy is made.
fn read_dacl(path: &[u16]) -> Result<Option<Words>, u32> {
    assert_eq!(path.last(), Some(&0));
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(status);
    }
    let descriptor = Local(descriptor);
    if dacl.is_null() {
        return Ok(None);
    }
    let size = usize::from(unsafe { (*dacl).AclSize });
    assert!((8..=ACL_BYTES_MAX).contains(&size));
    let copy = unsafe { Words::copy_from(dacl.cast_const().cast(), size) };
    drop(descriptor);
    Ok(Some(copy))
}

/// Builds a new access list from `current` plus one entry for the container. Relies on
/// `current` and the SID being aligned copies that outlive the call, on Windows only reading
/// them, and on the new list being a `LocalAlloc` block this fn copies and then frees.
fn merge(
    current: &[u8],
    sid: &Words,
    access: Access,
    inheritance: ACE_FLAGS,
) -> Result<Words, u32> {
    let current = Words::from_bytes(current);
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: access.mask(),
        grfAccessMode: if access.denies() {
            DENY_ACCESS
        } else {
            GRANT_ACCESS
        },
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.words.as_ptr().cast_mut().cast(),
        },
    };
    let mut merged: *mut ACL = std::ptr::null_mut();
    let status = unsafe { SetEntriesInAclW(1, &entry, current.words.as_ptr().cast(), &mut merged) };
    if status != ERROR_SUCCESS {
        return Err(status);
    }
    let owner = Local(merged.cast());
    assert!(!merged.is_null());
    let size = usize::from(unsafe { (*merged).AclSize });
    assert!((8..=ACL_BYTES_MAX).contains(&size));
    let copy = unsafe { Words::copy_from(merged.cast_const().cast(), size) };
    drop(owner);
    Ok(copy)
}

/// Replaces the path's access list, keeping its owner, group and inheritance setting; Windows
/// passes inheritable entries on to existing files and folders below. Relies on `path` being
/// NUL-terminated UTF-16 and `dacl` an aligned, well-formed access list, both outliving the call.
fn write_dacl(path: &[u16], dacl: &Words) -> Result<(), u32> {
    assert_eq!(path.last(), Some(&0));
    let status = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl.words.as_ptr().cast(),
            std::ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(status);
    }
    Ok(())
}
