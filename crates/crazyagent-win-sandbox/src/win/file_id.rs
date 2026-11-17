use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
};

/// The identity of an open file, in the role a device and inode pair plays on unix: its
/// volume's serial number and its file index on that volume, read from the handle, so it
/// names what was opened whatever the path does afterwards. Relies on `file` staying open
/// for the call and on the out pointer naming a live, zeroed struct of the size Windows
/// writes. Input: the open file. Output: (volume serial, file index), or the I/O error.
pub fn file_id(file: &File) -> io::Result<(u64, u64)> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    let read = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    let index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok((u64::from(information.dwVolumeSerialNumber), index))
}
