use super::last_error;
use super::owner::{token_user, user_sid};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use windows_sys::Wdk::System::Threading::{
    NtQueryInformationProcess, ProcessBasicInformation, ProcessCommandLineInformation,
};
use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    STILL_ACTIVE, UNICODE_STRING,
};
use windows_sys::Win32::Security::TOKEN_QUERY;
use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_UNKNOWN;
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, IsWow64Process2, OpenProcess, OpenProcessToken, PEB,
    PROCESS_BASIC_INFORMATION, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_VM_READ, QueryFullProcessImageNameW, RTL_USER_PROCESS_PARAMETERS,
};
use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

const PROCESSES_MAX: usize = 65536;
const IMAGE_UNITS_MAX: usize = 32768;
const COMMAND_WORDS: usize = (size_of::<UNICODE_STRING>() + 2 * 32768) / 8;
const COMMAND_UNITS_MAX: usize = 32768;
const CURRENT_DIRECTORY: usize =
    std::mem::offset_of!(RTL_USER_PROCESS_PARAMETERS, Reserved2) + 5 * size_of::<usize>();
const _: () = assert!(size_of::<usize>() != 8 || CURRENT_DIRECTORY == 0x38);
const HOSTS: [&str; 6] = [
    "node.exe",
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "bun.exe",
    "deno.exe",
];

/// Why the scan counted one process as running one of the folders: its program lies there,
/// its command line names one, it is a script host whose command line could not be read, or
/// a script host that works in one or names one by a relative path ([`counted`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    Image,
    Command,
    Unread,
    Hosted,
}

/// A folder or file the scan looks for, in the compared spelling. A folder is named only by
/// a path under it; a file also where an argument ends, as `codex.js --flag` names its file.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Spelling {
    text: String,
    file: bool,
}

/// Tells whether a live process of this user runs from one of the folders or files, or names
/// one in its command line as `node.exe` names its script, also by a relative path or by
/// working in it; another account's process and one that has exited are skipped.
/// Input: absolute folders and files in their real spelling. Output: the answer, or the
/// Windows error code when the snapshot or this user's SID cannot be read or holds too many.
pub fn runs_under(folders: &[PathBuf]) -> Result<bool, u32> {
    scan(folders).map(|hit| hit.is_some())
}

/// The work of [`runs_under`], keeping the first process counted and why, so a row can name
/// it. Input: absolute folders and files. Output: the process id, why it counted and the
/// NTSTATUS of its command-line read (0 once read), none when no process counts, or the error.
fn scan(folders: &[PathBuf]) -> Result<Option<(u32, Seen, i32)>, u32> {
    assert!(folders.iter().all(|folder| folder.is_absolute()));
    let user = user_sid()?;
    let spellings: Vec<Spelling> = folders
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
                Ok(None)
            } else {
                Err(code)
            };
        }
        let id = entry.th32ProcessID;
        if let Some((why, status)) = seen(id, &user, &spellings, &mut units, &mut command) {
            return Ok(Some((id, why, status)));
        }
        found = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) } != 0;
    }
    Err(ERROR_INSUFFICIENT_BUFFER)
}

/// Judges one process, keeping its first handle open until the end so its id names the same
/// process when a script host is opened again to read its working directory, and a host
/// that ended while it was judged, so its reads failed, is not counted. Relies on the
/// opened handle being null on failure or a new handle this caller owns alone. Inputs: the
/// process id, this user's SID, the spellings and the buffers. Output: why it counts, with
/// its command-line read status, or none when it cannot be opened, belongs to another
/// account, has exited, or names none of the spellings.
fn seen(
    id: u32,
    user: &[u8],
    spellings: &[Spelling],
    units: &mut [u16],
    command: &mut [u64],
) -> Option<(Seen, i32)> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, id) };
    if process.is_null() {
        return None;
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    if owner(&process).as_deref() != Some(user) || exited(&process) {
        return None;
    }
    let image = image(&process, units);
    if image
        .as_deref()
        .is_some_and(|image| names(image, spellings))
    {
        return Some((Seen::Image, 0));
    }
    let line = command_line(&process, command);
    if line
        .as_deref()
        .is_ok_and(|units| names(&lowered(units), spellings))
    {
        return Some((Seen::Command, 0));
    }
    if !host(image.as_deref()) {
        return None;
    }
    let hit = match line {
        Ok(_) => None,
        Err(status) => Some((Seen::Unread, status)),
    };
    hit.filter(|_| !exited(&process))
}

/// Tells whether a script host counts as running one of the spellings from where it works:
/// its working directory lies in one, or a path part of its arguments, made absolute
/// against that directory, lies in one or names one, or cannot be made absolute, as a
/// relative one cannot while the directory is unread. Inputs: the lowered working directory, ending in a backslash, the path parts
/// (none when the command line could not be split) and the spellings. Output: the answer.
fn counted(directory: Option<&str>, paths: Option<&[String]>, spellings: &[Spelling]) -> bool {
    let Some(paths) = paths else {
        return true;
    };
    if directory.is_some_and(|directory| names(directory, spellings)) {
        return true;
    }
    paths.iter().any(|path| {
        resolved(directory, path).is_none_or(|path| names(&format!("{path}\\"), spellings))
    })
}

/// The path part of one argument: a flag gives the value after its first `=`, as
/// `--require=.\x.js` does, and is skipped without one. Input: the lowered argument.
/// Output: the path part, none for a bare flag or an empty argument.
fn part(argument: String) -> Option<String> {
    let path = match argument.strip_prefix('-') {
        Some(flag) => flag.split_once('=')?.1.to_owned(),
        None => argument,
    };
    (!path.is_empty()).then_some(path)
}

/// A path part made absolute as Windows makes it: `c:\x` and `\\server\share\x` as they
/// are, `\x` from the working directory's drive or share root, `c:x` only on that drive,
/// anything else under the directory, with `.` and `..` folded. Inputs: the lowered
/// directory, ending in a backslash, none when unread, and the path part. Output: the path,
/// or none when it needs a directory that is unread or another drive's.
fn resolved(directory: Option<&str>, path: &str) -> Option<String> {
    assert!(directory.is_none_or(|directory| directory.ends_with('\\')));
    let bytes = path.as_bytes();
    let joined = if path.starts_with(r"\\") || (bytes.len() >= 3 && &bytes[1..3] == br":\") {
        path.to_owned()
    } else {
        let directory = directory?;
        if let Some(rest) = path.strip_prefix('\\') {
            format!("{}\\{rest}", root(directory))
        } else if bytes.len() >= 2 && bytes[1] == b':' {
            if !directory.starts_with(path.get(..2)?) {
                return None;
            }
            format!("{directory}{}", path.get(2..)?)
        } else {
            format!("{directory}{path}")
        }
    };
    let root = root(&joined);
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.get(root.len()..)?.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    Some(format!("{root}\\{}", parts.join("\\")))
}

/// The drive (`c:`) or share (`\\server\share`) a lowered absolute path starts with.
/// Input: the path. Output: the root, without a closing backslash.
fn root(directory: &str) -> &str {
    if let Some(share) = directory.strip_prefix("\\\\") {
        let end = share
            .match_indices('\\')
            .nth(1)
            .map_or(share.len(), |(at, _)| at);
        &directory[..2 + end]
    } else {
        directory.get(..2).unwrap_or(directory)
    }
}

/// A command line split as programs built with the C runtime split theirs
/// (`CommandLineToArgvW`), before any slash is turned, without the program itself, each
/// argument then lowered. Input: the command line's UTF-16 units. Output: the arguments, or
/// none when Windows cannot split it.
fn arguments(units: &[u16]) -> Option<Vec<String>> {
    if units
        .iter()
        .all(|&unit| unit == u16::from(b' ') || unit == u16::from(b'\t'))
    {
        return Some(Vec::new());
    }
    if units.len() > COMMAND_UNITS_MAX || units.contains(&0) {
        return None;
    }
    let wide: Vec<u16> = units.iter().copied().chain([0]).collect();
    let mut count = 0i32;
    let list = Argv(unsafe { CommandLineToArgvW(wide.as_ptr(), &mut count) });
    if list.0.is_null() {
        return None;
    }
    let count = usize::try_from(count).ok()?;
    assert!(count <= COMMAND_UNITS_MAX);
    let mut found = Vec::with_capacity(count.saturating_sub(1));
    for at in 1..count {
        let argument = unsafe { *list.0.add(at) };
        let length = (0..wide.len()).find(|&unit| unsafe { *argument.add(unit) } == 0)?;
        found.push(lowered(unsafe {
            std::slice::from_raw_parts(argument, length)
        }));
    }
    Some(found)
}

/// The argument list `CommandLineToArgvW` allocates, freed with `LocalFree` on every path.
struct Argv(*mut windows_sys::core::PWSTR);

impl Drop for Argv {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0.cast()) };
        }
    }
}

/// The working directory of a process, read from its process environment block, which
/// Windows does not document (64-bit layout, [`CURRENT_DIRECTORY`]). A refusal, a 32-bit
/// process or a short read gives none, and its script host then counts (fail closed) only
/// with a relative path part: an elevated or 32-bit bare REPL in a folder stays uncovered
/// rather than blocking every update. Input: the process id. Output: the lowered directory.
fn working_directory(id: u32) -> Option<String> {
    let rights = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ;
    let process = unsafe { OpenProcess(rights, 0, id) };
    if process.is_null() {
        return None;
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let (mut machine, mut native) = (IMAGE_FILE_MACHINE_UNKNOWN, IMAGE_FILE_MACHINE_UNKNOWN);
    let asked = unsafe { IsWow64Process2(process.as_raw_handle(), &mut machine, &mut native) };
    if asked == 0 || machine != IMAGE_FILE_MACHINE_UNKNOWN {
        return None;
    }
    let mut basic = PROCESS_BASIC_INFORMATION::default();
    let mut written = 0u32;
    let status = unsafe {
        NtQueryInformationProcess(
            process.as_raw_handle(),
            ProcessBasicInformation,
            (&raw mut basic).cast(),
            size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            &mut written,
        )
    };
    if status < 0 || basic.PebBaseAddress.is_null() {
        return None;
    }
    let at = basic.PebBaseAddress as usize + std::mem::offset_of!(PEB, ProcessParameters);
    let parameters: usize = remote(&process, at)?;
    if parameters == 0 {
        return None;
    }
    let path: UNICODE_STRING = remote(&process, parameters + CURRENT_DIRECTORY)?;
    let bytes = usize::from(path.Length);
    let fits = bytes % 2 == 0 && path.Length <= path.MaximumLength;
    if !fits || bytes == 0 || bytes / 2 > COMMAND_UNITS_MAX || path.Buffer.is_null() {
        return None;
    }
    let mut units = vec![0u16; bytes / 2];
    read(
        &process,
        path.Buffer as usize,
        units.as_mut_ptr().cast(),
        bytes,
    )?;
    let spelled = lowered(&units);
    let mut directory = match spelled.strip_prefix("\\\\?\\") {
        Some(rest) => rest
            .strip_prefix("unc\\")
            .map_or_else(|| rest.to_owned(), |share| format!("\\\\{share}")),
        None => spelled,
    };
    if !directory.ends_with('\\') {
        directory.push('\\');
    }
    Some(directory)
}

/// One value of plain data read from another process's memory, exactly its size. Inputs:
/// the process handle and the address. Output: the value, or none on any short read.
fn remote<T: Default>(process: &OwnedHandle, at: usize) -> Option<T> {
    let mut value = T::default();
    read(process, at, (&raw mut value).cast(), size_of::<T>())?;
    Some(value)
}

/// Reads bytes of another process's memory into a buffer of this one. Relies on `into`
/// holding `bytes` writable bytes. Inputs: the handle, the address, the buffer and its
/// size. Output: some only when Windows read exactly that many bytes.
fn read(
    process: &OwnedHandle,
    at: usize,
    into: *mut core::ffi::c_void,
    bytes: usize,
) -> Option<()> {
    let mut done = 0usize;
    let ok = unsafe {
        ReadProcessMemory(
            process.as_raw_handle(),
            at as *const _,
            into,
            bytes,
            &mut done,
        )
    };
    (ok != 0 && done == bytes).then_some(())
}

/// Tells whether a process has ended, read from its exit code, so a process whose handle
/// someone still holds is not counted as running. An exit code that cannot be read reads
/// as still running. Input: the process handle. Output: the answer.
fn exited(process: &OwnedHandle) -> bool {
    let mut code = 0u32;
    let read = unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) };
    read != 0 && code != STILL_ACTIVE as u32
}

/// Tells whether a program is a script host that can run a harness from a folder by naming
/// its script, so a command line of it that cannot be read counts as running it. Input: the
/// lowered program file name, none when unread. Output: the answer.
fn host(image: Option<&str>) -> bool {
    image
        .and_then(|image| image.rsplit('\\').next())
        .is_some_and(|name| HOSTS.contains(&name))
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

/// A process's command line as Windows keeps it, read by ntdll into a buffer that holds the
/// longest one Windows allows. Relies on Windows writing the string's text inside that
/// buffer. Inputs: the process handle and the [`COMMAND_WORDS`] buffer. Output: the UTF-16
/// units, or the NTSTATUS of a read that failed.
fn command_line(process: &OwnedHandle, command: &mut [u64]) -> Result<Vec<u16>, i32> {
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
        return Err(status);
    }
    let string = unsafe { &*command.as_ptr().cast::<UNICODE_STRING>() };
    let length = usize::from(string.Length);
    assert_eq!(length % 2, 0);
    if length == 0 {
        return Ok(Vec::new());
    }
    let start = command.as_ptr() as usize;
    let text = string.Buffer as usize;
    assert!(text >= start + size_of::<UNICODE_STRING>());
    assert!(text + length <= start + bytes as usize);
    let units = unsafe { std::slice::from_raw_parts(string.Buffer, length / 2) };
    Ok(units.to_vec())
}

/// The spellings a folder or file is looked for in: its own and, where the volume keeps one
/// that differs, its short (8.3) spelling, each in the compared spelling and marked a file
/// when it is one now. Input: the absolute path. Output: one or two spellings.
fn spellings(folder: &Path) -> Vec<Spelling> {
    let file = folder.is_file();
    let wide: Vec<u16> = folder.as_os_str().encode_wide().collect();
    let long = lowered(&wide);
    let named: Vec<u16> = wide.into_iter().chain([0]).collect();
    let mut units = vec![0u16; IMAGE_UNITS_MAX];
    let len =
        unsafe { GetShortPathNameW(named.as_ptr(), units.as_mut_ptr(), IMAGE_UNITS_MAX as u32) }
            as usize;
    let short = (len > 0 && len < IMAGE_UNITS_MAX).then(|| lowered(&units[..len]));
    let mut found = vec![Spelling { text: long, file }];
    if let Some(short) = short.filter(|short| *short != found[0].text) {
        found.push(Spelling { text: short, file });
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

/// Tells whether text names one of the spellings as a whole path part: a folder only where
/// a backslash follows, so a path under it; a file also where a quote, a space or the end
/// follows. A sibling whose name only starts the same, `harness old` beside `harness`, is not
/// named. Inputs: the lowered text and the spellings. Output: the answer.
fn names(text: &str, spellings: &[Spelling]) -> bool {
    spellings.iter().any(|spelling| {
        text.match_indices(spelling.text.as_str()).any(|(at, _)| {
            let next = text[at + spelling.text.len()..].chars().next();
            spelling.text.ends_with('\\') || matches!(next, None | Some('\\' | '"' | ' '))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CURRENT_DIRECTORY, HOSTS, Spelling, arguments, counted, host, lowered, part, resolved,
        runs_under, scan,
    };
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};

    const CREATE_SUSPENDED: u32 = 0x0000_0004;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// A started child that is killed and waited for once it goes out of scope, so a failed
    /// assert leaves no suspended process behind; errors there are ignored, since a child
    /// that already ended is no failure.
    struct Held(Child);

    impl Held {
        /// Ends the child now and waits for it, for a row that checks what follows its end.
        /// Output: Ok, or the error naming the step and the process.
        fn end(&mut self) -> Result<(), String> {
            let id = self.0.id();
            self.0
                .kill()
                .map_err(|error| format!("kill {id}: {error}"))?;
            self.0
                .wait()
                .map(drop)
                .map_err(|error| format!("wait {id}: {error}"))
        }
    }

    impl Drop for Held {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A fresh temporary folder, removed when the returned guard drops.
    /// Output: the folder, or the error naming the step.
    fn temporary() -> Result<tempfile::TempDir, String> {
        tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))
    }

    /// The System32 folder of this Windows. Output: the folder, or the error naming the
    /// variable that is not set.
    fn system32() -> Result<PathBuf, String> {
        let root = std::env::var_os("SystemRoot").ok_or("SystemRoot is not set")?;
        Ok(PathBuf::from(root).join("System32"))
    }

    /// The engine's spelling of a canonical path: a verbatim UNC path as `\\server\share`,
    /// another verbatim path without its prefix, and a plain path as it is, in native units.
    /// Input: the path. Output: the spelling.
    fn plain_text(path: &Path) -> PathBuf {
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
        let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
        let kept: Vec<u16> = match wide.strip_prefix(unc.as_slice()) {
            Some(rest) => r"\\".encode_utf16().chain(rest.iter().copied()).collect(),
            None => wide
                .strip_prefix(verbatim.as_slice())
                .unwrap_or(&wide)
                .to_vec(),
        };
        PathBuf::from(OsString::from_wide(&kept))
    }

    /// The real spelling of a folder as the engine passes it: long names, in
    /// [`plain_text`]'s spelling. Input: the folder. Output: the spelling, or the error.
    fn plain(folder: &Path) -> Result<PathBuf, String> {
        std::fs::canonicalize(folder)
            .map(|real| plain_text(&real))
            .map_err(|error| format!("canonicalize {}: {error}", folder.display()))
    }

    /// Starts a program held suspended, so it never reads its arguments or its language
    /// files. Inputs: the program and its arguments. Output: the held child, or the error.
    fn held(program: &Path, arguments: &[&OsStr]) -> Result<Held, String> {
        Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
            .spawn()
            .map(Held)
            .map_err(|error| format!("spawn {}: {error}", program.display()))
    }

    /// Starts the System32 `cmd.exe`, a script host, held suspended in a working directory.
    /// Inputs: its arguments and the directory. Output: the held child, or the error.
    fn host_in(arguments: &[&OsStr], directory: &Path) -> Result<Held, String> {
        let cmd = system32()?.join("cmd.exe");
        Command::new(&cmd)
            .args(arguments)
            .current_dir(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
            .spawn()
            .map(Held)
            .map_err(|error| {
                format!(
                    "spawn {} in {}: {error}",
                    cmd.display(),
                    directory.display()
                )
            })
    }

    /// Makes a folder and any missing parents. Input: the folder. Output: it, or the error.
    fn made(folder: PathBuf) -> Result<PathBuf, String> {
        std::fs::create_dir_all(&folder)
            .map_err(|error| format!("create {}: {error}", folder.display()))?;
        Ok(folder)
    }

    /// Starts the System32 ping held suspended with one more argument, so it never reads it.
    /// Input: the argument. Output: the held child, or the error.
    fn naming(argument: &OsStr) -> Result<Held, String> {
        let ping = system32()?.join("PING.EXE");
        held(&ping, &[OsStr::new("-n"), OsStr::new("60"), argument])
    }

    /// A path respelled unit by unit, kept in native units. Inputs: the path and the change
    /// of one UTF-16 unit. Output: the spelling.
    fn respelled(path: &Path, change: fn(u16) -> u16) -> OsString {
        let units: Vec<u16> = path.as_os_str().encode_wide().map(change).collect();
        OsString::from_wide(&units)
    }

    /// One UTF-16 unit with an ASCII lower-case letter put in upper case.
    /// Input: the unit. Output: the unit, changed only for such a letter.
    fn upper(unit: u16) -> u16 {
        match u8::try_from(unit) {
            Ok(byte) if byte.is_ascii_lowercase() => u16::from(byte.to_ascii_uppercase()),
            _ => unit,
        }
    }

    /// One UTF-16 unit with a backslash put as a forward slash.
    /// Input: the unit. Output: the unit, changed only for a backslash.
    fn slashed(unit: u16) -> u16 {
        if unit == u16::from(b'\\') {
            u16::from(b'/')
        } else {
            unit
        }
    }

    /// The short (8.3) spelling of a folder, or the folder itself where the volume keeps no
    /// short names. Input: the folder. Output: the spelling, or the Windows error.
    fn short(folder: &Path) -> Result<PathBuf, String> {
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
        let wide: Vec<u16> = folder.as_os_str().encode_wide().chain([0]).collect();
        let mut units = vec![0u16; 32768];
        let len = unsafe { GetShortPathNameW(wide.as_ptr(), units.as_mut_ptr(), 32768) };
        if len == 0 || len as usize >= units.len() {
            let error = std::io::Error::last_os_error();
            return Err(format!("short name of {}: {error}", folder.display()));
        }
        Ok(PathBuf::from(OsString::from_wide(&units[..len as usize])))
    }

    /// A verbatim path from canonicalize reads as the engine names it: a UNC share as
    /// `\\server\share`, a drive path without its prefix, a plain path unchanged.
    #[test]
    fn a_canonical_path_reads_in_the_spelling_the_engine_passes() {
        for (canonical, spelled) in [
            (r"\\?\UNC\server\share\x", r"\\server\share\x"),
            (r"\\?\C:\x", r"C:\x"),
            (r"C:\x", r"C:\x"),
        ] {
            let read = plain_text(Path::new(canonical));
            assert_eq!(read, PathBuf::from(spelled), "{canonical}");
        }
    }

    /// A program started from a folder outside any runtime, held suspended so it never needs
    /// its language files, runs under that folder and as that file, not under another
    /// folder, and under none once it has ended.
    #[test]
    fn a_program_started_from_a_folder_runs_under_it_until_it_ends() -> Result<(), String> {
        let made = temporary()?;
        let other = temporary()?;
        let folder = plain(made.path())?;
        let ping = system32()?.join("PING.EXE");
        let copy = folder.join("PING.EXE");
        std::fs::copy(&ping, &copy)
            .map_err(|error| format!("copy {} to {}: {error}", ping.display(), copy.display()))?;
        let arguments = [OsStr::new("-n"), OsStr::new("60"), OsStr::new("127.0.0.1")];
        let mut child = held(&copy, &arguments)?;
        let here = [folder.clone()];
        assert_eq!(runs_under(&here), Ok(true));
        assert_eq!(runs_under(std::slice::from_ref(&copy)), Ok(true));
        assert_eq!(runs_under(&[plain(other.path())?]), Ok(false));
        child.end()?;
        drop(child);
        assert_eq!(runs_under(&here), Ok(false));
        Ok(())
    }

    /// A program whose file lies elsewhere runs under a folder while its command line names a
    /// file there, as `node.exe` runs a script, in any case, with forward slashes or in the
    /// folder's short spelling; a sibling folder whose name only starts the same does not.
    #[test]
    fn a_program_whose_command_line_names_a_file_under_a_folder_runs_under_it() -> Result<(), String>
    {
        let made = temporary()?;
        let folder = plain(made.path())?;
        let script = folder.join("node_modules").join("tool").join("cli.js");
        let shortened = short(&folder)?
            .join("node_modules")
            .join("tool")
            .join("cli.js");
        let here = [folder.clone()];
        for named in [
            script.clone().into_os_string(),
            respelled(&script, upper),
            respelled(&script, slashed),
            shortened.into_os_string(),
        ] {
            let mut child = naming(&named)?;
            assert_eq!(runs_under(&here), Ok(true), "{}", named.display());
            child.end()?;
        }
        let mut sibling = folder.into_os_string();
        sibling.push(r"old\cli.js");
        let _sibling = naming(&sibling)?;
        assert_eq!(runs_under(&here), Ok(false));
        Ok(())
    }

    /// A program that named a folder and has ended counts as running it no more while its
    /// handle is still held, scanned many times so a process that only sometimes counts is
    /// named with why.
    #[test]
    fn an_ended_program_whose_handle_is_held_runs_under_nothing() -> Result<(), String> {
        let made = temporary()?;
        let folder = plain(made.path())?;
        let script = folder.join("node_modules").join("tool").join("cli.js");
        let mut child = naming(script.as_os_str())?;
        let mine = child.0.id();
        child.end()?;
        let here = [folder.clone()];
        for round in 0..50 {
            let hit = scan(&here);
            assert_eq!(hit, Ok(None), "round {round}, ended child {mine}");
        }
        drop(child);
        Ok(())
    }

    /// A script host started elsewhere runs under a folder while a relative argument of its
    /// command line resolves into it from its working directory, as `node ..\x\cli.js` does;
    /// a relative argument that resolves elsewhere does not.
    #[test]
    fn a_script_host_naming_a_relative_script_under_a_folder_runs_under_it() -> Result<(), String> {
        let made_folder = temporary()?;
        let made_outside = temporary()?;
        let folder = plain(made_folder.path())?;
        let outside = plain(made_outside.path())?;
        assert_eq!(
            folder.parent(),
            outside.parent(),
            "both temporary folders share a parent"
        );
        let name = folder.file_name().ok_or("the folder has a name")?;
        let mut into = OsString::from(r"..\");
        into.push(name);
        into.push(r"\cli.js");
        let here = [folder.clone()];
        let mut child = host_in(
            &[OsStr::new("/d"), OsStr::new("/c"), into.as_os_str()],
            &outside,
        )?;
        assert_eq!(runs_under(&here), Ok(true), "{}", into.display());
        child.end()?;
        let _beside = host_in(
            &[OsStr::new("/d"), OsStr::new("/c"), OsStr::new(r".\cli.js")],
            &outside,
        )?;
        assert_eq!(scan(&here), Ok(None));
        Ok(())
    }

    /// A script host whose working directory lies in a folder runs under it whatever its
    /// arguments, as a bare REPL or `node -e "require('./cli.js')"` started there does; one
    /// working elsewhere with no argument does not.
    #[test]
    fn a_script_host_working_in_a_folder_runs_under_it_whatever_it_runs() -> Result<(), String> {
        let made_folder = temporary()?;
        let made_outside = temporary()?;
        let folder = plain(made_folder.path())?;
        let tool = made(folder.join("node_modules").join("tool"))?;
        let here = [folder.clone()];
        let mut child = host_in(&[], &tool)?;
        assert_eq!(runs_under(&here), Ok(true));
        child.end()?;
        let _elsewhere = host_in(&[], &plain(made_outside.path())?)?;
        assert_eq!(scan(&here), Ok(None));
        Ok(())
    }

    /// A sibling whose name is the folder's name, a space and more, `harness old` beside
    /// `harness`, is not under the folder, by its program file or by its command line; a
    /// path under the folder itself is.
    #[test]
    fn a_sibling_whose_name_only_starts_with_the_folder_and_a_space_is_not_under_it()
    -> Result<(), String> {
        let made_root = temporary()?;
        let root = plain(made_root.path())?;
        let folder = made(root.join("harness"))?;
        let sibling = made(root.join("harness old"))?;
        let ping = system32()?.join("PING.EXE");
        let copy = sibling.join("PING.EXE");
        std::fs::copy(&ping, &copy)
            .map_err(|error| format!("copy {} to {}: {error}", ping.display(), copy.display()))?;
        let here = [folder.clone()];
        let arguments = [OsStr::new("-n"), OsStr::new("60"), OsStr::new("127.0.0.1")];
        let _program = held(&copy, &arguments)?;
        assert_eq!(scan(&here), Ok(None), "{}", copy.display());
        let _named = naming(sibling.join("cli.js").as_os_str())?;
        assert_eq!(scan(&here), Ok(None));
        let _under = naming(folder.join("cli.js").as_os_str())?;
        assert_eq!(runs_under(&here), Ok(true));
        Ok(())
    }

    /// A script host counts by where it works and by the path parts it names: a working
    /// directory in the folder, a part into it or naming it whole, relative or absolute, and
    /// a relative part it cannot resolve, as with an unread working directory, an unsplit
    /// command line or another drive's `d:x`; an unread directory alone does not count.
    #[test]
    fn a_script_host_counts_by_where_it_works_and_the_paths_it_names() {
        let harness = [Spelling {
            text: r"c:\tools\harness".to_owned(),
            file: false,
        }];
        let cases: [(Option<&str>, Option<&[&str]>, bool); 14] = [
            (Some(r"c:\tools\harness\x\"), Some(&[]), true),
            (Some(r"c:\work\"), Some(&[]), false),
            (Some(r"c:\tools\"), Some(&[r"harness\cli.js"]), true),
            (Some(r"c:\tools\"), Some(&["harness"]), true),
            (Some(r"c:\work\"), Some(&[r"..\tools\harness\x.js"]), true),
            (
                Some(r"c:\work\"),
                Some(&[r"..\tools\harness old\x.js"]),
                false,
            ),
            (Some(r"c:\work\"), Some(&[r".\cli.js"]), false),
            (Some(r"c:\work\"), Some(&[r"c:\tools\harness"]), true),
            (None, Some(&[r"c:\tools\harness"]), true),
            (None, Some(&[r"c:\tools\other\x.js"]), false),
            (None, Some(&[]), false),
            (None, Some(&["x.js"]), true),
            (Some(r"c:\work\"), None, true),
            (Some(r"d:\work\"), Some(&["c:x.js"]), true),
        ];
        for (directory, paths, counts) in cases {
            let paths = paths.map(|paths| {
                paths
                    .iter()
                    .map(|path| (*path).to_owned())
                    .collect::<Vec<_>>()
            });
            let read = counted(directory, paths.as_deref(), &harness);
            assert_eq!(read, counts, "{directory:?} {paths:?}");
        }
    }

    /// A path part resolves as Windows resolves it against a working directory, an absolute
    /// one folded as it is, and only the path part of a flag is taken; the command line is
    /// split before any slash is turned.
    #[test]
    fn a_path_part_resolves_against_the_working_directory_as_windows_does() {
        let cases = [
            (Some(r"c:\a\b\"), r".\x\..\y", Some(r"c:\a\b\y")),
            (Some(r"c:\a\b\"), r"\x", Some(r"c:\x")),
            (Some(r"c:\a\"), r"..\..\..\x", Some(r"c:\x")),
            (Some(r"\\srv\share\a\"), r"\x", Some(r"\\srv\share\x")),
            (Some(r"\\srv\share\a\"), r"..\..\x", Some(r"\\srv\share\x")),
            (Some(r"c:\a\"), "c:x", Some(r"c:\a\x")),
            (Some(r"c:\a\"), "d:x", None),
            (Some(r"c:\a\"), r"d:\x\..\y", Some(r"d:\y")),
            (None, r"\\srv\share\x", Some(r"\\srv\share\x")),
            (None, r".\x", None),
            (None, r"\x", None),
        ];
        for (directory, path, made) in cases {
            let read = resolved(directory, path);
            assert_eq!(read.as_deref(), made, "{directory:?} {path}");
        }
        assert_eq!(
            part(r"--require=.\x.js".to_owned()).as_deref(),
            Some(r".\x.js")
        );
        assert_eq!(part("-e".to_owned()), None);
        assert_eq!(part(String::new()), None);
        assert_eq!(part(r"c:\x.js".to_owned()).as_deref(), Some(r"c:\x.js"));
        let wide = |text: &str| text.encode_utf16().collect::<Vec<u16>>();
        let split = arguments(&wide(r#""C:\node.exe" "./Tool/" ".\a b.js" --flag"#));
        let made = [r".\tool\", r".\a b.js", "--flag"].map(str::to_owned);
        assert_eq!(split.as_deref(), Some(&made[..]));
        assert_eq!(arguments(&[]), Some(Vec::new()));
    }

    /// The process environment block layout the working-directory read relies on, which
    /// Windows does not document: the parameters pointer at 0x20 of the PEB, the current
    /// directory at 0x38 and the image path at 0x60 of the parameters, on 64-bit Windows.
    #[test]
    fn the_working_directory_read_uses_the_64_bit_parameters_layout() {
        use windows_sys::Win32::System::Threading::{PEB, RTL_USER_PROCESS_PARAMETERS};
        assert_eq!(std::mem::offset_of!(PEB, ProcessParameters), 0x20);
        assert_eq!(CURRENT_DIRECTORY, 0x38);
        assert_eq!(
            std::mem::offset_of!(RTL_USER_PROCESS_PARAMETERS, ImagePathName),
            0x60
        );
    }

    /// An unread command line counts only for a script host, its file name read in any case,
    /// and never for another program or one whose file name is unread.
    #[test]
    fn only_a_script_host_counts_when_its_command_line_is_unread() {
        for name in HOSTS {
            assert!(host(Some(&format!("c:\\tools\\{name}"))), "{name}");
        }
        let upper: Vec<u16> = r"C:\Program Files\nodejs\NODE.EXE".encode_utf16().collect();
        assert!(host(Some(&lowered(&upper))));
        assert!(!host(Some(r"c:\windows\system32\ping.exe")));
        assert!(!host(Some(r"c:\tools\node.exe.bak")));
        assert!(!host(Some("node.exe.old")));
        assert!(!host(None));
    }

    /// A folder named without its drive is a caller's mistake, never a folder no program
    /// runs from.
    #[test]
    #[should_panic]
    fn a_relative_folder_is_refused() {
        let _ = runs_under(&[PathBuf::from("relative")]);
    }
}
