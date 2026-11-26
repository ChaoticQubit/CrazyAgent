use super::Appearance;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DEFAULTS: &str = "/usr/bin/defaults";
const GSETTINGS: &str = "/usr/bin/gsettings";
const WINDOWS_ROOT: &str = r"C:\Windows";
const PERSONALIZE: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const KDEGLOBALS_BYTES_MAX: u64 = 256 * 1024;

/// The system whose light or dark setting the reader asks.
/// Input: none. Output: one of the three desktop systems CrazyAgent runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Os {
    Macos,
    Linux,
    Windows,
}

/// The system this build runs on.
/// Input: none. Output: macOS, Windows, or Linux for every other Unix.
pub(super) fn this_os() -> Os {
    match std::env::consts::OS {
        "macos" => Os::Macos,
        "windows" => Os::Windows,
        _ => Os::Linux,
    }
}

/// Reads this computer's light or dark setting for Follow macOS and Follow system.
/// Input: none. Output: Dark when the system says dark, otherwise Light, including when
/// the reader is missing or fails and on a Linux desktop that is neither GNOME nor KDE.
pub fn system_appearance() -> Appearance {
    appearance_on(
        this_os(),
        &mut run,
        &|name| std::env::var(name).ok(),
        &read_bounded,
    )
}

/// Asks one system's setting through the given runner, environment and file reader.
/// Inputs: the system, a runner returning a child's stdout on success, an environment
/// lookup and a file reader. Output: the appearance; macOS runs defaults, Windows reg.exe,
/// Linux reads kdeglobals on KDE and runs gsettings elsewhere, each at an absolute path.
pub(super) fn appearance_on(
    os: Os,
    run: &mut dyn FnMut(&mut Command) -> Option<Vec<u8>>,
    var: &dyn Fn(&str) -> Option<String>,
    read: &dyn Fn(&Path) -> Option<String>,
) -> Appearance {
    match os {
        Os::Macos => run(&mut command(
            DEFAULTS,
            &["read", "-g", "AppleInterfaceStyle"],
        ))
        .map_or(Appearance::Light, |out| from_defaults(&out)),
        Os::Windows => run(&mut command(
            &reg_exe(var("SystemRoot").as_deref()),
            &["query", PERSONALIZE, "/v", "AppsUseLightTheme"],
        ))
        .map_or(Appearance::Light, |out| from_reg(&out)),
        Os::Linux if on_kde(var("XDG_CURRENT_DESKTOP").as_deref()) => kdeglobals(var)
            .and_then(|path| read(&path))
            .map_or(Appearance::Light, |text| from_kdeglobals(&text)),
        Os::Linux => run(&mut command(
            GSETTINGS,
            &["get", "org.gnome.desktop.interface", "color-scheme"],
        ))
        .map_or(Appearance::Light, |out| from_gsettings(&out)),
    }
}

/// Builds a reader command with no input and no error output.
/// Inputs: the program, which must be an absolute path so no PATH lookup can pick a
/// planted program, and its arguments. Output: the command, stdout piped.
fn command(program: &str, args: &[&str]) -> Command {
    assert!(
        absolute(program),
        "a reader program must be an absolute path: {program}"
    );
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    command
}

/// Starts the command through the spawn gate.
/// Input: the command. Output: its stdout when it exited with success, otherwise None.
fn run(command: &mut Command) -> Option<Vec<u8>> {
    kernel_processes::spawn_gate::output(command)
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout)
}

/// Whether a path is absolute on Unix (`/usr/bin/...`) or a Windows drive path (`C:\...`).
/// Input: the path text. Output: true for either form, on every build.
fn absolute(path: &str) -> bool {
    path.starts_with('/') || drive(path)
}

fn drive(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\'
}

/// The reg.exe path under the Windows folder.
/// Input: SystemRoot, if set. Output: `<root>\System32\reg.exe`; a root that is not a
/// plain drive path, or that holds `..`, falls back to `C:\Windows`.
fn reg_exe(root: Option<&str>) -> String {
    let root = root
        .filter(|root| drive(root) && !root.split('\\').any(|part| part == ".."))
        .unwrap_or(WINDOWS_ROOT);
    format!(r"{}\System32\reg.exe", root.trim_end_matches('\\'))
}

fn on_kde(desktop: Option<&str>) -> bool {
    desktop.is_some_and(|desktop| {
        desktop
            .split(':')
            .any(|part| part.eq_ignore_ascii_case("KDE"))
    })
}

/// Where KDE keeps its color scheme.
/// Input: an environment lookup. Output: `$XDG_CONFIG_HOME/kdeglobals` when that is
/// absolute, else `$HOME/.config/kdeglobals`, else None.
fn kdeglobals(var: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    var("XDG_CONFIG_HOME")
        .filter(|folder| absolute(folder))
        .map(PathBuf::from)
        .or_else(|| {
            var("HOME")
                .filter(|home| absolute(home))
                .map(|home| Path::new(&home).join(".config"))
        })
        .map(|folder| folder.join("kdeglobals"))
}

/// Reads a small regular file.
/// Input: the path. Output: its text, or None when it is missing, not a regular file,
/// larger than the cap or not UTF-8.
fn read_bounded(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > KDEGLOBALS_BYTES_MAX {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn from_defaults(out: &[u8]) -> Appearance {
    dark_when(out.trim_ascii() == b"Dark")
}

fn from_gsettings(out: &[u8]) -> Appearance {
    dark_when(out.trim_ascii() == b"'prefer-dark'")
}

/// Reads `AppsUseLightTheme    REG_DWORD    0x0` from reg.exe's answer.
/// Input: the answer bytes. Output: Dark for 0x0, otherwise Light.
fn from_reg(out: &[u8]) -> Appearance {
    let text = String::from_utf8_lossy(out);
    let value = text
        .lines()
        .find(|line| line.trim_start().starts_with("AppsUseLightTheme"))
        .and_then(|line| line.split_whitespace().last());
    dark_when(value == Some("0x0"))
}

/// Reads `ColorScheme=` in the `[General]` section of kdeglobals.
/// Input: the file text. Output: Dark when the scheme's name holds "dark", otherwise Light.
fn from_kdeglobals(text: &str) -> Appearance {
    let mut general = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            general = line == "[General]";
        } else if let Some(scheme) = line.strip_prefix("ColorScheme=").filter(|_| general) {
            return dark_when(scheme.to_ascii_lowercase().contains("dark"));
        }
    }
    Appearance::Light
}

fn dark_when(dark: bool) -> Appearance {
    if dark {
        Appearance::Dark
    } else {
        Appearance::Light
    }
}

#[cfg(test)]
mod tests;
