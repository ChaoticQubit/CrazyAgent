use crate::project_setup::DOCUMENT_EXTENSIONS;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) mod unix_socket;

#[cfg(unix)]
pub(crate) use std::os::unix::fs::FileExt;

/// Positional whole-buffer file reads and writes on Windows, matching the Unix `FileExt`
/// names. Built on the standard `seek_read` and `seek_write`, which also move the file
/// cursor; callers read and write whole small files at fixed offsets, so that is harmless.
#[cfg(windows)]
pub(crate) trait FileExt {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()>;
    fn write_all_at(&self, buf: &[u8], offset: u64) -> std::io::Result<()>;
}

#[cfg(windows)]
impl FileExt for std::fs::File {
    fn read_exact_at(&self, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
        use std::os::windows::fs::FileExt as _;
        while !buf.is_empty() {
            match self.seek_read(buf, offset) {
                Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                Ok(read) => {
                    let rest = buf;
                    buf = &mut rest[read..];
                    offset += read as u64;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn write_all_at(&self, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
        use std::os::windows::fs::FileExt as _;
        while !buf.is_empty() {
            match self.seek_write(buf, offset) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(written) => {
                    buf = &buf[written..];
                    offset += written as u64;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// The Windows open flag that opens a reparse point itself rather than its target, the
/// equivalent of Unix `O_NOFOLLOW` on the last path component: a link then opens as a
/// link, whose metadata is not a regular file.
#[cfg(windows)]
pub(crate) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// The Windows open flag that lets a folder be opened as a handle.
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

/// The Windows file attribute of a reparse point: a junction, a link or a mount point.
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

/// What [`open_plain_beneath`] answers on Windows when the folder itself is a junction, a
/// link or a mount point, so nothing below it is read. Callers that show a reason match it.
#[cfg(windows)]
pub(crate) const BENEATH_FOLDER_LINKED: &str = "beneath_folder_linked";

/// What [`open_plain_beneath`] answers on Windows when a part below the folder is a
/// junction, a link or a mount point, is no longer directly inside the part before it, or
/// was opened under another name than the one asked for.
#[cfg(windows)]
pub(crate) const BENEATH_PATH_LINKED: &str = "beneath_path_linked";

/// Reports whether the engine's effective user owns the file the metadata describes.
/// Takes the metadata. Returns the Unix owner comparison; false where the platform has no
/// Unix owner to compare, so an ownership check refuses rather than passes.
#[cfg(unix)]
pub(crate) fn owned_by_effective_user(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.uid() == rustix::process::geteuid().as_raw()
}

/// Reports that no file is known to be owned by the engine's effective user, since the
/// platform has no Unix owner to compare. Takes the metadata, unused. Returns false, so
/// an ownership check refuses rather than passes.
#[cfg(not(unix))]
pub(crate) fn owned_by_effective_user(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// The most text one clipboard copy takes, so a copy never holds a large buffer.
pub const CLIPBOARD_BYTES_MAX: usize = 64 * 1024;

/// The longest project folder or context path handed to the opener checks.
pub const OPEN_PATH_BYTES_MAX: usize = 4096;

#[cfg(target_os = "macos")]
const OPEN: &str = "/usr/bin/open";
#[cfg(target_os = "macos")]
const PBCOPY: &str = "/usr/bin/pbcopy";

#[cfg(target_os = "linux")]
const LINUX_BIN_DIRS: &[&str] = &["/usr/bin", "/usr/local/bin", "/run/current-system/sw/bin"];

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The command that opens one file, folder or web link with the owner's default app:
/// `open` on macOS, `xdg-open` from a fixed folder on Linux, Explorer on Windows. The target
/// is one argument and never passes through a shell. Output: the command, or
/// `open_unavailable` when this machine has no opener or the target cannot be passed safely.
pub fn open_command(target: &OsStr) -> Result<Command, &'static str> {
    if target.is_empty() {
        return Err("open_unavailable");
    }
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new(OPEN);
        command.arg(target);
        Ok(command)
    }
    #[cfg(target_os = "linux")]
    {
        let mut command = Command::new(linux_tool("xdg-open").ok_or("open_unavailable")?);
        command.arg(target);
        Ok(command)
    }
    #[cfg(target_os = "windows")]
    {
        explorer(target, false)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err("open_unavailable")
    }
}

/// The command that shows one folder to the owner without opening anything inside it:
/// Finder with the folder selected on macOS, Explorer with it selected on Windows, the
/// folder itself through `xdg-open` on Linux. Output: the command, or `open_unavailable`.
pub fn reveal_command(folder: &Path) -> Result<Command, &'static str> {
    if !folder.is_absolute() {
        return Err("open_unavailable");
    }
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new(OPEN);
        command.arg("-R").arg(folder);
        Ok(command)
    }
    #[cfg(target_os = "linux")]
    {
        open_command(folder.as_os_str())
    }
    #[cfg(target_os = "windows")]
    {
        explorer(folder.as_os_str(), true)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err("open_unavailable")
    }
}

/// The command that reads text on its standard input into the clipboard: `pbcopy` on macOS,
/// `wl-copy` on Wayland or `xclip` or `xsel` on X11 from fixed folders on Linux, `clip.exe`
/// on Windows. Output: the command, or `clipboard_unavailable` when there is none.
pub fn clipboard_command() -> Result<Command, &'static str> {
    #[cfg(target_os = "macos")]
    {
        Ok(Command::new(PBCOPY))
    }
    #[cfg(target_os = "linux")]
    {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
        let x11 = std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty());
        if wayland && let Some(tool) = linux_tool("wl-copy") {
            return Ok(Command::new(tool));
        }
        if x11 && let Some(tool) = linux_tool("xclip") {
            let mut command = Command::new(tool);
            command.args(["-selection", "clipboard", "-in"]);
            return Ok(command);
        }
        if x11 && let Some(tool) = linux_tool("xsel") {
            let mut command = Command::new(tool);
            command.args(["--clipboard", "--input"]);
            return Ok(command);
        }
        Err("clipboard_unavailable")
    }
    #[cfg(target_os = "windows")]
    {
        Ok(Command::new(system32("clip.exe")))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err("clipboard_unavailable")
    }
}

/// Resolves a project's stored folder before it is shown to the owner. Input: the folder
/// the engine recorded. Output: its canonical path when it is an existing absolute folder of
/// at most `OPEN_PATH_BYTES_MAX` bytes, or `project_folder_missing`. Show it with [`reveal`],
/// never [`open`], so a folder named like an app bundle is not launched.
pub fn project_folder(stored: &str) -> Result<PathBuf, &'static str> {
    let path = Path::new(stored);
    if !path.is_absolute() || stored.len() > OPEN_PATH_BYTES_MAX {
        return Err("project_folder_missing");
    }
    let canonical = path.canonicalize().map_err(|_| "project_folder_missing")?;
    if !canonical.is_dir() {
        return Err("project_folder_missing");
    }
    Ok(canonical)
}

/// Resolves one context file to open with its default app: a relative path of plain parts,
/// a document type the project accepts, not a link, and a regular file still inside the
/// project folder. Inputs: the canonical project folder and the relative path.
/// Output: the file's canonical path, or `context_item_not_openable`.
pub fn context_file(folder: &Path, relative: &str) -> Result<PathBuf, &'static str> {
    let refused = "context_item_not_openable";
    let extension = Path::new(relative)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    if !extension.is_some_and(|value| DOCUMENT_EXTENSIONS.contains(&value.as_str())) {
        return Err(refused);
    }
    project_file(folder, relative).map_err(|_| refused)
}

/// Resolves one file inside a project folder for the owner's own editor, of any type:
/// a relative path of plain parts, not a link, and a regular file still inside the folder.
/// Inputs: the canonical folder and the relative path. Output: the file's canonical path,
/// `project_file_missing` when nothing is there, or `project_file_not_openable`.
pub fn project_file(folder: &Path, relative: &str) -> Result<PathBuf, &'static str> {
    assert!(folder.is_absolute());
    let refused = "project_file_not_openable";
    let path = Path::new(relative);
    if relative.is_empty()
        || relative.len() > OPEN_PATH_BYTES_MAX
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(refused);
    }
    let joined = folder.join(path);
    let metadata = match std::fs::symlink_metadata(&joined) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("project_file_missing");
        }
        Err(_) => return Err(refused),
    };
    if !metadata.file_type().is_file() {
        return Err(refused);
    }
    let canonical = joined.canonicalize().map_err(|_| refused)?;
    if !canonical.starts_with(folder) || !canonical.is_file() {
        return Err(refused);
    }
    Ok(canonical)
}

/// Reads a small UTF-8 regular file through one open that never follows a link in the last
/// path part and never waits on a FIFO, then checks the opened file itself, so nothing
/// swapped in after an earlier check is read. Takes the path and the byte limit. Returns
/// the text, or none for a link, any other non-regular file, more bytes or invalid UTF-8.
pub fn read_plain_file(path: &Path, limit: u64) -> Option<String> {
    let file = open_plain(path).ok()?;
    let opened = file.metadata().ok()?;
    if !opened.is_file() || opened.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > limit {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Opens a path to read without following a link in its last part and without waiting
/// for a FIFO writer. Takes the path. Returns the open file or the I/O error.
#[cfg(unix)]
pub(crate) fn open_plain(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags.bits() as i32)
        .open(path)
}

/// Opens a path below a folder the engine holds one part at a time, following no link in
/// any part from that folder down, so a folder an agent swapped for a link is refused, not
/// followed. Each part on the way opens as a folder and the last with `last`. Inputs: the
/// folder, opened by its path, the plain names below it and the last part's flags. Output:
/// the descriptor, or `EINVAL` for a part that is not a plain name.
#[cfg(unix)]
pub(crate) fn open_beneath(
    base: &Path,
    below: &Path,
    last: rustix::fs::OFlags,
) -> rustix::io::Result<std::os::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    const FOLDER: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    let parts = below
        .components()
        .map(|part| match part {
            Component::Normal(name) => Ok(name),
            _ => Err(rustix::io::Errno::INVAL),
        })
        .collect::<rustix::io::Result<Vec<_>>>()?;
    let mut opened = rustix::fs::open(base, FOLDER, Mode::empty())?;
    for (index, name) in parts.iter().enumerate() {
        let flags = if index + 1 == parts.len() {
            last
        } else {
            FOLDER
        };
        opened = rustix::fs::openat(&opened, *name, flags, Mode::empty())?;
    }
    Ok(opened)
}

/// Opens a file below a folder the engine holds to read, with no link followed from that
/// folder down and no FIFO waited on ([`open_beneath`]). On Windows every part opens as
/// itself ([`open_real_beneath`]), so a junction, link or mount point in any part, the
/// folder itself included, is refused wherever it leads, inside the folder or out of it.
/// Inputs: the folder and the path below it, plain names only. Output: the open file or the
/// I/O error.
pub(crate) fn open_plain_beneath(base: &Path, below: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use rustix::fs::OFlags;
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        Ok(std::fs::File::from(open_beneath(base, below, flags)?))
    }
    #[cfg(windows)]
    {
        let plain = below
            .components()
            .all(|part| matches!(part, Component::Normal(_)));
        if !plain || below.as_os_str().is_empty() {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        open_real_beneath(base, below)
    }
}

/// The Windows way of [`open_plain_beneath`]: opens the folder and then each name below it
/// as itself, never as a link's target, and goes on only while the opened thing is no
/// reparse point, its real place, read from its handle, is directly inside the real place
/// of the one before ([`name_child`]) and that place ends in the asked name
/// ([`name_ends_in`]). A part swapped between two opens so gives nothing, and neither does
/// a name Windows matches another way: an 8.3 short name, or a spelling with an ending dot
/// or space. Inputs: the folder and at least one plain name below it. Output: the open
/// file, `BENEATH_FOLDER_LINKED` when the folder itself is a reparse point,
/// `BENEATH_PATH_LINKED` for a part below it, or the I/O error.
#[cfg(windows)]
fn open_real_beneath(base: &Path, below: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    let refused =
        |code: &'static str| std::io::Error::new(std::io::ErrorKind::PermissionDenied, code);
    let folder = |path: &Path| {
        std::fs::OpenOptions::new()
            .access_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    };
    let mut held = folder(base)?;
    if reparse_point(&held)? {
        return Err(refused(BENEATH_FOLDER_LINKED));
    }
    let mut place = crazyagent_win_sandbox::final_name(&held)?;
    let mut path = base.to_path_buf();
    let mut parts = below.components().peekable();
    assert!(parts.peek().is_some(), "a path below a folder has a name");
    while let Some(part) = parts.next() {
        assert!(matches!(part, Component::Normal(_)), "plain names only");
        path.push(part);
        let opened = if parts.peek().is_some() {
            folder(&path)?
        } else {
            open_plain(&path)?
        };
        let found = crazyagent_win_sandbox::final_name(&opened)?;
        let asked: Vec<u16> = part.as_os_str().encode_wide().collect();
        let own = name_child(&place, &found) && name_ends_in(&found, &asked);
        if reparse_point(&opened)? || !own {
            return Err(refused(BENEATH_PATH_LINKED));
        }
        (held, place) = (opened, found);
    }
    Ok(held)
}

/// Tells whether an open file or folder is a reparse point: a junction, a link or a mount
/// point, opened as itself. Takes the open handle. Returns the answer or the I/O error.
#[cfg(windows)]
fn reparse_point(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::windows::fs::MetadataExt;
    Ok(file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// Tells whether a real place is directly inside a folder's real place: below it
/// ([`name_under`]) by exactly one name, with no further `\`. Inputs: the folder's name and
/// the found name, as the device names Windows gives for open handles, in UTF-16 units.
/// Output: the answer.
#[cfg(any(windows, test))]
fn name_child(folder: &[u16], found: &[u16]) -> bool {
    const SEPARATOR: u16 = b'\\' as u16;
    let start = folder.strip_suffix(&[SEPARATOR]).unwrap_or(folder).len() + 1;
    let child = name_under(folder, found) && !found[start..].contains(&SEPARATOR);
    assert!(!child || found.len() > start);
    child
}

/// Tells whether a real place ends in the asked name: its last name, the units after its
/// last `\`, is the asked name unit for unit, with ASCII letters taken in any case. A name
/// Windows matched another way, an 8.3 short name or a spelling whose ending dot or space
/// it drops, is never the asked name; a name that differs only in the case of a letter
/// outside ASCII is refused too. Inputs: the found name, as the device name Windows gives
/// for an open handle, and the asked name, both in UTF-16 units. Output: the answer.
#[cfg(any(windows, test))]
fn name_ends_in(found: &[u16], asked: &[u16]) -> bool {
    const SEPARATOR: u16 = b'\\' as u16;
    let last = found
        .rsplit(|unit| *unit == SEPARATOR)
        .next()
        .unwrap_or(found);
    let fold =
        |unit: &u16| u8::try_from(*unit).map_or(*unit, |byte| u16::from(byte.to_ascii_lowercase()));
    let same = !asked.is_empty()
        && last.len() == asked.len()
        && last
            .iter()
            .zip(asked)
            .all(|(have, want)| fold(have) == fold(want));
    assert!(!same || !asked.contains(&SEPARATOR));
    same
}

/// Tells whether a real place lies below a folder's real place, both as the device names
/// Windows gives for open handles: the folder's name, then a `\`, then at least one more
/// unit. Names are compared unit for unit, so a sibling whose name only starts with the
/// folder's, the folder itself and an empty folder name are never below. Inputs: the
/// folder's name and the found name, in UTF-16 units. Output: the answer.
#[cfg(any(windows, test))]
fn name_under(folder: &[u16], found: &[u16]) -> bool {
    const SEPARATOR: u16 = b'\\' as u16;
    let folder = folder.strip_suffix(&[SEPARATOR]).unwrap_or(folder);
    let under = !folder.is_empty()
        && found.len() > folder.len() + 1
        && found.starts_with(folder)
        && found[folder.len()] == SEPARATOR;
    assert!(!under || found.len() >= 3);
    under
}

/// Opens a path to read as the reparse point itself, never a link's target.
/// Takes the path. Returns the open file or the I/O error.
#[cfg(windows)]
pub(crate) fn open_plain(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Opens one file, folder or web link with the owner's default app and waits for the opener.
/// Input: the target, already checked by the caller. Output: Ok once the system took it,
/// `open_unavailable`, or `open_failed`. Explorer's exit code means nothing, so on Windows a
/// started Explorer counts as taken.
pub fn open(target: &OsStr) -> Result<(), &'static str> {
    finish(open_command(target)?)
}

/// Shows one folder to the owner and waits for the opener; see [`reveal_command`].
/// Output: Ok, `open_unavailable` or `open_failed`.
pub fn reveal(folder: &Path) -> Result<(), &'static str> {
    finish(reveal_command(folder)?)
}

/// Puts text on the clipboard. Input: at most `CLIPBOARD_BYTES_MAX` bytes. Output: Ok,
/// `clipboard_unavailable` when this machine has no clipboard tool, or `clipboard_failed`.
pub fn copy_text(text: &str) -> Result<(), &'static str> {
    if text.len() > CLIPBOARD_BYTES_MAX {
        return Err("clipboard_failed");
    }
    let mut command = clipboard_command()?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    quiet(&mut command);
    let mut child = crate::spawn_gate::spawn(&mut command).map_err(|_| "clipboard_unavailable")?;
    let written = child
        .stdin
        .take()
        .ok_or("clipboard_failed")
        .and_then(|mut stdin| {
            stdin
                .write_all(text.as_bytes())
                .map_err(|_| "clipboard_failed")
        });
    let status = child.wait().map_err(|_| "clipboard_failed")?;
    written?;
    if status.success() {
        Ok(())
    } else {
        Err("clipboard_failed")
    }
}

fn finish(mut command: Command) -> Result<(), &'static str> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    quiet(&mut command);
    let status = crate::spawn_gate::status(&mut command).map_err(|_| "open_unavailable")?;
    if status.success() || cfg!(target_os = "windows") {
        Ok(())
    } else {
        Err("open_failed")
    }
}

fn quiet(command: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = command;
    }
}

#[cfg(target_os = "linux")]
fn linux_tool(name: &str) -> Option<std::path::PathBuf> {
    LINUX_BIN_DIRS
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|path| path.is_file())
}

#[cfg(target_os = "windows")]
fn system_root() -> std::path::PathBuf {
    std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .filter(|root| root.is_absolute())
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
}

#[cfg(target_os = "windows")]
fn system32(program: &str) -> std::path::PathBuf {
    system_root().join("System32").join(program)
}

#[cfg(target_os = "windows")]
fn explorer(target: &OsStr, select: bool) -> Result<Command, &'static str> {
    use std::os::windows::process::CommandExt;
    let text = target.to_str().ok_or("open_unavailable")?;
    if text.contains('"') || text.chars().any(char::is_control) {
        return Err("open_unavailable");
    }
    let mut command = Command::new(system_root().join("explorer.exe"));
    if select {
        command.raw_arg(format!("/select,\"{text}\""));
    } else {
        command.raw_arg(format!("\"{text}\""));
    }
    Ok(command)
}

#[cfg(test)]
mod beneath_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openers_and_the_clipboard_use_this_systems_own_tools() {
        assert_eq!(open_command(OsStr::new("")).err(), Some("open_unavailable"));
        assert_eq!(
            reveal_command(Path::new("relative")).err(),
            Some("open_unavailable")
        );
        assert_eq!(
            copy_text(&"x".repeat(CLIPBOARD_BYTES_MAX + 1)),
            Err("clipboard_failed")
        );
        #[cfg(target_os = "macos")]
        {
            let open = open_command(OsStr::new("https://example.com/")).unwrap();
            assert_eq!(open.get_program(), OPEN);
            assert!(open.get_args().eq(["https://example.com/"]));
            let reveal = reveal_command(Path::new("/tmp")).unwrap();
            assert!(reveal.get_args().eq(["-R", "/tmp"]));
            assert_eq!(clipboard_command().unwrap().get_program(), PBCOPY);
        }
        #[cfg(target_os = "linux")]
        {
            match linux_tool("xdg-open") {
                Some(tool) => {
                    let open = open_command(OsStr::new("https://example.com/")).unwrap();
                    assert_eq!(open.get_program(), tool.as_os_str());
                    assert!(open.get_args().eq(["https://example.com/"]));
                    let reveal = reveal_command(Path::new("/tmp")).unwrap();
                    assert!(reveal.get_args().eq(["/tmp"]));
                }
                None => assert_eq!(
                    open_command(OsStr::new("/tmp")).err(),
                    Some("open_unavailable")
                ),
            }
            if let Ok(clipboard) = clipboard_command() {
                let program = Path::new(clipboard.get_program());
                assert!(LINUX_BIN_DIRS.iter().any(|dir| program.starts_with(dir)));
            }
        }
        #[cfg(target_os = "windows")]
        {
            let open = open_command(OsStr::new(r"C:\Users\mira\notes.md")).unwrap();
            assert!(
                open.get_program()
                    .to_string_lossy()
                    .ends_with("explorer.exe")
            );
            assert_eq!(
                open_command(OsStr::new("C:\\bad\"name")).err(),
                Some("open_unavailable")
            );
            let clipboard = clipboard_command().unwrap();
            assert!(
                clipboard
                    .get_program()
                    .to_string_lossy()
                    .ends_with("clip.exe")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_plain_file_is_read_in_one_open_that_follows_no_link_and_waits_on_no_fifo() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("config");
        std::fs::write(&file, "[core]\n").unwrap();
        assert_eq!(read_plain_file(&file, 7).as_deref(), Some("[core]\n"));
        assert_eq!(read_plain_file(&file, 6), None);
        let link = folder.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(read_plain_file(&link, 64), None);
        assert_eq!(read_plain_file(folder.path(), 64), None);
        let binary = folder.path().join("binary");
        std::fs::write(&binary, [0xff, 0xfe]).unwrap();
        assert_eq!(read_plain_file(&binary, 64), None);
        let fifo = folder.path().join("fifo");
        let made = crate::spawn_gate::status(Command::new("/usr/bin/mkfifo").arg(&fifo)).unwrap();
        assert!(made.success());
        let (sent, read) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sent.send(read_plain_file(&fifo, 64));
        });
        assert_eq!(
            read.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(None),
            "a FIFO is refused without waiting for a writer"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_project_file_for_the_editor_is_any_regular_file_inside_the_folder_and_never_a_link() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().canonicalize().unwrap();
        let folder = home.join("notes");
        std::fs::create_dir_all(folder.join("src/nested")).unwrap();
        std::fs::write(folder.join("src/app.rs"), "fn main() {}\n").unwrap();
        std::fs::write(folder.join("run.command"), "#!/bin/sh\n").unwrap();
        std::fs::write(home.join("outside.rs"), "kept outside").unwrap();
        std::os::unix::fs::symlink(home.join("outside.rs"), folder.join("src/escape.rs")).unwrap();
        std::os::unix::fs::symlink(&home, folder.join("up")).unwrap();
        assert_eq!(
            project_file(&folder, "src/app.rs"),
            Ok(folder.join("src/app.rs"))
        );
        assert_eq!(
            project_file(&folder, "run.command"),
            Ok(folder.join("run.command")),
            "the owner's editor only reads it, so any type opens"
        );
        assert_eq!(
            project_file(&folder, "src/added.rs"),
            Err("project_file_missing")
        );
        for refused in [
            "src/escape.rs",
            "up/outside.rs",
            "src/nested",
            "../outside.rs",
            "src/../../outside.rs",
            "/etc/hosts",
            "",
        ] {
            assert_eq!(
                project_file(&folder, refused),
                Err("project_file_not_openable"),
                "{refused}"
            );
        }
        let long = "a/".repeat(OPEN_PATH_BYTES_MAX / 2 + 1);
        assert_eq!(
            project_file(&folder, &long),
            Err("project_file_not_openable")
        );
        assert_eq!(
            context_file(&folder, "src/app.rs"),
            Err("context_item_not_openable"),
            "context files keep their document types"
        );
        assert_eq!(
            context_file(&folder, "src/escape.rs"),
            Err("context_item_not_openable")
        );
    }

    #[test]
    fn ownership_matches_the_engine_user_only_on_unix() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("owned");
        std::fs::write(&file, b"x").unwrap();
        let metadata = std::fs::symlink_metadata(&file).unwrap();
        assert_eq!(owned_by_effective_user(&metadata), cfg!(unix));
    }
}
