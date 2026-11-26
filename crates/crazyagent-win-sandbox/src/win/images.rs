use super::last_error;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

const PROCESSES_MAX: usize = 65536;
const IMAGE_UNITS_MAX: usize = 32768;

/// Tells whether a running program's file lies in one of the folders or is one of the files,
/// read from a snapshot of every process on the computer; a process this user cannot open is
/// skipped. Input: absolute folders and files in their real spelling. Output: whether one
/// runs, or the Windows error code when the snapshot cannot be read or holds too many.
pub fn image_runs_under(folders: &[PathBuf]) -> Result<bool, u32> {
    assert!(folders.iter().all(|folder| folder.is_absolute()));
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut units = vec![0u16; IMAGE_UNITS_MAX];
    let mut found = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) } != 0;
    for _ in 0..PROCESSES_MAX {
        if !found {
            let code = last_error();
            return if code == ERROR_NO_MORE_FILES {
                Ok(false)
            } else {
                Err(code)
            };
        }
        let runs = image(entry.th32ProcessID, &mut units)
            .is_some_and(|image| folders.iter().any(|folder| image.starts_with(folder)));
        if runs {
            return Ok(true);
        }
        found = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) } != 0;
    }
    Err(ERROR_INSUFFICIENT_BUFFER)
}

/// The full file name of a process's program, or none for a process this user cannot open,
/// such as a system process, or one that ended since the snapshot. Relies on the opened
/// handle being null on failure or a new handle this caller owns alone. Inputs: the process
/// id and a buffer of [`IMAGE_UNITS_MAX`] units. Output: the file name.
fn image(id: u32, units: &mut [u16]) -> Option<PathBuf> {
    assert_eq!(units.len(), IMAGE_UNITS_MAX);
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, id) };
    if process.is_null() {
        return None;
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let mut len = IMAGE_UNITS_MAX as u32;
    let read = unsafe {
        QueryFullProcessImageNameW(
            process.as_raw_handle(),
            PROCESS_NAME_WIN32,
            units.as_mut_ptr(),
            &mut len,
        )
    };
    if read == 0 {
        return None;
    }
    assert!((len as usize) < IMAGE_UNITS_MAX);
    Some(PathBuf::from(OsString::from_wide(&units[..len as usize])))
}

#[cfg(test)]
mod tests {
    use super::image_runs_under;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// The real spelling of a folder as the engine passes it: long names, without the
    /// verbatim prefix. Input: the folder. Output: the spelling.
    fn plain(folder: &std::path::Path) -> PathBuf {
        let real = std::fs::canonicalize(folder).unwrap();
        let text = real.to_str().unwrap();
        PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text))
    }

    /// A program started from a folder outside any runtime, held suspended so it never needs
    /// its language files, runs under that folder and as that file, not under another
    /// folder, and under none once it has ended.
    #[test]
    fn a_program_started_from_a_folder_runs_under_it_until_it_ends() {
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        let made = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let folder = plain(made.path());
        let copy = folder.join("PING.EXE");
        std::fs::copy(system.join("PING.EXE"), &copy).unwrap();
        let mut child = Command::new(&copy)
            .args(["-n", "60", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        let here = [folder.clone()];
        assert_eq!(image_runs_under(&here), Ok(true));
        assert_eq!(image_runs_under(std::slice::from_ref(&copy)), Ok(true));
        assert_eq!(image_runs_under(&[plain(other.path())]), Ok(false));
        child.kill().unwrap();
        child.wait().unwrap();
        drop(child);
        assert_eq!(image_runs_under(&here), Ok(false));
    }

    /// The short (8.3) spelling of a folder, or the folder itself where the volume keeps no
    /// short names. Input: the folder. Output: the spelling.
    fn short(folder: &std::path::Path) -> PathBuf {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
        let wide: Vec<u16> = folder.as_os_str().encode_wide().chain([0]).collect();
        let mut units = vec![0u16; 32768];
        let len = unsafe { GetShortPathNameW(wide.as_ptr(), units.as_mut_ptr(), 32768) };
        assert!(
            len > 0 && (len as usize) < units.len(),
            "{}",
            folder.display()
        );
        PathBuf::from(std::ffi::OsString::from_wide(&units[..len as usize]))
    }

    /// Starts the System32 ping held suspended with one more argument, so it never reads it.
    /// Input: the argument. Output: the child.
    fn naming(argument: &str) -> std::process::Child {
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        Command::new(system.join("PING.EXE"))
            .args(["-n", "60", argument])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
            .spawn()
            .unwrap()
    }

    /// A program whose file lies elsewhere runs under a folder while its command line names a
    /// file there, as `node.exe` runs a script, in any case, with forward slashes or in the
    /// folder's short spelling; a sibling folder whose name only starts the same does not.
    #[test]
    fn a_program_whose_command_line_names_a_file_under_a_folder_runs_under_it() {
        let made = tempfile::tempdir().unwrap();
        let folder = plain(made.path());
        let script = folder.join("node_modules").join("tool").join("cli.js");
        let text = script.to_str().unwrap().to_owned();
        let shortened = short(&folder)
            .join("node_modules")
            .join("tool")
            .join("cli.js");
        let here = [folder.clone()];
        for named in [
            text.clone(),
            text.to_uppercase(),
            text.replace('\\', "/"),
            shortened.to_str().unwrap().to_owned(),
        ] {
            let mut child = naming(&named);
            assert_eq!(image_runs_under(&here), Ok(true), "{named}");
            child.kill().unwrap();
            child.wait().unwrap();
        }
        let mut sibling = naming(&format!("{}old\\cli.js", folder.display()));
        assert_eq!(image_runs_under(&here), Ok(false));
        sibling.kill().unwrap();
        sibling.wait().unwrap();
    }

    /// A folder named without its drive is a caller's mistake, never a folder no program
    /// runs from.
    #[test]
    #[should_panic]
    fn a_relative_folder_is_refused() {
        let _ = image_runs_under(&[PathBuf::from("relative")]);
    }
}
