use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// The most text one clipboard copy takes, so a copy never holds a large buffer.
pub const CLIPBOARD_BYTES_MAX: usize = 64 * 1024;

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
    let mut child = command.spawn().map_err(|_| "clipboard_unavailable")?;
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
    let status = command.status().map_err(|_| "open_unavailable")?;
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
}
