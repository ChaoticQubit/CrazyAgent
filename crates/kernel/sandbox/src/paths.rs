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

/// The plain Windows spelling of path text: a verbatim disk path
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
/// fold group with no bracket class or wildcard before it, as `^/x/(s|S)(/.*)?$`.
/// One regex keeps the profile under its cap
/// for long roots, where an `X$` and `X/` pair did not.
/// Input: the pattern after `^`, an absolute path first. Output: the filter.
pub fn at_or_below(pattern: &str) -> String {
    assert!(pattern.starts_with('/'));
    format!("(regex #\"^{pattern}(/|$)\")")
}

/// The code points outside ASCII that a case-insensitive volume folds onto ASCII, with the
/// ASCII they fold onto: on APFS the full case fold of their canonical decomposition, each
/// checked on the volume (probe 2026-10-28), and U+0131 DOTLESS I, which the NTFS upcase
/// table maps to `I`. The one list the landing checks, the name compares and the Seatbelt
/// name spellings share.
pub const ASCII_FOLDS: [(char, &str); 14] = [
    ('\u{DF}', "ss"),
    ('\u{131}', "i"),
    ('\u{17F}', "s"),
    ('\u{37E}', ";"),
    ('\u{1E9E}', "ss"),
    ('\u{1FEF}', "`"),
    ('\u{212A}', "k"),
    ('\u{FB00}', "ff"),
    ('\u{FB01}', "fi"),
    ('\u{FB02}', "fl"),
    ('\u{FB03}', "ffi"),
    ('\u{FB04}', "ffl"),
    ('\u{FB05}', "st"),
    ('\u{FB06}', "st"),
];

/// The owner's credential files and folders, relative to the home, that come before the
/// harness folders in the owner's secret list: keychains, SSH, GnuPG, git and cloud tool
/// credentials.
pub const OWNER_SECRET_PATHS_BEFORE_HARNESSES: &[&str] = &[
    "Library/Keychains",
    ".ssh",
    ".gnupg",
    ".netrc",
    ".git-credentials",
    ".config/gh",
    ".config/git/credentials",
    ".aws",
    ".docker",
    ".kube",
];

/// The owner's package registry and cloud credentials, relative to the home, that come
/// after the harness folders in the owner's secret list.
pub const OWNER_SECRET_PATHS_AFTER_HARNESSES: &[&str] = &[
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".npmrc",
    ".pypirc",
    ".config/gcloud",
];

/// The system-wide credential files and folders every sandbox keeps out, as absolute
/// paths.
pub const SYSTEM_SECRET_PATHS: &[&str] = &[
    "/Library/Keychains",
    "/private/var/db/SystemKey",
    "/opt/homebrew/etc/npmrc",
    "/usr/local/etc/npmrc",
];

/// Why a sandbox root is refused when it equals, holds or sits inside a secret path from
/// `ProjectSandbox::secret_paths`: an allow-list boundary such as Landlock or bubblewrap
/// cannot carve the secret back out of a granted root.
pub const SECRET_ROOT_REFUSED: &str = "sandbox roots must not hold or sit in a secret path";

/// Why an install is refused when a path its build granted reads with other identities at
/// install, such as a root renamed away and replaced by a link or a new folder: the profile
/// grants by path, so it would grant what no check saw.
pub const GRANT_CHANGED: &str = "a granted path changed between the sandbox build and its install";

/// The owner's secret names relative to the home, in order: the fixed ones before the
/// harness folders, the harness folders as given, then the fixed ones after. Input: the
/// harness folders, relative to the home. Output: the names.
pub fn owner_secret_paths(harnesses: &[&'static str]) -> impl Iterator<Item = &'static str> {
    OWNER_SECRET_PATHS_BEFORE_HARNESSES
        .iter()
        .copied()
        .chain(harnesses.iter().copied())
        .chain(OWNER_SECRET_PATHS_AFTER_HARNESSES.iter().copied())
}

/// The owner's secret paths under `owner` from [`owner_secret_paths`], then the system's.
/// Inputs: the owner's home and the harness folders, relative to it. Output: the absolute
/// paths.
pub fn owner_secrets(owner: &Path, harnesses: &[&'static str]) -> impl Iterator<Item = PathBuf> {
    owner_secret_paths(harnesses)
        .map(move |path| {
            path.split('/')
                .fold(owner.to_path_buf(), |at, part| at.join(part))
        })
        .chain(SYSTEM_SECRET_PATHS.iter().map(PathBuf::from))
}

/// The identity of one entry on its volume: device and inode on unix, volume serial number
/// and file index on Windows.
type Identity = (u64, u64);

/// Reads the identity of the entry a path names, following links. Input: the path.
/// Output: the identity, None when the path or a folder above it does not exist, or the
/// I/O error.
#[cfg(unix)]
fn identity(path: &Path) -> io::Result<Option<Identity>> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(path) {
        Ok(found) => Ok(Some((found.dev(), found.ino()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The Windows open flag that lets a folder be opened as a handle, as `std` opens one for
/// its metadata.
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

/// Reads the identity of the entry a path names, following links: opened with no access
/// rights and [`FILE_FLAG_BACKUP_SEMANTICS`], then read from the handle. Input: the path.
/// Output: the identity, None when the path or a folder above it does not exist, or the
/// I/O error.
#[cfg(windows)]
fn identity(path: &Path) -> io::Result<Option<Identity>> {
    use std::os::windows::fs::OpenOptionsExt;
    let opened = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path);
    match opened {
        Ok(file) => crazyagent_win_sandbox::file_id(&file).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    /// How many walks [`Walk::read`] took on this thread, which rows read through [`walks`].
    static WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The number of walks [`Walk::read`] took on this thread so far, read by the macOS profile
/// rows. Takes nothing. Returns the count.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn walks() -> usize {
    WALKS.with(std::cell::Cell::get)
}

#[cfg(all(test, unix))]
thread_local! {
    /// How many entries [`Reads::entry`] read from the disk on this thread, read through
    /// [`entry_reads`].
    static ENTRY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The number of entries [`Reads::entry`] read from the disk on this thread so far, a
/// cached answer not counted, read by the shared-reads rows. Takes nothing. Returns the
/// count.
#[cfg(all(test, unix))]
pub(crate) fn entry_reads() -> usize {
    ENTRY_READS.with(std::cell::Cell::get)
}

/// The most links whose targets do not exist yet one walk follows from its first missing
/// run before it refuses the path.
const DANGLING_LINK_HOPS: usize = 16;

/// The reads one sandbox build takes, kept by exact path for the whole build, so a folder
/// its walks pass through, such as the owner home above most secrets, is read once per
/// build; entry reads, links not followed, sit in their own map. A path that changes during
/// the build is judged as first read until the install, whose [`Kept::recheck`] reads
/// everything again with reads of its own. Only a read that succeeded is kept.
#[derive(Debug, Default)]
struct Reads {
    identities: std::collections::HashMap<PathBuf, Option<Identity>>,
    canonical: std::collections::HashMap<PathBuf, PathBuf>,
    #[cfg(unix)]
    entries: std::collections::HashMap<PathBuf, Option<Entry>>,
}

/// One entry as read with links not followed: a link, or another entry with its identity,
/// which then equals the identity read following links.
#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
enum Entry {
    Link,
    Plain(Identity),
}

impl Clone for Reads {
    /// Starts a copy with no reads, so a copied build reads the disk again rather than
    /// carry answers it did not read. Output: empty reads.
    fn clone(&self) -> Self {
        Reads::default()
    }
}

impl Reads {
    /// The identity of the entry a path names, as [`identity`] reads it, read once per
    /// path. Input: the path. Output: the identity, None when it does not exist, or the
    /// I/O error, which is not kept.
    fn identity(&mut self, path: &Path) -> io::Result<Option<Identity>> {
        if let Some(found) = self.identities.get(path) {
            return Ok(*found);
        }
        let found = identity(path)?;
        self.identities.insert(path.to_path_buf(), found);
        Ok(found)
    }

    /// The entry a path names, links not followed, read once per path. Input: the path.
    /// Output: the entry, None when it does not exist, or the I/O error, which is not kept.
    #[cfg(unix)]
    fn entry(&mut self, path: &Path) -> io::Result<Option<Entry>> {
        use std::os::unix::fs::MetadataExt;
        if let Some(found) = self.entries.get(path) {
            return Ok(*found);
        }
        #[cfg(test)]
        ENTRY_READS.with(|reads| reads.set(reads.get() + 1));
        let found = match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => Some(Entry::Link),
            Ok(meta) => Some(Entry::Plain((meta.dev(), meta.ino()))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        self.entries.insert(path.to_path_buf(), found);
        Ok(found)
    }

    /// The canonical form of a path, read once per path. Input: the path. Output: the
    /// resolved path, or the I/O error, which is not kept.
    fn canonical(&mut self, path: &Path) -> io::Result<PathBuf> {
        if let Some(found) = self.canonical.get(path) {
            return Ok(found.clone());
        }
        let found = path.canonicalize()?;
        assert!(found.is_absolute());
        self.canonical.insert(path.to_path_buf(), found.clone());
        Ok(found)
    }
}

/// A path's parts with the identity of each leading run of them, top down: None from the
/// first run that does not exist on, and None for a Windows drive prefix alone, which names
/// the drive's current folder. `reached` holds the identities of the folders the deepest
/// existing run resolves through once its links are followed, and of the folders above
/// where a first missing run that is a link leads ([`dangling_reach`]), so a run that leads
/// into a protected folder by a link, made or still to be made, is told too.
#[derive(Debug, Clone)]
struct Walk {
    path: PathBuf,
    ids: Vec<Option<Identity>>,
    reached: Vec<Identity>,
}

impl Walk {
    /// Walks a path through `reads`: by [`Walk::read_fast`] when it answers, else by
    /// [`Walk::read_full`], which gives the same walk for every path. Inputs: the path and
    /// the reads of its list. Output: the walk, or `Invalid` with [`SECRET_ROOT_REFUSED`].
    fn read(path: &Path, reads: &mut Reads) -> Result<Walk, SandboxError> {
        #[cfg(all(test, target_os = "macos"))]
        WALKS.with(|walks| walks.set(walks.get() + 1));
        #[cfg(unix)]
        if let Some(walk) = Self::read_fast(path, reads)? {
            return Ok(walk);
        }
        Self::read_full(path, reads)
    }

    /// Walks an absolute path with no link, `.` or `..` part by one entry read per run,
    /// links not followed. Inputs: the path and the reads of its list. Output: the walk,
    /// None when the path is not one this read can answer, or `Invalid` with
    /// [`SECRET_ROOT_REFUSED`] when a read fails other than not found.
    #[cfg(unix)]
    fn read_fast(path: &Path, reads: &mut Reads) -> Result<Option<Walk>, SandboxError> {
        let refused = |_| invalid(SECRET_ROOT_REFUSED);
        let parts: Vec<Component<'_>> = path.components().collect();
        if parts.first() != Some(&Component::RootDir) {
            return Ok(None);
        }
        let mut ids = Vec::with_capacity(parts.len());
        let mut run = PathBuf::new();
        let mut gone = false;
        for part in &parts {
            if !matches!(part, Component::RootDir | Component::Normal(_)) {
                return Ok(None);
            }
            run.push(part);
            let found = if gone {
                None
            } else {
                match reads.entry(&run).map_err(refused)? {
                    Some(Entry::Link) => return Ok(None),
                    Some(Entry::Plain(id)) => Some(id),
                    None => {
                        gone = true;
                        None
                    }
                }
            };
            ids.push(found);
        }
        let reached: Vec<Identity> = ids.iter().rev().flatten().copied().collect();
        assert_eq!(ids.len(), parts.len());
        assert!(!reached.is_empty() && reached.len() <= ids.len());
        Ok(Some(Walk {
            path: path.to_path_buf(),
            ids,
            reached,
        }))
    }

    /// Reads the identities of a path's leading runs, of every folder above the deepest
    /// existing run once its links are resolved, and of every folder above where its first
    /// missing run leads when that run is a link, through `reads`. Inputs: the path and the
    /// reads of its list. Output: the walk, or `Invalid` with [`SECRET_ROOT_REFUSED`] when a
    /// read or a resolve fails other than not found, or a missing run leads through more
    /// than [`DANGLING_LINK_HOPS`] links.
    fn read_full(path: &Path, reads: &mut Reads) -> Result<Walk, SandboxError> {
        let refused = |_| invalid(SECRET_ROOT_REFUSED);
        let parts: Vec<Component<'_>> = path.components().collect();
        let mut ids = Vec::with_capacity(parts.len());
        let mut run = PathBuf::new();
        let mut existing = None;
        let mut missing = None;
        let mut gone = false;
        for (at, part) in parts.iter().enumerate() {
            run.push(part);
            let prefix = matches!(part, Component::Prefix(_));
            let found = if gone || prefix {
                None
            } else {
                reads.identity(&run).map_err(refused)?
            };
            if found.is_some() {
                existing = Some(run.clone());
            } else if !prefix && !gone {
                missing = Some(at);
                gone = true;
            }
            ids.push(found);
        }
        let mut reals = Vec::with_capacity(2);
        if let Some(existing) = existing {
            reals.push(reads.canonical(&existing).map_err(refused)?);
        }
        if let Some(at) = missing {
            let run: PathBuf = parts[..=at].iter().collect();
            reals.extend(dangling_reach(&run, &parts[at + 1..], reads)?);
        }
        let mut reached = Vec::new();
        for real in &reals {
            for above in real.ancestors() {
                if let Some(found) = reads.identity(above).map_err(refused)? {
                    reached.push(found);
                }
            }
        }
        assert_eq!(ids.len(), parts.len());
        assert!(reals.len() <= 2);
        Ok(Walk {
            path: path.to_path_buf(),
            ids,
            reached,
        })
    }

    /// Tells whether this path equals or lies under `outer`: by text; or by entry, when
    /// `outer` exists and one of this path's existing runs is that entry; or when this path's
    /// new parts start under the same folder as `outer`'s and name it, folded on a volume
    /// that folds names and exactly elsewhere ([`Walk::new_parts_in`]). Input: the outer
    /// walk. Output: true when it does.
    fn sits_in(&self, outer: &Walk) -> bool {
        if self.path.starts_with(&outer.path) {
            return true;
        }
        if let Some(Some(entry)) = outer.ids.last()
            && (self.ids.contains(&Some(*entry)) || self.reached.contains(entry))
        {
            return true;
        }
        self.new_parts_in(outer, cfg!(any(target_os = "macos", windows)))
    }

    /// Compares this path's new parts below its deepest existing run when that run is one of
    /// `outer`'s: a part that is no plain name, or with `fold` folds to no ASCII name, counts
    /// as inside; else they sit in `outer` when at least as many as its parts below and each
    /// matches, the [`same_name`] with `fold` and the same text without. Inputs: the outer
    /// walk and whether the volume folds names. Output: true when this path may sit in it.
    fn new_parts_in(&self, outer: &Walk, fold: bool) -> bool {
        let Some(deepest) = self.ids.iter().rposition(Option::is_some) else {
            return false;
        };
        let Some(level) = outer.ids.iter().position(|id| *id == self.ids[deepest]) else {
            return false;
        };
        let parts: Vec<Component<'_>> = self.path.components().collect();
        let outer_parts: Vec<Component<'_>> = outer.path.components().collect();
        assert_eq!(parts.len(), self.ids.len());
        assert_eq!(outer_parts.len(), outer.ids.len());
        let Some(new) = parts[deepest + 1..]
            .iter()
            .map(|part| plain_name(*part).filter(|name| !fold || ascii_fold(name).is_some()))
            .collect::<Option<Vec<&str>>>()
        else {
            return true;
        };
        let below = &outer_parts[level + 1..];
        let named = |part: &str, step: &str| {
            if fold {
                same_name(part, step)
            } else {
                part == step
            }
        };
        new.len() >= below.len()
            && new
                .iter()
                .zip(below)
                .all(|(part, step)| plain_name(*step).is_some_and(|step| named(part, step)))
    }
}

/// Where a run that does not exist leads when it is a link whose target does not exist yet:
/// the target, read against the link's own folder, with the parts after the run, walked top
/// down again, and on through each further such link it meets, at most
/// [`DANGLING_LINK_HOPS`] of them. Inputs: the missing run, the parts after it and the reads
/// of its walk's list. Output: the canonical deepest existing folder of where it leads, None
/// when the run is no link, or `Invalid` with [`SECRET_ROOT_REFUSED`] past the bound or when
/// a read fails.
fn dangling_reach(
    run: &Path,
    rest: &[Component<'_>],
    reads: &mut Reads,
) -> Result<Option<PathBuf>, SandboxError> {
    let refused = |_| invalid(SECRET_ROOT_REFUSED);
    let mut link = run.to_path_buf();
    let mut after: PathBuf = rest.iter().collect();
    let mut deepest = None;
    for hop in 0..=DANGLING_LINK_HOPS {
        let linked = match std::fs::symlink_metadata(&link) {
            Ok(found) => found.file_type().is_symlink(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(refused(error)),
        };
        if !linked {
            break;
        }
        if hop == DANGLING_LINK_HOPS {
            return Err(invalid(SECRET_ROOT_REFUSED));
        }
        let target = std::fs::read_link(&link).map_err(refused)?;
        let folder = link.parent().map_or_else(PathBuf::new, Path::to_path_buf);
        let led = folder.join(target).join(&after);
        let parts: Vec<Component<'_>> = led.components().collect();
        let mut walked = PathBuf::new();
        let mut next = None;
        deepest = None;
        for (at, part) in parts.iter().enumerate() {
            walked.push(part);
            if matches!(part, Component::Prefix(_)) {
                continue;
            }
            if reads.identity(&walked).map_err(refused)?.is_none() {
                next = Some(at);
                break;
            }
            deepest = Some(walked.clone());
        }
        let Some(at) = next else {
            break;
        };
        after = parts[at + 1..].iter().collect();
        link = walked;
    }
    deepest
        .map(|found| reads.canonical(&found).map_err(refused))
        .transpose()
}

/// The text of a plain folder or file name part. Input: the part. Output: its text, or
/// None for a root, prefix, `.` or `..` part or a name that is not UTF-8.
fn plain_name(part: Component<'_>) -> Option<&str> {
    match part {
        Component::Normal(name) => name.to_str(),
        _ => None,
    }
}

/// The owner's harness homes: each harness folder name joined to the owner home. Inputs:
/// the owner home and the folder names. Output: the folders, in the names' order.
pub(crate) fn harness_homes(owner: &Path, harnesses: &[&'static str]) -> Vec<PathBuf> {
    harnesses
        .iter()
        .map(|private| owner.join(private))
        .collect()
}

/// The walks of the owner home, of its harness homes and of the secret paths, each taken
/// once with one set of [`Reads`] for the roots one check compares against them. Built by
/// [`Walked::of`]; a sandbox build keeps its own in a [`Kept`].
#[derive(Debug, Clone)]
pub(crate) struct Walked {
    owner: Walk,
    harnesses: Vec<Walk>,
    secrets: Vec<Walk>,
    reads: Reads,
}

impl Walked {
    /// Walks the owner home, each harness home and each secret path once, sharing their
    /// reads. Inputs: the owner home, the harness homes ([`harness_homes`]) and the secret
    /// paths. Output: the walks, or `Invalid` with [`SECRET_ROOT_REFUSED`] when a read fails
    /// other than not found.
    pub(crate) fn of(
        owner: &Path,
        harnesses: &[PathBuf],
        secrets: &[PathBuf],
    ) -> Result<Walked, SandboxError> {
        let mut reads = Reads::default();
        let walked = Walked {
            owner: Walk::read(owner, &mut reads)?,
            harnesses: harnesses
                .iter()
                .map(|home| Walk::read(home, &mut reads))
                .collect::<Result<_, _>>()?,
            secrets: secrets
                .iter()
                .map(|secret| Walk::read(secret, &mut reads))
                .collect::<Result<_, _>>()?,
            reads,
        };
        assert_eq!(walked.harnesses.len(), harnesses.len());
        assert_eq!(walked.secrets.len(), secrets.len());
        Ok(walked)
    }

    /// Walks a root once, through this list's reads, and refuses it when it holds or sits in
    /// one of this list's secrets, the first rule an extra read-only folder is judged by.
    /// Input: the root. Output: its walk for [`Walked::grant_walked`], or `Invalid` with
    /// [`SECRET_ROOT_REFUSED`].
    pub(crate) fn walk_clear(&mut self, root: &Path) -> Result<WalkedRoot, SandboxError> {
        let walk = Walk::read(root, &mut self.reads)?;
        clear_of_walked(&walk, &self.secrets)?;
        Ok(WalkedRoot(walk))
    }

    /// Judges a root [`Walked::walk_clear`] walked by the rest of [`granted_walk`]'s rules,
    /// with no second walk. Inputs: the root and its walk. Output: unit, or `Invalid` naming
    /// the rule.
    pub(crate) fn grant_walked(
        &self,
        root: &Path,
        walked: &WalkedRoot,
    ) -> Result<(), SandboxError> {
        assert_eq!(walked.0.path, root);
        deep_plain(root)?;
        granted_walk(root, &walked.0, self, &self.secrets)
    }

    /// The walks of those of `secrets` this list holds, in its order. Input: the secret
    /// paths. Output: their walks.
    fn secrets_of<'w>(&'w self, secrets: &'w [PathBuf]) -> impl Iterator<Item = &'w Walk> {
        self.secrets
            .iter()
            .filter(|walk| secrets.contains(&walk.path))
    }
}

/// One root's walk from [`Walked::walk_clear`], judged once more by [`Walked::grant_walked`].
pub(crate) struct WalkedRoot(Walk);

/// Refuses a root that is not absolute, plain and at least two folders deep, before any
/// read of it. Input: the root. Output: unit, or `Invalid` naming the rule.
fn deep_plain(root: &Path) -> Result<(), SandboxError> {
    let depth = root
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    if !clean_absolute(root) || depth < 2 {
        return Err(invalid("sandbox roots must be absolute, deep and plain"));
    }
    Ok(())
}

/// The rules a granted root meets after the plain-path rule ([`deep_plain`]), on a root
/// already walked: not holding the owner home, not inside a harness home, clear of the given
/// secret walks and an existing canonical directory. Inputs: the root, its walk, the walked
/// owner and harness homes and the secret walks. Output: unit, or `Invalid` naming the rule.
fn granted_walk<'w>(
    root: &Path,
    walk: &Walk,
    walked: &Walked,
    secrets: impl IntoIterator<Item = &'w Walk>,
) -> Result<(), SandboxError> {
    if walked.owner.sits_in(walk) {
        return Err(invalid("sandbox roots must not contain the owner home"));
    }
    for home in &walked.harnesses {
        if walk.sits_in(home) {
            return Err(invalid("sandbox roots must not be owner harness homes"));
        }
    }
    clear_of_walked(walk, secrets)?;
    if !root.is_dir() || canonical(root)? != root {
        return Err(invalid(
            "sandbox roots must be existing canonical directories",
        ));
    }
    Ok(())
}

/// Refuses a walked root that equals, holds or sits inside one of the walked secrets,
/// comparing entries on the volume ([`Walk::sits_in`]), so another spelling of a secret's
/// folder is refused too. Inputs: the root's walk and the secrets' walks. Output: unit,
/// or `Invalid` with [`SECRET_ROOT_REFUSED`].
fn clear_of_walked<'w>(
    root: &Walk,
    secrets: impl IntoIterator<Item = &'w Walk>,
) -> Result<(), SandboxError> {
    for secret in secrets {
        if root.sits_in(secret) || secret.sits_in(root) {
            return Err(invalid(SECRET_ROOT_REFUSED));
        }
    }
    Ok(())
}

/// Whether `paths` holds `path` by [`Path`] equality, so a spelling that differs only by a
/// doubled slash, a `.` part or a trailing slash still matches. Inputs: the paths and the
/// path. Output: the answer.
fn holds_path<'p>(paths: impl Iterator<Item = &'p Path> + Clone, path: &Path) -> bool {
    #[cfg(test)]
    PATH_FALLBACKS.with(|fallbacks| fallbacks.set(fallbacks.get() + 1));
    paths.into_iter().any(|held| held == path)
}

/// The paths of `walks`, in order, for [`holds_path`]. Input: the walks. Output: their
/// paths.
fn walked_paths(walks: &[Walk]) -> impl Iterator<Item = &Path> + Clone {
    walks.iter().map(|walk| walk.path.as_path())
}

#[cfg(test)]
thread_local! {
    /// How many times [`holds_path`] compared by [`Path`] equality on this thread, read by
    /// the rows that pin which compares a build makes.
    static PATH_FALLBACKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The number of [`Path`] equality passes [`holds_path`] made on this thread so far.
/// Takes nothing. Returns the count.
#[cfg(test)]
pub(crate) fn path_fallbacks() -> usize {
    PATH_FALLBACKS.with(std::cell::Cell::get)
}

/// One path a sandbox build granted, with its walk from that check and every secret path
/// it was checked against.
#[derive(Debug, Clone)]
struct Checked {
    walk: Walk,
    against: Vec<PathBuf>,
}

/// What one sandbox build keeps from its start until its install: the walks of the owner
/// home, the harness homes and each secret path, each taken once, and each path it granted
/// with that path's walk and the secrets it was checked against. A build never shares it
/// with another, so no walk serves two launches; its install walks every secret and every
/// granted path again ([`Kept::recheck`]), so a secret or a root that moved, appeared or was
/// swapped after the build is judged as it is then.
#[derive(Debug, Clone)]
pub(crate) struct Kept {
    walked: Walked,
    checked: Vec<Checked>,
}

impl Kept {
    /// Walks the owner home, the harness homes and the secret paths once, as [`Walked::of`]
    /// does. Inputs: those paths. Output: the kept walks, with no path granted yet, or
    /// `Invalid` with [`SECRET_ROOT_REFUSED`].
    pub(crate) fn of(
        owner: &Path,
        harnesses: &[PathBuf],
        secrets: &[PathBuf],
    ) -> Result<Kept, SandboxError> {
        Ok(Kept {
            walked: Walked::of(owner, harnesses, secrets)?,
            checked: Vec::new(),
        })
    }

    /// Checks a root by [`deep_plain`] and [`granted_walk`] against `secrets`, walking only the
    /// root, or reusing its walk when this build checked it already, and any of `secrets`
    /// not walked yet, then keeps the root. `secrets` must hold every harness home, so the
    /// install's [`Kept::recheck`] judges them again. Inputs: the root and the secret
    /// paths. Output: unit, or `Invalid` naming the rule, keeping nothing.
    pub(crate) fn grant(&mut self, root: &Path, secrets: &[PathBuf]) -> Result<(), SandboxError> {
        assert!(
            self.walked
                .harnesses
                .iter()
                .all(|home| secrets.contains(&home.path)),
            "a grant is checked against every harness home"
        );
        deep_plain(root)?;
        self.walk_secrets(secrets)?;
        let walk = self.walk_of(root)?;
        granted_walk(root, &walk, &self.walked, self.walked.secrets_of(secrets))?;
        self.keep(walk, secrets);
        Ok(())
    }

    /// Checks a path by [`clear_of_walked`] against `secrets`, walking only the path,
    /// or reusing its walk when this build checked it already, and any of `secrets` not
    /// walked yet, then keeps the path. Inputs: the path and the secret paths. Output: unit,
    /// or `Invalid` with [`SECRET_ROOT_REFUSED`], keeping nothing.
    pub(crate) fn clear(&mut self, path: &Path, secrets: &[PathBuf]) -> Result<(), SandboxError> {
        self.walk_secrets(secrets)?;
        let walk = self.walk_of(path)?;
        clear_of_walked(&walk, self.walked.secrets_of(secrets))?;
        self.keep(walk, secrets);
        Ok(())
    }

    /// Walks again every kept path and every secret one was checked against, each once,
    /// sharing the reads; a kept path must read with the identities its grant kept, then its
    /// new walk is checked against the secrets' new walks. Output: unit, `Invalid` with
    /// [`GRANT_CHANGED`] for a kept path that changed, or with [`SECRET_ROOT_REFUSED`] when a
    /// kept path now holds or sits in a secret.
    pub(crate) fn recheck(&self) -> Result<(), SandboxError> {
        let mut reads = Reads::default();
        let mut fresh: Vec<Walk> = Vec::new();
        for checked in &self.checked {
            for secret in &checked.against {
                if !holds_path(fresh.iter().map(|walk| walk.path.as_path()), secret) {
                    fresh.push(Walk::read(secret, &mut reads)?);
                }
            }
        }
        for checked in &self.checked {
            let now = Walk::read(&checked.walk.path, &mut reads)?;
            if now.ids != checked.walk.ids || now.reached != checked.walk.reached {
                return Err(invalid(GRANT_CHANGED));
            }
            let against = fresh.iter().filter(|walk| {
                holds_path(checked.against.iter().map(PathBuf::as_path), &walk.path)
            });
            clear_of_walked(&now, against)?;
        }
        Ok(())
    }

    /// Walks each of `secrets` this list does not hold yet, through the build's reads, and
    /// adds them. Input: the secret paths. Output: unit, or `Invalid` with
    /// [`SECRET_ROOT_REFUSED`].
    fn walk_secrets(&mut self, secrets: &[PathBuf]) -> Result<(), SandboxError> {
        for secret in secrets {
            if !holds_path(walked_paths(&self.walked.secrets), secret) {
                let walk = Walk::read(secret, &mut self.walked.reads)?;
                self.walked.secrets.push(walk);
            }
        }
        assert!(
            secrets
                .iter()
                .all(|secret| holds_path(walked_paths(&self.walked.secrets), secret))
        );
        Ok(())
    }

    /// The walk of a path this build checked already, else a new walk of it through the
    /// build's reads. Input: the path. Output: the walk, or `Invalid` with
    /// [`SECRET_ROOT_REFUSED`].
    fn walk_of(&mut self, path: &Path) -> Result<Walk, SandboxError> {
        match self
            .checked
            .iter()
            .find(|checked| checked.walk.path == path)
        {
            Some(checked) => Ok(checked.walk.clone()),
            None => Walk::read(path, &mut self.walked.reads),
        }
    }

    /// Keeps a path that passed a check, with the secrets it was checked against, adding
    /// them to its entry when the build checked it before. Inputs: its walk and the secret
    /// paths.
    fn keep(&mut self, walk: Walk, secrets: &[PathBuf]) {
        match self
            .checked
            .iter_mut()
            .find(|checked| checked.walk.path == walk.path)
        {
            Some(checked) => {
                for secret in secrets {
                    if !holds_path(checked.against.iter().map(PathBuf::as_path), secret) {
                        checked.against.push(secret.clone());
                    }
                }
            }
            None => self.checked.push(Checked {
                walk,
                against: secrets.to_vec(),
            }),
        }
    }
}

/// The code points a case-insensitive HFS+ volume ignores in names (git's
/// `core.protectHFS` list), so `HE\u{200c}AD` opens `HEAD` there: the one list every name
/// compare that must match such a volume leaves out.
pub const HFS_IGNORED: [char; 16] = [
    '\u{200C}', '\u{200D}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
    '\u{202E}', '\u{206A}', '\u{206B}', '\u{206C}', '\u{206D}', '\u{206E}', '\u{206F}', '\u{FEFF}',
];

/// Folds one path part the way the volume compares names, when the result is ASCII.
/// Input: the part. Output: the part in ASCII lower case with [`ASCII_FOLDS`] applied and
/// the code points HFS+ ignores in names left out
/// ([`HFS_IGNORED`]), or None when a code point
/// stays outside ASCII.
pub fn ascii_fold(part: &str) -> Option<String> {
    let mut folded = String::with_capacity(part.len());
    for point in part.chars() {
        if HFS_IGNORED.contains(&point) {
            continue;
        }
        if point.is_ascii() {
            folded.push(point.to_ascii_lowercase());
        } else {
            let (_, ascii) = ASCII_FOLDS.iter().find(|(from, _)| *from == point)?;
            folded.push_str(ascii);
        }
    }
    assert!(folded.is_ascii());
    Some(folded)
}

/// Tells whether two path parts may name one entry on a volume that folds names, the one
/// name compare of the landing checks: both fold onto the same ASCII name under
/// [`ascii_fold`], or both stay outside ASCII, since the volume may still fold one onto
/// the other. A part that stays outside ASCII never names the entry of one that folds onto
/// ASCII. Inputs: the two parts. Output: true when they may.
pub fn same_name(one: &str, other: &str) -> bool {
    match (ascii_fold(one), ascii_fold(other)) {
        (Some(one), Some(other)) => one == other,
        (None, None) => true,
        _ => false,
    }
}

/// Spells plain text as a Seatbelt regex that matches every name a case-insensitive volume
/// finds under it: each letter in both cases with every code point that folds onto it or
/// onto a run of two or three letters ([`spelled`]), so the rule never leans on how Seatbelt
/// folds names itself. A stretch with more than [`SPELLINGS_MAX`] spellings, a
/// long run of `s` or `f` in path text, is spelled as the [`superset`], which matches more,
/// so the result holds in a deny. Input: text without a quote or backslash. Output: the
/// pattern text, without anchors.
pub fn any_case(text: &str) -> String {
    spelled(&regex_escape(text), false)
}

/// A Seatbelt regex fragment matched as [`any_case`] matches plain text, every character
/// that is not a letter, regex syntax included, staying as written; a letter run next to a
/// wildcard also takes a fold reaching into it ([`spelled_run`]). Every stretch is listed
/// whole, so a rule that gives access back stays exact. Input: a fragment with no bracket
/// class or quote. Output: the pattern.
pub fn fold_case(pattern: &str) -> String {
    assert!(!pattern.contains('[') && !pattern.contains('"'));
    spelled(pattern, true)
}

/// The most spellings one stretch of letters between two places no fold spans lists one by
/// one; past it path text takes the [`superset`].
const SPELLINGS_MAX: usize = 8;

/// Spells text run by run from [`ASCII_FOLDS`]: each run of ASCII
/// letters through [`spelled_run`], every other character through [`spelled_char`]. In a
/// regex fragment a letter a quantifier follows stands alone, and no fold may join it to a
/// letter beside it. Inputs: the text and whether it is a regex fragment. Output: the pattern.
fn spelled(text: &str, regex: bool) -> String {
    let characters: Vec<char> = text.chars().collect();
    let letter = |at: usize| characters.get(at).is_some_and(char::is_ascii_alphabetic);
    let quantified =
        |at: usize| regex && matches!(characters.get(at + 1), Some('?' | '*' | '+' | '{'));
    let mut pattern = String::with_capacity(text.len() * 8);
    let mut at = 0;
    while at < characters.len() {
        if !letter(at) || quantified(at) {
            assert!(!letter(at) || !joins_fold(&characters, at));
            pattern.push_str(&spelled_char(characters[at]));
            at += 1;
            continue;
        }
        let start = at;
        while letter(at) && !quantified(at) {
            at += 1;
        }
        let before = regex && start > 0 && matches!(characters[start - 1], '*' | '+');
        let after = regex
            && characters.get(at) == Some(&'.')
            && matches!(characters.get(at + 1), Some('*' | '+'));
        pattern.push_str(&spelled_run(&characters[start..at], before, after, regex));
    }
    pattern
}

/// Tells whether a fold of two or three letters would join the letter at `at` to the letter
/// before it or to the one after its quantifier. Inputs: the characters and the letter's
/// place. Output: true when one would.
fn joins_fold(characters: &[char], at: usize) -> bool {
    let joined = |left: Option<&char>, right: Option<&char>| match (left, right) {
        (Some(left), Some(right)) if left.is_ascii_alphabetic() && right.is_ascii_alphabetic() => {
            let pair = format!("{left}{right}").to_ascii_lowercase();
            ASCII_FOLDS
                .iter()
                .any(|(_, folded)| folded.contains(pair.as_str()))
        }
        _ => false,
    };
    let previous = at.checked_sub(1).and_then(|before| characters.get(before));
    joined(previous, characters.get(at)) || joined(characters.get(at), characters.get(at + 2))
}

/// Spells one character as a case-insensitive volume finds it: an ASCII letter in both
/// cases, with every code point that folds onto it alone, the non-ASCII ones in a group so
/// each matches its whole UTF-8 sequence. Input: the character. Output: the pattern.
fn spelled_char(character: char) -> String {
    let lower = character.to_ascii_lowercase();
    let mut alternatives = vec![lower.to_string()];
    if character.is_ascii_alphabetic() {
        alternatives.push(character.to_ascii_uppercase().to_string());
    }
    for (point, folded) in ASCII_FOLDS {
        if folded.len() == 1 && folded.starts_with(lower) {
            alternatives.push(point.to_string());
        }
    }
    if alternatives.len() == 1 {
        character.to_string()
    } else if alternatives
        .iter()
        .all(|alternative| alternative.is_ascii())
    {
        format!("[{}]", alternatives.concat())
    } else {
        format!("({})", alternatives.join("|"))
    }
}

/// Spells one run of ASCII letters with every fold of two or three letters inside it and,
/// next to a wildcard, every fold reaching past its first or last letter into the wildcard.
/// The run splits where no fold spans, each stretch listing all its spellings, or past
/// [`SPELLINGS_MAX`] taking the [`superset`] unless it must stay exact. Inputs: the letters,
/// whether a wildcard comes before and after them and whether every stretch must be listed
/// whole. Output: the pattern text.
fn spelled_run(run: &[char], before: bool, after: bool, exact: bool) -> String {
    let lower: String = run.iter().map(char::to_ascii_lowercase).collect();
    let count = run.len();
    let mut folds: Vec<(usize, usize, char)> = Vec::new();
    for (point, folded) in ASCII_FOLDS {
        let width = folded.len();
        if width < 2 {
            continue;
        }
        for at in (0..count).filter(|&at| lower[at..].starts_with(folded)) {
            folds.push((at, at + width, point));
        }
        for split in 1..width {
            if before && lower.starts_with(&folded[split..]) {
                folds.push((0, width - split, point));
            }
            if after && lower.ends_with(&folded[..split]) {
                folds.push((count - split, count, point));
            }
        }
        for inner in 1..width.saturating_sub(count) {
            if before && after && folded[inner..inner + count] == lower {
                folds.push((0, count, point));
            }
        }
    }
    folds.sort_unstable();
    folds.dedup();
    let mut pieces: Vec<(usize, usize, String)> = (0..count)
        .map(|at| (at, at + 1, spelled_char(run[at])))
        .collect();
    pieces.extend(
        folds
            .into_iter()
            .map(|(start, end, point)| (start, end, point.to_string())),
    );
    let cuts: Vec<usize> = (0..=count)
        .filter(|&cut| {
            !pieces
                .iter()
                .any(|(start, end, _)| *start < cut && cut < *end)
        })
        .collect();
    let mut pattern = String::new();
    for stretch in cuts.windows(2) {
        let (at, to) = (stretch[0], stretch[1]);
        let (count, fewest) = tally(&pieces, at, to);
        assert!(count >= 1 && (1..=to - at).contains(&fewest));
        if count > SPELLINGS_MAX {
            assert!(!exact, "a pattern stretch lists every spelling");
            pattern.push_str(&superset(&pieces, at, to, fewest));
            continue;
        }
        let mut spellings = Vec::new();
        spell_stretch(&pieces, at, to, String::new(), &mut spellings);
        assert_eq!(spellings.len(), count);
        if let [only] = spellings.as_slice() {
            pattern.push_str(only);
        } else {
            pattern.push_str(&format!("({})", spellings.join("|")));
        }
    }
    pattern
}

/// Counts the spellings of the letters from `at` to `to` that the pieces give, up to one
/// past [`SPELLINGS_MAX`], and the fewest pieces one spelling takes, place by place from the
/// end, so no long run is listed to be counted. Inputs: the pieces as start, end and
/// pattern, and the stretch. Output: the count and the fewest pieces.
fn tally(pieces: &[(usize, usize, String)], at: usize, to: usize) -> (usize, usize) {
    assert!(at < to);
    let mut counts = vec![0; to - at + 1];
    let mut fewest = vec![0; to - at + 1];
    counts[to - at] = 1;
    for place in (at..to).rev() {
        let ends: Vec<usize> = pieces
            .iter()
            .filter(|(start, end, _)| *start == place && *end <= to)
            .map(|(_, end, _)| end - at)
            .collect();
        assert!(!ends.is_empty());
        counts[place - at] = ends
            .iter()
            .fold(0, |sum, &end| (sum + counts[end]).min(SPELLINGS_MAX + 1));
        fewest[place - at] = 1 + ends.iter().map(|&end| fewest[end]).min().unwrap_or(0);
    }
    (counts[0], fewest[0])
}

/// Spells the letters from `at` to `to` as one of the stretch's pieces at each place, at
/// least `fewest` places and at most one per letter, the extra places optional, since
/// Seatbelt reads no `{m,n}` count (probe 2026-11-15). It matches every spelling and more,
/// so it holds only in a deny. Inputs: the pieces, the stretch and its fewest pieces.
/// Output: the pattern.
fn superset(pieces: &[(usize, usize, String)], at: usize, to: usize, fewest: usize) -> String {
    let mut alternatives: Vec<&str> = Vec::new();
    for (start, end, piece) in pieces {
        if at <= *start && *end <= to && !alternatives.contains(&piece.as_str()) {
            alternatives.push(piece);
        }
    }
    let place = format!("({})", alternatives.join("|"));
    let optional = format!("{place}?");
    let pattern = place.repeat(fewest) + &optional.repeat(to - at - fewest);
    assert!(fewest <= to - at && pattern.len() <= (to - at) * optional.len());
    pattern
}

/// Lists every spelling of the letters from `at` to `to` that the pieces give, in order.
/// Inputs: the pieces as start, end and pattern, the stretch, the spelling so far and the
/// list it adds to. Output: none; the list grows.
fn spell_stretch(
    pieces: &[(usize, usize, String)],
    at: usize,
    to: usize,
    so_far: String,
    spellings: &mut Vec<String>,
) {
    if at == to {
        spellings.push(so_far);
        return;
    }
    assert!(at < to);
    for (start, end, piece) in pieces {
        if *start == at && *end <= to {
            assert!(start < end);
            spell_stretch(pieces, *end, to, format!("{so_far}{piece}"), spellings);
        }
    }
}

/// Escapes plain text for a Seatbelt regex, so every character matches itself. Input: text
/// without a quote or backslash. Output: the pattern text, without anchors.
pub fn regex_escape(text: &str) -> String {
    assert!(!text.contains('"') && !text.contains('\\'));
    let mut pattern = String::with_capacity(text.len() * 2);
    for character in text.chars() {
        if ".^$*+?()[]{}|".contains(character) {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern
}

/// Tells whether a name opens one of `names` on a case-insensitive volume, compared as
/// [`folded`] compares names, so `.GIT`, `\u{feff}.git` and `.g\u{131}t` all open `.git`.
/// Inputs: the name and the plain names. Output: true for one.
pub fn names_one_of(name: &str, names: &[&str]) -> bool {
    let name = folded(name);
    names.iter().any(|known| folded(known) == name)
}

/// Folds a file name the way case-insensitive volumes compare names, so a name such a
/// volume opens as an instruction file matches it: the code points HFS+ ignores in names
/// (git's `core.protectHFS` list) left out, each code point in [`ASCII_FOLDS`] spelled as
/// its ASCII and ASCII letters put in lower case. Input: the name or path. Output: the
/// folded text.
pub fn folded(name: &str) -> String {
    let mut text = String::with_capacity(name.len());
    for point in name.chars().filter(|point| !HFS_IGNORED.contains(point)) {
        match ASCII_FOLDS.iter().find(|(from, _)| *from == point) {
            Some((_, ascii)) => text.push_str(ascii),
            None => text.push(point.to_ascii_lowercase()),
        }
    }
    text
}

#[cfg(test)]
mod tests;
