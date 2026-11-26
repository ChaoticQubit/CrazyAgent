use super::last_error;
use super::owner::{token_user, user_sid};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use windows_sys::Wdk::System::Threading::{
    NtQueryInformationProcess, ProcessCommandLineInformation,
};
use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE, UNICODE_STRING,
};
use windows_sys::Win32::Security::TOKEN_QUERY;
use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};

const PROCESSES_MAX: usize = 65536;
const IMAGE_UNITS_MAX: usize = 32768;
const COMMAND_WORDS: usize = (size_of::<UNICODE_STRING>() + 2 * 32768) / 8;

/// Tells whether a process of this user runs from one of the folders or files, or names one in
/// its command line as `node.exe` names its script; another account's process is skipped, and
/// one of this user's whose command line cannot be read counts as running. Input: absolute
/// folders and files in their real spelling. Output: the answer, or the Windows error code when
/// the snapshot or this user's SID cannot be read or the snapshot holds too many processes.
pub fn runs_under(folders: &[PathBuf]) -> Result<bool, u32> {
    assert!(folders.iter().all(|folder| folder.is_absolute()));
    let user = user_sid()?;
    let spellings: Vec<String> = folders
        .iter()
        .map(PathBuf::as_path)
        .flat_map(spellings)
        .collect();
    assert!(spellings.len() >= folders.len());
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
    let mut command = vec![0u64; COMMAND_WORDS];
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
        if runs(
            entry.th32ProcessID,
            &user,
            &spellings,
            &mut units,
            &mut command,
        ) {
            return Ok(true);
        }
        found = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) } != 0;
    }
    Err(ERROR_INSUFFICIENT_BUFFER)
}

/// Tells whether one process of this user runs from or names one of the spellings; a process
/// that cannot be opened, ended since the snapshot or belongs to another account does not.
/// Relies on the opened handle being null on failure or a new handle this caller owns alone.
/// Inputs: the process id, this user's SID, the spellings and the two read buffers. Output:
/// the answer; true when its command line cannot be read.
fn runs(
    id: u32,
    user: &[u8],
    spellings: &[String],
    units: &mut [u16],
    command: &mut [u64],
) -> bool {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, id) };
    if process.is_null() {
        return false;
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    if owner(&process).as_deref() != Some(user) {
        return false;
    }
    if image(&process, units).is_some_and(|image| names(&image, spellings)) {
        return true;
    }
    command_line(&process, command).is_none_or(|text| names(&text, spellings))
}

/// The user SID of a process's token, or none when the token cannot be opened or read, as a
/// process of another account often refuses. Input: the process handle. Output: the SID.
fn owner(process: &OwnedHandle) -> Option<Vec<u8>> {
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_user(&token).ok()
}

/// The full file name of a process's program, in the compared spelling, or none when it
/// cannot be read. Inputs: the process handle and a buffer of [`IMAGE_UNITS_MAX`] units.
/// Output: the lowered file name.
fn image(process: &OwnedHandle, units: &mut [u16]) -> Option<String> {
    assert_eq!(units.len(), IMAGE_UNITS_MAX);
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
    Some(lowered(&units[..len as usize]))
}

/// A process's command line in the compared spelling, read by ntdll into a buffer that holds
/// the longest one Windows allows. Relies on Windows writing the string's text inside that
/// buffer. Inputs: the process handle and the [`COMMAND_WORDS`] buffer. Output: the lowered
/// command line, or none when it cannot be read.
fn command_line(process: &OwnedHandle, command: &mut [u64]) -> Option<String> {
    assert_eq!(command.len(), COMMAND_WORDS);
    let bytes = (command.len() * 8) as u32;
    let mut written = 0u32;
    let status = unsafe {
        NtQueryInformationProcess(
            process.as_raw_handle(),
            ProcessCommandLineInformation,
            command.as_mut_ptr().cast(),
            bytes,
            &mut written,
        )
    };
    if status < 0 {
        return None;
    }
    let string = unsafe { &*command.as_ptr().cast::<UNICODE_STRING>() };
    let length = usize::from(string.Length);
    assert_eq!(length % 2, 0);
    if length == 0 {
        return Some(String::new());
    }
    let start = command.as_ptr() as usize;
    let text = string.Buffer as usize;
    assert!(text >= start + size_of::<UNICODE_STRING>());
    assert!(text + length <= start + bytes as usize);
    let units = unsafe { std::slice::from_raw_parts(string.Buffer, length / 2) };
    Some(lowered(units))
}

/// The spellings a folder or file is looked for in: its own and, where the volume keeps one
/// that differs, its short (8.3) spelling, each in the compared spelling. Input: the absolute
/// path. Output: one or two spellings.
fn spellings(folder: &Path) -> Vec<String> {
    let wide: Vec<u16> = folder.as_os_str().encode_wide().collect();
    let long = lowered(&wide);
    let named: Vec<u16> = wide.into_iter().chain([0]).collect();
    let mut units = vec![0u16; IMAGE_UNITS_MAX];
    let len =
        unsafe { GetShortPathNameW(named.as_ptr(), units.as_mut_ptr(), IMAGE_UNITS_MAX as u32) }
            as usize;
    let short = (len > 0 && len < IMAGE_UNITS_MAX).then(|| lowered(&units[..len]));
    let mut found = vec![long];
    if let Some(short) = short.filter(|short| *short != found[0]) {
        found.push(short);
    }
    found
}

/// Text in the spelling every compare uses: lower case with backslashes, so case and slash
/// direction never hide a path. Input: UTF-16 units. Output: the text.
fn lowered(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
        .to_lowercase()
        .replace('/', "\\")
}

/// Tells whether text names one of the spellings as a whole path part: each place it shows
/// is followed by a backslash, a quote, a space or the end, so a sibling whose name only
/// starts the same is not named. Inputs: the lowered text and the spellings. Output: the
/// answer.
fn names(text: &str, spellings: &[String]) -> bool {
    spellings.iter().any(|spelling| {
        text.match_indices(spelling.as_str()).any(|(at, _)| {
            spelling.ends_with('\\')
                || matches!(
                    text[at + spelling.len()..].chars().next(),
                    None | Some('\\' | '"' | ' ')
                )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::runs_under;
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
        assert_eq!(runs_under(&here), Ok(true));
        assert_eq!(runs_under(std::slice::from_ref(&copy)), Ok(true));
        assert_eq!(runs_under(&[plain(other.path())]), Ok(false));
        child.kill().unwrap();
        child.wait().unwrap();
        drop(child);
        assert_eq!(runs_under(&here), Ok(false));
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
            assert_eq!(runs_under(&here), Ok(true), "{named}");
            child.kill().unwrap();
            child.wait().unwrap();
        }
        let mut sibling = naming(&format!("{}old\\cli.js", folder.display()));
        assert_eq!(runs_under(&here), Ok(false));
        sibling.kill().unwrap();
        sibling.wait().unwrap();
    }

    /// A folder named without its drive is a caller's mistake, never a folder no program
    /// runs from.
    #[test]
    #[should_panic]
    fn a_relative_folder_is_refused() {
        let _ = runs_under(&[PathBuf::from("relative")]);
    }
}
