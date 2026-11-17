use super::acl::{ACL_BYTES_MAX, Local};
use super::{BoundaryError, last_error, windows};
use crate::dacl::{
    ANCESTOR_CHANGES, DENY_TYPES, FILE_CHANGES, OWN_FOLDER_CHANGES, SID_BYTES_MAX, other_writer,
    trusted_owner,
};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use windows_sys::Win32::Foundation::{
    ERROR_INVALID_NAME, ERROR_INVALID_SID, ERROR_SUCCESS, GENERIC_ALL, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetLengthSid, GetTokenInformation, IsValidSid,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
    WRITE_OWNER,
};
use windows_sys::Win32::System::SystemServices::{
    ACCESS_DENIED_ACE_TYPE, ACCESS_DENIED_CALLBACK_ACE_TYPE,
    ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE, ACCESS_DENIED_OBJECT_ACE_TYPE,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const TOKEN_USER_BYTES_MAX: usize = 256;
const FOLDERS_MAX: usize = 128;

const _: () = assert!(
    FILE_CHANGES
        == FILE_WRITE_DATA
            | FILE_APPEND_DATA
            | FILE_WRITE_EA
            | FILE_WRITE_ATTRIBUTES
            | DELETE
            | WRITE_DAC
            | WRITE_OWNER
            | GENERIC_WRITE
            | GENERIC_ALL
);
const _: () = assert!(OWN_FOLDER_CHANGES == FILE_CHANGES | FILE_DELETE_CHILD);
const _: () = assert!(
    ANCESTOR_CHANGES
        == FILE_DELETE_CHILD | DELETE | WRITE_DAC | WRITE_OWNER | GENERIC_WRITE | GENERIC_ALL
);
const _: () = assert!(size_of::<TOKEN_USER>() < TOKEN_USER_BYTES_MAX);
const _: () = assert!(
    DENY_TYPES[0] as u32 == ACCESS_DENIED_ACE_TYPE
        && DENY_TYPES[1] as u32 == ACCESS_DENIED_OBJECT_ACE_TYPE
        && DENY_TYPES[2] as u32 == ACCESS_DENIED_CALLBACK_ACE_TYPE
        && DENY_TYPES[3] as u32 == ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE
);

/// Refuses a program the engine would start with its own rights unless only SYSTEM, the
/// administrators, TrustedInstaller and the engine's own user can change it, the Windows twin
/// of the Unix owner and mode check: the file, its folder and every folder above it as
/// written, each read where it sits without following a link, owned by one of them and with
/// no other SID given a right that changes it there ([`crate::dacl::other_writer`]). Input: the
/// absolute program path. Output: Ok, or the refusal or the failed step.
pub fn owner_only(program: &Path) -> Result<(), BoundaryError> {
    if !program.is_absolute() {
        return Err(BoundaryError::Refused("a program path is not absolute"));
    }
    let user = user_sid().map_err(windows("read the engine's user"))?;
    let mut folders = program.ancestors().skip(1);
    let own = folders
        .next()
        .ok_or(BoundaryError::Refused("a program has no folder"))?;
    changed_only_by(program, &user, FILE_CHANGES)?;
    changed_only_by(own, &user, OWN_FOLDER_CHANGES)?;
    let mut checked = 0;
    for folder in folders {
        checked += 1;
        if checked > FOLDERS_MAX {
            return Err(BoundaryError::Refused("a program sits too deep"));
        }
        changed_only_by(folder, &user, ANCESTOR_CHANGES)?;
    }
    assert!(checked <= FOLDERS_MAX);
    Ok(())
}

/// Refuses a file or folder unless one of the trusted owners owns it and no other SID holds
/// any of `rights` on it. Inputs: the path, the engine's user SID and the rights that count
/// here. Output: Ok, or the refusal or the failed step.
fn changed_only_by(path: &Path, user: &[u8], rights: u32) -> Result<(), BoundaryError> {
    let (owner, dacl) =
        security(path).map_err(windows("read a program's owner and access list"))?;
    if !trusted_owner(&owner, user) {
        return Err(BoundaryError::Refused(
            "another account owns the program or a folder above it",
        ));
    }
    let Some(dacl) = dacl else {
        return Err(BoundaryError::Refused(
            "the program or a folder above it has no access list, so everyone can change it",
        ));
    };
    match other_writer(&dacl, user, rights).map_err(BoundaryError::Refused)? {
        None => Ok(()),
        Some(_) => Err(BoundaryError::Refused(
            "another account can change the program or a folder above it",
        )),
    }
}

/// Reads the owner SID and a copy of the access list of the file or folder at `path` itself,
/// opened with the link flag so a link or junction there is read, never what it leads to.
/// Relies on the handle staying open for the call, on the owner and list pointing into the
/// descriptor the call allocated, and on that descriptor staying alive until both are copied.
/// Input: the path. Output: the owner and the list (None when it has none), or the error.
fn security(path: &Path) -> Result<(Vec<u8>, Option<Vec<u8>>), u32> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    if wide[..wide.len() - 1].contains(&0) {
        return Err(ERROR_INVALID_NAME);
    }
    let opened = unsafe {
        CreateFileW(
            wide.as_ptr(),
            READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if opened == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(opened) };
    let mut owner: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
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
    let owner = sid_bytes(owner)?;
    let list = if dacl.is_null() {
        None
    } else {
        let size = usize::from(unsafe { (*dacl).AclSize });
        assert!((8..=ACL_BYTES_MAX).contains(&size));
        Some(unsafe { std::slice::from_raw_parts(dacl.cast::<u8>(), size) }.to_vec())
    };
    drop(descriptor);
    drop(handle);
    Ok((owner, list))
}

/// The engine's own user SID, read from its process token. Relies on the token handle
/// staying open for both calls, and on the buffer, aligned for `TOKEN_USER` and as long as
/// Windows asked for, outliving the read of the SID it holds. Output: the SID bytes, or the
/// Windows error.
fn user_sid() -> Result<Vec<u8>, u32> {
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut needed = 0_u32;
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    let bytes = needed as usize;
    if !(size_of::<TOKEN_USER>()..=TOKEN_USER_BYTES_MAX).contains(&bytes) {
        return Err(last_error());
    }
    let mut buffer = vec![0_u64; bytes.div_ceil(size_of::<u64>())];
    let read = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if read == 0 {
        return Err(last_error());
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    sid_bytes(user.User.Sid)
}

/// Copies a SID Windows returned. Relies on `sid`, when not null, pointing at memory that
/// stays alive for the call. Input: the SID. Output: its bytes, or `ERROR_INVALID_SID`.
fn sid_bytes(sid: PSID) -> Result<Vec<u8>, u32> {
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(ERROR_INVALID_SID);
    }
    let size = unsafe { GetLengthSid(sid) } as usize;
    assert!((8..=SID_BYTES_MAX).contains(&size));
    Ok(unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), size) }.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn users(path: &Path, change: &[&str]) {
        let changed = Command::new("icacls")
            .arg(path)
            .args(change)
            .output()
            .unwrap();
        assert!(
            changed.status.success(),
            "{}",
            String::from_utf8_lossy(&changed.stderr)
        );
    }

    fn refused(program: &Path) -> bool {
        matches!(owner_only(program), Err(BoundaryError::Refused(_)))
    }

    /// #306 r2: the curl Windows ships passes and a relative path is refused. A program in a
    /// folder of this user's passes until Users may write it, add a file or a folder beside
    /// it, or delete a child of a folder above it; adding a folder above it, as Authenticated
    /// Users may at `C:\`, still passes.
    #[test]
    fn only_the_system_and_this_user_may_change_a_program_or_the_folders_above_it() {
        let shipped = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("curl.exe");
        owner_only(&shipped).unwrap();
        assert!(refused(Path::new("curl.exe")));

        let directory = tempfile::tempdir_in(std::env::var_os("USERPROFILE").unwrap()).unwrap();
        let above = directory.path().join("above");
        let own = above.join("own");
        fs::create_dir_all(&own).unwrap();
        let program = own.join("tool.exe");
        fs::write(&program, "").unwrap();
        owner_only(&program).unwrap();

        let remove = ["/remove:g", "*S-1-5-32-545"];
        users(&above, &["/grant", "*S-1-5-32-545:(AD)"]);
        owner_only(&program).unwrap();
        for (path, right) in [
            (&above, "(DC)"),
            (&own, "(AD)"),
            (&own, "(WD)"),
            (&program, "(W)"),
        ] {
            users(&above, &remove);
            users(path, &["/grant", &format!("*S-1-5-32-545:{right}")]);
            assert!(refused(&program), "{path:?} {right}");
            users(path, &remove);
        }
        users(&above, &remove);
        owner_only(&program).unwrap();
    }
}
