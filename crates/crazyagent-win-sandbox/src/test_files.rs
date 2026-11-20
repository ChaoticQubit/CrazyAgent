use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_NON_DIRECTORY_FILE, FILE_SUPERSEDE, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
};
use windows_sys::Win32::Foundation::{HANDLE, UNICODE_STRING};
use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    READ_CONTROL, SYNCHRONIZE, WRITE_DAC,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READWRITE,
    UnmapViewOfFile,
};

const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const CASE_INSENSITIVE: u32 = 0x40;

/// A writable view of a whole file whose file and section handles are already closed, so only
/// the view keeps the section alive; dropping it unmaps the view.
pub struct WritableView {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

impl Drop for WritableView {
    fn drop(&mut self) {
        assert_ne!(unsafe { UnmapViewOfFile(self.view) }, 0);
    }
}

/// Maps a file for write and closes its file and section handles, keeping only the view, as a
/// program that writes through a mapping leaves it (N1 probe 2c). Relies on the file being
/// non-empty. Input: the file path. Output: the view, or the Windows error.
pub fn writable_view(path: &Path) -> io::Result<WritableView> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(SHARE_ALL)
        .open(path)?;
    let section =
        unsafe { CreateFileMappingW(file.as_raw_handle(), null(), PAGE_READWRITE, 0, 0, null()) };
    if section.is_null() {
        return Err(io::Error::last_os_error());
    }
    let section = unsafe { OwnedHandle::from_raw_handle(section) };
    let view = unsafe { MapViewOfFile(section.as_raw_handle(), FILE_MAP_WRITE, 0, 0, 0) };
    if view.Value.is_null() {
        return Err(io::Error::last_os_error());
    }
    drop(section);
    drop(file);
    Ok(WritableView { view })
}

/// Supersedes a file through `NtCreateFile` with DELETE access alone and every share mode, the
/// open that empties a file another handle holds with read and delete sharing (N1 probe 2d1).
/// The path must be absolute and in plain drive form (`C:\...`), since `\??\` before a
/// verbatim or UNC spelling names no file. Input: the path. Output: Ok, or the NTSTATUS error.
pub fn supersede(path: &Path) -> io::Result<()> {
    assert!(path.is_absolute());
    let spelled: Vec<u16> = path.as_os_str().encode_wide().take(3).collect();
    assert_eq!(spelled.get(1), Some(&u16::from(b':')));
    assert_ne!(spelled.first(), Some(&u16::from(b'\\')));
    let name: Vec<u16> = format!(r"\??\{}", path.display()).encode_utf16().collect();
    let bytes = u16::try_from(name.len() * 2).map_err(|_| io::ErrorKind::InvalidInput)?;
    let text = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: name.as_ptr().cast_mut(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: null_mut(),
        ObjectName: &text,
        Attributes: CASE_INSENSITIVE,
        SecurityDescriptor: null(),
        SecurityQualityOfService: null(),
    };
    let mut handle: HANDLE = null_mut();
    let mut status = IO_STATUS_BLOCK::default();
    let result = unsafe {
        NtCreateFile(
            &mut handle,
            DELETE | SYNCHRONIZE,
            &attributes,
            &mut status,
            null(),
            FILE_ATTRIBUTE_NORMAL,
            SHARE_ALL,
            FILE_SUPERSEDE,
            FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::other(format!(
            "NTSTATUS {:#010x}",
            result as u32
        )));
    }
    drop(unsafe { OwnedHandle::from_raw_handle(handle) });
    Ok(())
}

/// Sets a NULL access list on a file, which grants every right to everyone, so a row can show
/// that a check refuses a file with no list rather than skipping it. Input: the path. Output:
/// Ok, or the Windows error.
pub fn null_dacl(path: &Path) -> io::Result<()> {
    let file: File = OpenOptions::new()
        .access_mode(WRITE_DAC | READ_CONTROL)
        .share_mode(SHARE_ALL)
        .open(path)?;
    let set = unsafe {
        SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null(),
            null(),
        )
    };
    if set != 0 {
        return Err(io::Error::from_raw_os_error(set as i32));
    }
    Ok(())
}
