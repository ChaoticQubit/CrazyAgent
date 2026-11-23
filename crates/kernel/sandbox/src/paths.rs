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

/// The code points outside ASCII that a case-insensitive volume folds onto ASCII, with the
/// ASCII they fold onto: on APFS the full case fold of their canonical decomposition, each
/// checked on the volume (probe 2026-10-28), and U+0131 DOTLESS I, which the NTFS upcase
/// table maps to `I`. The one list the landing checks, the name compares and the Seatbelt
/// name spellings share (SA vid 202, DS-148 F1).
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

/// Refuses a root that equals, holds or sits inside one of `secrets`. Inputs: the root and
/// the secret paths. Output: unit, or `Invalid` with [`SECRET_ROOT_REFUSED`].
pub fn clear_of_secrets(root: &Path, secrets: &[PathBuf]) -> Result<(), SandboxError> {
    if secrets
        .iter()
        .any(|secret| root.starts_with(secret) || secret.starts_with(root))
    {
        return Err(invalid(SECRET_ROOT_REFUSED));
    }
    Ok(())
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
    let depth = root
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    if !clean_absolute(root) || depth < 2 {
        return Err(invalid("sandbox roots must be absolute, deep and plain"));
    }
    if owner.starts_with(root) {
        return Err(invalid("sandbox roots must not contain the owner home"));
    }
    for private in harnesses {
        if root.starts_with(owner.join(private)) {
            return Err(invalid("sandbox roots must not be owner harness homes"));
        }
    }
    clear_of_secrets(root, secrets)?;
    if !root.is_dir() || canonical(root)? != root {
        return Err(invalid(
            "sandbox roots must be existing canonical directories",
        ));
    }
    Ok(())
}

/// Spells plain text as a Seatbelt regex that matches every name a case-insensitive volume
/// finds under it: each letter in both cases with every code point that folds onto it or
/// onto a run of two or three letters ([`spelled`]), so the rule never leans on how Seatbelt
/// folds names itself (SA vid 202). A stretch with more than [`SPELLINGS_MAX`] spellings, a
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
/// one; past it path text takes the [`superset`] (B5 r2).
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
/// each matches its whole UTF-8 sequence (LL2S-2). Input: the character. Output: the pattern.
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

#[cfg(test)]
mod tests;
