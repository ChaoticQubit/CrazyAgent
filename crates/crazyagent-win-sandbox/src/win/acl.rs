use super::{BoundaryError, Words, last_error, wide, windows};
use crate::dacl::{ALL_APPLICATION_PACKAGES, converge, reads, without};
use crate::plan::{ANCHOR, Access, Ace, KEEP_ENTRIES, MODIFY, READ_EXECUTE};
use crate::snapshot::{self, Snapshot};
use std::io::ErrorKind;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, OBJECT_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
    UNPROTECTED_DACL_SECURITY_INFORMATION,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD,
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, WRITE_DAC, WRITE_OWNER,
};

const _: () = assert!(READ_EXECUTE == FILE_GENERIC_READ | FILE_GENERIC_EXECUTE);
const _: () =
    assert!(MODIFY == FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE);
const _: () = assert!(MODIFY & (FILE_DELETE_CHILD | WRITE_DAC | WRITE_OWNER) == 0);
const _: () = assert!(ANCHOR == MODIFY & !DELETE);
const _: () = assert!(KEEP_ENTRIES == ANCHOR & !(FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY));

pub(super) const ACL_BYTES_MAX: usize = 64 * 1024;

/// A path's access list as Windows holds it, and whether its inheritance is cut.
struct Dacl {
    list: Vec<u8>,
    protected: bool,
}

/// One access list to write: the path, the list and the security information that says
/// whether Windows cuts the path's inheritance, turns it back on, or keeps it as it is.
pub(super) struct Change {
    path: String,
    list: Vec<u8>,
    info: OBJECT_SECURITY_INFORMATION,
}

/// What one planned role takes on its path: the list to write, if any, and for a role that
/// cuts, the snapshot to save before that write.
pub(super) struct Step {
    pub(super) change: Option<Change>,
    pub(super) snapshot: Option<Snapshot>,
}

/// What withdrawing the container from a path takes: the list to write, if any, what the
/// shared record keeps for the path (None removes it), and whether the owner's own list was
/// kept because it changed while guarded.
pub(super) struct Withdrawal {
    pub(super) change: Option<Change>,
    pub(super) snapshot: Option<Snapshot>,
    pub(super) owner_changed: bool,
}

/// What the container's role on one path needs, from the path's list now (DEC-193): the
/// container's own entries become the role's, and a guarded path or anchored folder is cut
/// and every other package narrowed. A granted root must exist and must not be a link or
/// junction; a guarded path or anchored folder that is missing, a link or a junction gets
/// nothing, since the plan keeps the entries of the present folder above it. A read root every
/// container can already read, as with `C:\Program Files`, is left as it is unless it still
/// holds an entry of the container's own. Inputs: the container SID, the role and the path's
/// saved snapshot. Output: the step, or the refusal or failed step.
pub(super) fn role(
    container: &Words,
    ace: &Ace,
    saved: Option<&Snapshot>,
) -> Result<Step, BoundaryError> {
    let none = || Step {
        change: None,
        snapshot: None,
    };
    let Some(folder) = kind(&ace.path, ace.access.cuts())? else {
        return Ok(none());
    };
    let current = read(&ace.path)?;
    let sid = container.bytes();
    if ace.access == Access::ReadExecute
        && reads(&current.list, &ALL_APPLICATION_PACKAGES)
        && without(&current.list, &sid)
            .map_err(BoundaryError::Refused)?
            .is_none()
    {
        return Ok(none());
    }
    let target =
        converge(&current.list, &sid, ace.access, folder).map_err(BoundaryError::Refused)?;
    let cut = ace.access.cuts();
    let rewrite = target.is_some() || (cut && !current.protected);
    let list = target.unwrap_or_else(|| current.list.clone());
    let snapshot = if cut {
        let snapshot = snapshot::guard(
            saved,
            &ace.path,
            &sid,
            current.protected,
            &current.list,
            &list,
        )
        .map_err(BoundaryError::Refused)?;
        Some(snapshot)
    } else {
        None
    };
    let protect = cut || current.protected;
    let change = rewrite.then(|| Change {
        path: ace.path.clone(),
        info: info(protect, current.protected),
        list,
    });
    assert!(!cut || snapshot.is_some());
    assert!(change.as_ref().is_none_or(|change| change.path == ace.path));
    Ok(Step { change, snapshot })
}

/// What taking the container off a path needs: its own entries go and every other entry
/// stays; a path the shared record keeps a snapshot for is released through it, so the last
/// container guarding it brings back the original list and protection unless the owner
/// changed it. A path that is gone, or is now a link or junction, holds none of the
/// boundary's entries. Inputs: the container SID, the path, its saved snapshot and whether
/// another container still guards it. Output: the withdrawal, or the refusal or failed step.
pub(super) fn withdrawal(
    container: &Words,
    path: &str,
    saved: Option<&Snapshot>,
    guarded_by_others: bool,
) -> Result<Withdrawal, BoundaryError> {
    let kept = saved.filter(|_| guarded_by_others).cloned();
    if kind(path, true)?.is_none() {
        return Ok(Withdrawal {
            change: None,
            snapshot: kept,
            owner_changed: false,
        });
    }
    let current = read(path)?;
    let sid = container.bytes();
    let Some(saved) = saved else {
        let list = without(&current.list, &sid).map_err(BoundaryError::Refused)?;
        let change = list.map(|list| Change {
            path: path.to_owned(),
            info: info(current.protected, current.protected),
            list,
        });
        return Ok(Withdrawal {
            change,
            snapshot: None,
            owner_changed: false,
        });
    };
    let release = snapshot::release(saved, &current.list, &sid, guarded_by_others)
        .map_err(BoundaryError::Refused)?;
    let protect = release.protected.unwrap_or(current.protected);
    let change = release.list.map(|list| Change {
        path: path.to_owned(),
        info: info(protect, current.protected),
        list,
    });
    assert!(!guarded_by_others || release.snapshot.is_some());
    assert!(guarded_by_others || release.snapshot.is_none());
    Ok(Withdrawal {
        change,
        snapshot: release.snapshot,
        owner_changed: release.owner_changed,
    })
}

/// Writes one change. Input: the change. Output: Ok, or the failed step.
pub(super) fn write(change: &Change) -> Result<(), BoundaryError> {
    let path = wide(&change.path);
    let list = Words::from_bytes(&change.list);
    write_dacl(&path, &list, change.info).map_err(windows("change an access list"))
}

/// Whether a path is present as the boundary counts it for a plan: a real file or folder, not
/// a link or junction, the only kind it puts entries on. Input: a plain drive path. Output:
/// true when present.
pub(super) fn present(path: &str) -> bool {
    std::fs::symlink_metadata(Path::new(path))
        .is_ok_and(|meta| meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0)
}

/// Whether a path is a folder, or None when it is missing, a link or a junction and `optional`
/// lets it be skipped; a path that must be there and is not is refused. Inputs: the path and
/// whether it may be skipped. Output: true for a folder, or the refusal.
fn kind(path: &str, optional: bool) -> Result<Option<bool>, BoundaryError> {
    let meta = match std::fs::symlink_metadata(Path::new(path)) {
        Ok(meta) => meta,
        Err(error) if error.kind() == ErrorKind::NotFound && optional => return Ok(None),
        Err(_) => return Err(BoundaryError::Refused("a granted folder cannot be read")),
    };
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return Ok(Some(meta.is_dir()));
    }
    if optional {
        return Ok(None);
    }
    Err(BoundaryError::Refused(
        "a granted folder is a link or junction",
    ))
}

/// The security information for writing a list: cut the inheritance when `protect` holds,
/// turn it back on when it was cut and is no longer, and say nothing of it otherwise, the
/// way every unprotected list has been written. Inputs: the protection to set and the one
/// the path has now. Output: the information flags.
fn info(protect: bool, protected: bool) -> OBJECT_SECURITY_INFORMATION {
    let info = match (protect, protected) {
        (true, _) => DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
        (false, true) => DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
        (false, false) => DACL_SECURITY_INFORMATION,
    };
    assert_ne!(info & DACL_SECURITY_INFORMATION, 0);
    assert_eq!(
        info & PROTECTED_DACL_SECURITY_INFORMATION != 0,
        protect,
        "a cut is always stated"
    );
    info
}

/// Frees memory the system allocated with `LocalAlloc` for this caller when dropped.
pub(super) struct Local(pub(super) HLOCAL);

impl Drop for Local {
    /// Frees the block. Relies on it being a `LocalAlloc` block owned by this value alone.
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

/// Reads a copy of the path's access list and whether its inheritance is cut; a path with no
/// list, which grants everyone everything, is refused. Input: a plain drive path. Output:
/// the list, or the refusal or failed step.
fn read(path: &str) -> Result<Dacl, BoundaryError> {
    let path = wide(path);
    let (list, control) = read_dacl(&path).map_err(windows("read an access list"))?;
    let list = list.ok_or(BoundaryError::Refused("a granted path has no access list"))?;
    let list = list.bytes();
    assert!(list.len() >= 8);
    Ok(Dacl {
        list,
        protected: control & SE_DACL_PROTECTED != 0,
    })
}

/// Reads a copy of the path's access list, or None when it has none, with the control bits
/// of its security descriptor. Relies on `path` being NUL-terminated UTF-16 that outlives the
/// call, on the list pointing into the security descriptor the call allocated, and on that
/// descriptor staying alive until the copy is made and its control bits are read.
fn read_dacl(path: &[u16]) -> Result<(Option<Words>, u16), u32> {
    assert_eq!(path.last(), Some(&0));
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let started = std::time::Instant::now();
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
    probe_note("get", path, started);
    if status != ERROR_SUCCESS {
        return Err(status);
    }
    let descriptor = Local(descriptor);
    assert!(!descriptor.0.is_null());
    let mut control: u16 = 0;
    let mut revision: u32 = 0;
    if unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) } == 0 {
        return Err(last_error());
    }
    if dacl.is_null() {
        return Ok((None, control));
    }
    let size = usize::from(unsafe { (*dacl).AclSize });
    assert!((8..=ACL_BYTES_MAX).contains(&size));
    let copy = unsafe { Words::copy_from(dacl.cast_const().cast(), size) };
    drop(descriptor);
    Ok((Some(copy), control))
}

/// Replaces the path's access list, keeping its owner and group, with the inheritance setting
/// `info` names; Windows passes inheritable entries on to existing files and folders below.
/// Relies on `path` being NUL-terminated UTF-16 and `dacl` an aligned, well-formed access
/// list, both outliving the call.
fn write_dacl(path: &[u16], dacl: &Words, info: OBJECT_SECURITY_INFORMATION) -> Result<(), u32> {
    assert_eq!(path.last(), Some(&0));
    assert_ne!(info & DACL_SECURITY_INFORMATION, 0);
    let started = std::time::Instant::now();
    let status = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            info,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl.words.as_ptr().cast(),
            std::ptr::null(),
        )
    };
    probe_note("set", path, started);
    if status != ERROR_SUCCESS {
        return Err(status);
    }
    Ok(())
}

/// Appends one timed access-list call to the file CRAZYAGENT_PROBE_ACL_LOG names, when it is
/// set, for the ARM64 Defender probe on a test-only farm branch. Inputs: the call kind, the
/// NUL-terminated UTF-16 path and when the call started. Output: none; a failed append is
/// dropped, so the probe never changes what the boundary does.
fn probe_note(kind: &str, path: &[u16], started: std::time::Instant) {
    let elapsed = started.elapsed();
    let Some(log) = std::env::var_os("CRAZYAGENT_PROBE_ACL_LOG") else {
        return;
    };
    assert_eq!(path.last(), Some(&0));
    let shown = String::from_utf16_lossy(&path[..path.len() - 1]);
    let line = format!("{kind} {:.3} {shown}\n", elapsed.as_secs_f64() * 1000.0);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .and_then(|mut file| std::io::Write::write_all(&mut file, line.as_bytes()));
}
