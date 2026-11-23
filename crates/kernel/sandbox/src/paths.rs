use crate::error::{SandboxError, invalid};
use std::io;
use std::path::{Component, Path, PathBuf};

/// The longest path, in bytes, a sandbox root or extra path may have.
pub const PATH_BYTES_MAX: usize = 1024;

/// The mount of the macOS data volume, under which every firmlinked folder (/Users,
/// /private, /Applications and the rest) has a second name.
#[cfg(target_os = "macos")]
pub const DATA_VOLUME: &str = "/System/Volumes/Data";

/// True when two paths name one existing entry (device and inode). Inputs: the paths.
/// Output: the answer, false when either does not resolve.
#[cfg(target_os = "macos")]
pub fn same_entry(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(left), std::fs::metadata(right)) {
        (Ok(left), Ok(right)) => (left.dev(), left.ino()) == (right.dev(), right.ino()),
        _ => false,
    }
}

/// One spelling for a path: on macOS a path under the data volume that names the same entry as
/// the path without that prefix is given without it, the spelling `canonicalize` gives the
/// short form, so both compare equal as text; on Windows a verbatim disk path is given plain
/// ([`windows_plain`]) and any other form is kept, so the plain-path checks refuse it. Input:
/// a path, resolved or not; only its text changes. Output: that spelling, or the path
/// unchanged.
pub fn one_spelling(real: PathBuf) -> PathBuf {
    #[cfg(target_os = "macos")]
    if let Ok(rest) = real.strip_prefix(DATA_VOLUME) {
        let short = Path::new("/").join(rest);
        if same_entry(&real, &short) {
            return short;
        }
    }
    #[cfg(windows)]
    if let Some(plain) = real.to_str().and_then(windows_plain) {
        return PathBuf::from(plain);
    }
    real
}

/// The plain Windows spelling of path text (rv-sec2 #717 C1): a verbatim disk path
/// `\\?\C:\...` gives `C:\...`, and plain text stays as it is, when the result passes
/// [`windows_clean_text`]. A UNC, device or other verbatim form, or text Win32 would
/// normalise to another name, gives none. Input: the text. Output: the plain text, or none.
pub fn windows_plain(text: &str) -> Option<&str> {
    let plain = text.strip_prefix(r"\\?\").unwrap_or(text);
    windows_clean_text(plain).then_some(plain)
}

/// The canonical form of an existing path. On macOS a path named through the data volume
/// comes back under its short name, so a root given in the long name is not canonical. On
/// Windows the verbatim `\\?\` prefix `canonicalize` adds to a drive path is removed; any other
/// verbatim form stays and fails [`clean_absolute`]. Input: the path. Output: the canonical
/// path, or the I/O error.
pub fn canonical(path: &Path) -> io::Result<PathBuf> {
    let resolved = path.canonicalize()?;
    #[cfg(target_os = "windows")]
    if let Some(plain) = resolved
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
        && plain.as_bytes().get(1) == Some(&b':')
    {
        return Ok(PathBuf::from(plain));
    }
    Ok(one_spelling(resolved))
}

/// Tells whether a path is absolute and plain: at most [`PATH_BYTES_MAX`] bytes, only the
/// root and folder names, and no control character, quote or backslash, so it sits in a
/// Seatbelt rule or a bubblewrap argument as written. Input: the path. Output: true when
/// plain.
#[cfg(not(target_os = "windows"))]
pub fn clean_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= PATH_BYTES_MAX
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
        && path.to_str().is_some_and(|text| {
            !text
                .chars()
                .any(|c| c.is_control() || c == '"' || c == '\\' || c == '\'')
        })
}

/// Tells whether a Windows path is absolute and plain: a drive prefix such as `C:` (never a
/// UNC, verbatim or device prefix), then folder names joined by `\`, as
/// [`windows_clean_text`] checks. Input: the path. Output: true when plain.
#[cfg(target_os = "windows")]
pub fn clean_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| match component {
            Component::Prefix(prefix) => matches!(prefix.kind(), std::path::Prefix::Disk(_)),
            Component::RootDir | Component::Normal(_) => true,
            Component::CurDir | Component::ParentDir => false,
        })
        && path.to_str().is_some_and(windows_clean_text)
}

/// Checks Windows path text on any system: a drive letter, `:\`, then nonempty folder names
/// joined by `\`, each neither `.` nor `..` nor ending in a space or dot, without control
/// characters, quotes, `/`, `:` or the characters Windows forbids in names, at most 1024
/// bytes. Input: the text. Output: true when plain.
pub fn windows_clean_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    if text.len() > PATH_BYTES_MAX
        || bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes[2] != b'\\'
    {
        return false;
    }
    text[3..].split('\\').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.ends_with(' ')
            && !part.ends_with('.')
            && !part.chars().any(|c| {
                c.is_control() || matches!(c, '"' | '\'' | '/' | ':' | '<' | '>' | '|' | '?' | '*')
            })
    })
}

/// The owner's home folder from the user profile, canonical when it resolves, which
/// no sandbox root may contain. Takes nothing. Returns the folder, or `Invalid` when the
/// profile names none or a relative one.
#[cfg(target_os = "windows")]
pub fn owner_home() -> Result<PathBuf, SandboxError> {
    let home = std::env::home_dir().ok_or_else(|| invalid("owner account is unknown"))?;
    settled_home(home)
}

/// The owner's home folder from the account database, canonical when it resolves, which
/// no sandbox root may contain. Takes nothing. Returns the folder, `Io` when the lookup
/// fails, or `Invalid` when the account is unknown or its home is relative.
#[cfg(unix)]
pub fn owner_home() -> Result<PathBuf, SandboxError> {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid())
        .map_err(|error| SandboxError::Io(io::Error::from(error)))?
        .ok_or_else(|| invalid("owner account is unknown"))?;
    settled_home(user.dir)
}

/// The owner's home as the sandbox compares it: refused unless absolute and clean, then
/// [`canonical`] when it resolves, so a home named through the macOS data volume has the
/// short spelling every sandbox root has. Input: the home the account names. Output: the
/// folder, or `Invalid` for a relative or unclean one.
pub fn settled_home(home: PathBuf) -> Result<PathBuf, SandboxError> {
    if !clean_absolute(&home) {
        return Err(invalid("owner home must be absolute"));
    }
    Ok(canonical(&home).unwrap_or(home))
}

/// Whether `name` can be one top-level name `ProjectSandbox::open_only_tops` keeps open:
/// one path part, never `.` or `..`, at most 255 bytes, with no control character, quote or
/// backslash, so it sits in a Seatbelt pattern as written. Takes the name. Returns the answer.
pub fn plain_top(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '"' | '\\' | '\''))
}

/// The Seatbelt filter for a path and everything below it: one `^X(/|$)` in place of
/// `^X(/.*)?$`, whose `(/.*)` tail libsandbox fails to compile again in lower case after a
/// fold group with no bracket class or wildcard before it, as `^/x/(s|S)(/.*)?$`
/// (V241-SANDBOX-REGEX, V243-FARM2-PRODUCT-SMALL). One regex keeps the profile under its cap
/// for long roots, where an `X$` and `X/` pair did not (V243-PROFILE-CAP-ROW, DEC-195 Q6).
/// Input: the pattern after `^`, an absolute path first. Output: the filter.
pub fn at_or_below(pattern: &str) -> String {
    assert!(pattern.starts_with('/'));
    format!("(regex #\"^{pattern}(/|$)\")")
}

#[cfg(test)]
mod tests;
