use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

const FOLDER_UNITS_START: usize = 260;
const FOLDER_UNITS_MAX: usize = 32768;
const _: () = assert!(FOLDER_UNITS_START >= 2 && FOLDER_UNITS_START <= FOLDER_UNITS_MAX);

/// The Windows system folder, such as `C:\Windows\system32`, as GetSystemDirectoryW names
/// it, never read from a variable the user's environment can set. Only administrators can
/// create a file in it. Output: the folder, or the I/O error.
pub fn system_folder() -> io::Result<PathBuf> {
    let mut units = vec![0u16; FOLDER_UNITS_START];
    for _ in 0..2 {
        assert!(units.len() <= FOLDER_UNITS_MAX);
        let wrote = unsafe { GetSystemDirectoryW(units.as_mut_ptr(), units.len() as u32) } as usize;
        if wrote == 0 {
            return Err(io::Error::last_os_error());
        }
        if wrote < units.len() {
            units.truncate(wrote);
            if units.contains(&0) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "system folder holds a zero",
                ));
            }
            return Ok(PathBuf::from(OsString::from_wide(&units)));
        }
        if wrote > FOLDER_UNITS_MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "system folder too long",
            ));
        }
        units = vec![0u16; wrote];
    }
    Err(io::Error::other("system folder changed while it was read"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// V244-WIN-GIT-SAFE-PROGRAM-PATH: the folder GetSystemDirectoryW names is absolute and
    /// holds kernel32.dll, so it is the system folder and not a guess.
    #[test]
    fn the_system_folder_is_the_absolute_folder_that_holds_kernel32() {
        let folder = system_folder().unwrap();
        assert!(folder.is_absolute(), "{}", folder.display());
        assert!(
            folder.join("kernel32.dll").is_file(),
            "{}",
            folder.display()
        );
    }
}
