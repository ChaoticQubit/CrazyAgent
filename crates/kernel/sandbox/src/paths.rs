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

/// Refuses a root that equals, holds or sits inside one of `secrets`, comparing entries on
/// the volume ([`Walk::sits_in`]) so another spelling of a secret's folder is refused too.
/// Inputs: the root and the secret paths. Output: unit, or `Invalid` with
/// [`SECRET_ROOT_REFUSED`], also when a metadata read on either path fails other than not
/// found.
pub fn clear_of_secrets(root: &Path, secrets: &[PathBuf]) -> Result<(), SandboxError> {
    let root = Walk::of(root)?;
    for secret in secrets {
        let secret = Walk::of(secret)?;
        if root.sits_in(&secret) || secret.sits_in(&root) {
            return Err(invalid(SECRET_ROOT_REFUSED));
        }
    }
    Ok(())
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
    /// How many walks [`Walk::of`] took on this thread, which rows read through [`walks`].
    static WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The number of walks [`Walk::of`] took on this thread so far, read by the macOS profile
/// rows. Takes nothing. Returns the count.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn walks() -> usize {
    WALKS.with(std::cell::Cell::get)
}

/// A path's parts with the identity of each leading run of them, top down: None from the
/// first run that does not exist on, and None for a Windows drive prefix alone, which names
/// the drive's current folder. `reached` holds the identities of the folders the deepest
/// existing run resolves through once its links are followed, so a run that leads into a
/// protected folder by a link is told too.
struct Walk<'a> {
    path: &'a Path,
    parts: Vec<Component<'a>>,
    ids: Vec<Option<Identity>>,
    reached: Vec<Identity>,
}

impl<'a> Walk<'a> {
    /// Reads the identities of a path's leading runs, and of every folder above the deepest
    /// existing run once its links are resolved. Input: the path. Output: the walk, or
    /// `Invalid` with [`SECRET_ROOT_REFUSED`] when a read or the resolve fails other than
    /// not found.
    fn of(path: &'a Path) -> Result<Walk<'a>, SandboxError> {
        #[cfg(all(test, target_os = "macos"))]
        WALKS.with(|walks| walks.set(walks.get() + 1));
        let refused = |_| invalid(SECRET_ROOT_REFUSED);
        let parts: Vec<Component<'a>> = path.components().collect();
        let mut ids = Vec::with_capacity(parts.len());
        let mut run = PathBuf::new();
        let mut existing = None;
        let mut gone = false;
        for part in &parts {
            run.push(part);
            let prefix = matches!(part, Component::Prefix(_));
            let found = if gone || prefix {
                None
            } else {
                identity(&run).map_err(refused)?
            };
            if found.is_some() {
                existing = Some(run.clone());
            } else if !prefix {
                gone = true;
            }
            ids.push(found);
        }
        let mut reached = Vec::new();
        if let Some(existing) = existing {
            let real = existing.canonicalize().map_err(refused)?;
            for above in real.ancestors() {
                if let Some(found) = identity(above).map_err(refused)? {
                    reached.push(found);
                }
            }
        }
        assert_eq!(ids.len(), parts.len());
        Ok(Walk {
            path,
            parts,
            ids,
            reached,
        })
    }

    /// Tells whether this path equals or lies under `outer`: by text; or by entry, when
    /// `outer` exists and one of this path's existing runs is that entry; or, on a volume
    /// that folds names, when this path's new parts start under the same folder as
    /// `outer`'s ([`Walk::folds_into`]). Input: the outer walk. Output: true when it does.
    fn sits_in(&self, outer: &Walk) -> bool {
        if self.path.starts_with(outer.path) {
            return true;
        }
        if let Some(Some(entry)) = outer.ids.last()
            && (self.ids.contains(&Some(*entry)) || self.reached.contains(entry))
        {
            return true;
        }
        cfg!(any(target_os = "macos", windows)) && self.folds_into(outer)
    }

    /// Compares the parts of this path that do not exist yet, on a volume that folds names.
    /// When this path's deepest existing run is `outer` or one of `outer`'s existing runs,
    /// a new part that folds to no ASCII name, or is not a plain name, may name anything
    /// there and counts as inside; else the new parts sit in `outer` when they are at least
    /// as many as `outer`'s parts below that run and each is the [`same_name`] as its match.
    /// Input: the outer walk. Output: true when this path may sit in `outer`.
    fn folds_into(&self, outer: &Walk) -> bool {
        let Some(deepest) = self.ids.iter().rposition(Option::is_some) else {
            return false;
        };
        let Some(level) = outer.ids.iter().position(|id| *id == self.ids[deepest]) else {
            return false;
        };
        let Some(new) = self.parts[deepest + 1..]
            .iter()
            .map(|part| plain_name(*part).filter(|name| ascii_fold(name).is_some()))
            .collect::<Option<Vec<&str>>>()
        else {
            return true;
        };
        let below = &outer.parts[level + 1..];
        new.len() >= below.len()
            && new
                .iter()
                .zip(below)
                .all(|(part, step)| plain_name(*step).is_some_and(|step| same_name(part, step)))
    }
}

/// The text of a plain folder or file name part. Input: the part. Output: its text, or
/// None for a root, prefix, `.` or `..` part or a name that is not UTF-8.
fn plain_name(part: Component<'_>) -> Option<&str> {
    match part {
        Component::Normal(name) => name.to_str(),
        _ => None,
    }
}

/// Checks a root a sandbox may grant: absolute, plain and at least two folders deep, not
/// holding the owner home, not inside a harness folder of it, clear of `secrets`, and an
/// existing canonical directory. Inputs: the root, the owner home, the secret paths and the
/// harness folders relative to the home. Output: unit, or `Invalid` naming the rule.
pub fn granted_root(
    root: &Path,
    owner: &Path,
    secrets: &[PathBuf],
    harnesses: &[&'static str],
) -> Result<(), SandboxError> {
    let homes = harness_homes(owner, harnesses);
    granted_walked(root, &Walked::of(owner, &homes, secrets)?)
}

/// The owner's harness homes: each harness folder name joined to the owner home. Inputs:
/// the owner home and the folder names. Output: the folders, in the names' order.
pub(crate) fn harness_homes(owner: &Path, harnesses: &[&'static str]) -> Vec<PathBuf> {
    harnesses
        .iter()
        .map(|private| owner.join(private))
        .collect()
}

/// The walks of the owner home, of its harness homes and of the secret paths, taken once for
/// the roots one check or one sandbox build compares against them, and dropped with it, so
/// no walk outlives the moment its answer is used. Built by [`Walked::of`].
pub(crate) struct Walked<'a> {
    owner: Walk<'a>,
    harnesses: Vec<Walk<'a>>,
    secrets: Vec<Walk<'a>>,
}

impl<'a> Walked<'a> {
    /// Walks the owner home, each harness home and each secret path once. Inputs: the owner
    /// home, the harness homes ([`harness_homes`]) and the secret paths. Output: the walks,
    /// or `Invalid` with [`SECRET_ROOT_REFUSED`] when a read fails other than not found.
    pub(crate) fn of(
        owner: &'a Path,
        harnesses: &'a [PathBuf],
        secrets: &'a [PathBuf],
    ) -> Result<Walked<'a>, SandboxError> {
        let walked = Walked {
            owner: Walk::of(owner)?,
            harnesses: harnesses
                .iter()
                .map(|home| Walk::of(home.as_path()))
                .collect::<Result<_, _>>()?,
            secrets: secrets
                .iter()
                .map(|secret| Walk::of(secret.as_path()))
                .collect::<Result<_, _>>()?,
        };
        assert_eq!(walked.harnesses.len(), harnesses.len());
        assert_eq!(walked.secrets.len(), secrets.len());
        Ok(walked)
    }
}

/// Checks a root as [`granted_root`] does, against the owner home, harness homes and secrets
/// one [`Walked`] holds, walking only the root itself, once. Inputs: the root and the walks.
/// Output: unit, or `Invalid` naming the rule.
pub(crate) fn granted_walked(root: &Path, walked: &Walked) -> Result<(), SandboxError> {
    let depth = root
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    if !clean_absolute(root) || depth < 2 {
        return Err(invalid("sandbox roots must be absolute, deep and plain"));
    }
    let walk = Walk::of(root)?;
    if walked.owner.sits_in(&walk) {
        return Err(invalid("sandbox roots must not contain the owner home"));
    }
    for home in &walked.harnesses {
        if walk.sits_in(home) {
            return Err(invalid("sandbox roots must not be owner harness homes"));
        }
    }
    clear_of_walked(&walk, &walked.secrets)?;
    if !root.is_dir() || canonical(root)? != root {
        return Err(invalid(
            "sandbox roots must be existing canonical directories",
        ));
    }
    Ok(())
}

/// Refuses a walked root that equals, holds or sits inside one of the walked secrets, as
/// [`clear_of_secrets`] does. Inputs: the root's walk and the secrets' walks. Output: unit,
/// or `Invalid` with [`SECRET_ROOT_REFUSED`].
fn clear_of_walked(root: &Walk, secrets: &[Walk]) -> Result<(), SandboxError> {
    for secret in secrets {
        if root.sits_in(secret) || secret.sits_in(root) {
            return Err(invalid(SECRET_ROOT_REFUSED));
        }
    }
    Ok(())
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
