use super::super::manifest::{MISSING, Manifest, Refused, TYPE, VALUE};
use super::super::transcript::{FileKey, file_key};
use crate::app_plugins::yaml::Value as Yaml;
use serde_json::Value;
use std::fs::{File, Metadata};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The most levels below the private home a record lies, the record itself included.
pub const DEPTH_MAX: usize = 8;
/// The most folder entries, and names looked up, one search of a record path takes.
pub const ENTRIES_SCANNED_MAX: usize = 512;
/// The most bytes of the session id a record path is filled with.
pub const SESSION_BYTES_MAX: usize = 64;
/// The most bytes of a record path as its manifest writes it.
pub const PATH_BYTES_MAX: usize = 512;
/// The most levels of one JSON object [`typed_at`] looks through.
pub const TYPED_DEPTH_MAX: usize = 16;
/// The most values of one JSON object [`typed_at`] looks at.
pub const TYPED_VALUES_MAX: usize = 4096;
const SETTING: &str = "terminal.record.path";
const HOME: &str = "{home}/";
const SESSION: &str = "{session}";
const DEEP: &str = "**";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    Name(String),
    Glob(String),
    Deep,
}

/// Where a harness writes its terminal record: the manifest's `terminal.record.path` with
/// `{home}` and `{session}` filled in, always below the private home. A part is a plain
/// name, a name with `*` wildcards, or `**` for any number of folders, at most once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    home: PathBuf,
    parts: Vec<Part>,
}

/// One file a record path matches: its place below the home, its last change and which
/// file it is.
struct Found {
    below: PathBuf,
    modified: SystemTime,
    key: FileKey,
}

/// One search of a record path: the folder entries and names looked at so far and the
/// files found.
struct Search {
    scanned: usize,
    found: Vec<Found>,
}

impl Pattern {
    /// Reads a manifest's record path and fills it in. Inputs: the manifest, the absolute
    /// private home and the session id. Output: the pattern, or a refusal that names the
    /// setting: no path, a path not below `{home}/`, an empty, `.` or `..` part, another
    /// placeholder, a misplaced `**`, too many parts, or a session id that is no plain word.
    pub fn read(manifest: &Manifest, home: &Path, session: &str) -> Result<Self, Refused> {
        assert!(home.is_absolute());
        let written = manifest
            .block("terminal")
            .and_then(|terminal| terminal.get("record"))
            .and_then(|record| record.get("path"));
        let text = match written {
            None | Some(Yaml::Null) => return refused(MISSING, "a record reader needs one"),
            Some(Yaml::Str(text)) => text,
            Some(_) => return refused(TYPE, "this must be text"),
        };
        if !session_fits(session) {
            let rule = format!("1 to {SESSION_BYTES_MAX} letters, digits, - or _");
            return refused(VALUE, &format!("the session id must be {rule}"));
        }
        if text.len() > PATH_BYTES_MAX {
            let most = format!("this must be at most {PATH_BYTES_MAX} bytes");
            return refused(VALUE, &most);
        }
        let Some(rest) = text.strip_prefix(HOME) else {
            return refused(VALUE, "the record must be below {home}/");
        };
        let mut parts = Vec::new();
        for part in rest.split('/') {
            if part.is_empty() || part == "." || part == ".." || part.contains('\\') {
                return refused(VALUE, "each part must be a plain name");
            }
            if part.replace(SESSION, "").contains(['{', '}']) {
                return refused(VALUE, "only {home} and {session} are filled in here");
            }
            let part = part.replace(SESSION, session);
            if part.contains(DEEP) && part != DEEP {
                return refused(VALUE, "** must be a whole part");
            }
            parts.push(if part == DEEP {
                Part::Deep
            } else if part.contains('*') {
                Part::Glob(part)
            } else {
                Part::Name(part)
            });
        }
        if parts.len() > DEPTH_MAX {
            let most = format!("this must be at most {DEPTH_MAX} parts below {{home}}");
            return refused(VALUE, &most);
        }
        let deep = parts.iter().filter(|part| **part == Part::Deep).count();
        if deep > 1 || parts.last() == Some(&Part::Deep) {
            return refused(VALUE, "** may stand once, and never for the file itself");
        }
        assert!(!parts.is_empty());
        let home = home.to_path_buf();
        Ok(Self { home, parts })
    }

    /// Lists the files this record path matches, never following a link, looking at no more
    /// than `ENTRIES_SCANNED_MAX` folder entries and no deeper than `DEPTH_MAX` levels.
    /// Only regular files count. Output: each file's place below the home, last change and
    /// identity.
    fn found(&self) -> Vec<Found> {
        let mut search = Search {
            scanned: 0,
            found: Vec::new(),
        };
        self.walk(Path::new(""), 0, &mut search);
        assert!(search.scanned <= ENTRIES_SCANNED_MAX);
        assert!(search.found.len() <= search.scanned);
        search.found
    }

    /// Matches part `at` and those after it in one folder below the home. `**` matches the
    /// folder itself and, one level at a time, each real folder in it. Inputs: the folder,
    /// the part and the search so far. Output: nothing; matches join the search.
    fn walk(&self, folder: &Path, at: usize, search: &mut Search) {
        assert!(at < self.parts.len());
        if folder.components().count() >= DEPTH_MAX {
            return;
        }
        match &self.parts[at] {
            Part::Deep => {
                assert!(at + 1 < self.parts.len());
                self.walk(folder, at + 1, search);
                for (name, metadata) in self.entries(folder, search) {
                    if metadata.is_dir() {
                        self.walk(&folder.join(name), at, search);
                    }
                }
            }
            Part::Name(name) => {
                if search.scanned >= ENTRIES_SCANNED_MAX {
                    return;
                }
                search.scanned += 1;
                let below = folder.join(name);
                if let Ok(metadata) = std::fs::symlink_metadata(self.home.join(&below)) {
                    self.step(below, &metadata, at, search);
                }
            }
            Part::Glob(glob) => {
                for (name, metadata) in self.entries(folder, search) {
                    if fits(glob, &name) {
                        self.step(folder.join(name), &metadata, at, search);
                    }
                }
            }
        }
    }

    /// Goes on from one name that matched part `at`: a regular file is found when the part
    /// is the last, a real folder is searched on for the next part, and a link or anything
    /// else ends there. Inputs: the name's place, its own metadata, the part and the search.
    fn step(&self, below: PathBuf, metadata: &Metadata, at: usize, search: &mut Search) {
        if at + 1 < self.parts.len() {
            if metadata.is_dir() {
                self.walk(&below, at + 1, search);
            }
            return;
        }
        if metadata.is_file() {
            let key = file_key(metadata, &self.home.join(&below));
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            search.found.push(Found {
                below,
                modified,
                key,
            });
        }
    }

    /// Lists one folder below the home with each entry's own metadata, a link read as a
    /// link, until the search has looked at `ENTRIES_SCANNED_MAX` entries. A name that is
    /// not UTF-8 is passed over. Inputs: the folder and the search. Output: the entries.
    fn entries(&self, folder: &Path, search: &mut Search) -> Vec<(String, Metadata)> {
        let Ok(listing) = std::fs::read_dir(self.home.join(folder)) else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        for entry in listing.flatten() {
            if search.scanned >= ENTRIES_SCANNED_MAX {
                break;
            }
            search.scanned += 1;
            let name = entry.file_name().into_string();
            if let (Ok(name), Ok(metadata)) = (name, entry.metadata()) {
                entries.push((name, metadata));
            }
        }
        entries
    }

    /// The identities of every file this record path matches now, which a reader started
    /// before its harness never takes. Output: the identities.
    pub(super) fn keys(&self) -> Vec<FileKey> {
        self.found().into_iter().map(|found| found.key).collect()
    }

    /// Opens the newest file this record path matches that is none of `earlier`, through
    /// `open_plain_beneath`, so no link below the home is followed. A file whose identity
    /// changed between the search and the open is not taken. Input: the identities to pass
    /// over. Output: the open file, its place below the home and its identity, or none.
    pub(super) fn newest(&self, earlier: &[FileKey]) -> Option<(File, PathBuf, FileKey)> {
        let newest = self
            .found()
            .into_iter()
            .filter(|found| !earlier.contains(&found.key))
            .max_by_key(|found| (found.modified, found.below.clone()))?;
        let (file, key) = self.open(&newest.below)?;
        (key == newest.key).then_some((file, newest.below, key))
    }

    /// Opens one file below the home as itself: no link followed on the way, no FIFO
    /// waited on. Input: the file's place below the home. Output: the file and its
    /// identity, or none for anything but a regular file.
    pub(super) fn open(&self, below: &Path) -> Option<(File, FileKey)> {
        let file = crate::platform::open_plain_beneath(&self.home, below).ok()?;
        let metadata = file.metadata().ok()?;
        let key = file_key(&metadata, &self.home.join(below));
        metadata.is_file().then_some((file, key))
    }

    /// Tells which file is at one place below the home now, without opening it or
    /// following a link there. Input: the place. Output: its identity, or none when no
    /// regular file is there.
    pub(super) fn key_at(&self, below: &Path) -> Option<FileKey> {
        let path = self.home.join(below);
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        metadata.is_file().then(|| file_key(&metadata, &path))
    }

    /// The full path of one place below the home. Input: the place. Output: the path.
    pub(super) fn full(&self, below: &Path) -> PathBuf {
        self.home.join(below)
    }
}

fn refused<T>(code: &'static str, problem: &str) -> Result<T, Refused> {
    let message = format!("{SETTING}: {problem}.");
    Err(Refused { code, message })
}

fn session_fits(session: &str) -> bool {
    (1..=SESSION_BYTES_MAX).contains(&session.len())
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Tells whether a name fits a part with `*` wildcards, each standing for any run of
/// characters, none included. Inputs: the part and the name. Output: true when it fits.
fn fits(glob: &str, name: &str) -> bool {
    let (glob, name) = (glob.as_bytes(), name.as_bytes());
    let (mut at_glob, mut at_name) = (0, 0);
    let mut star = None;
    while at_name < name.len() {
        if at_glob < glob.len() && glob[at_glob] == b'*' {
            star = Some((at_glob, at_name));
            at_glob += 1;
        } else if at_glob < glob.len() && glob[at_glob] == name[at_name] {
            at_glob += 1;
            at_name += 1;
        } else if let Some((star_glob, star_name)) = star {
            at_glob = star_glob + 1;
            at_name = star_name + 1;
            star = Some((star_glob, star_name + 1));
        } else {
            return false;
        }
    }
    glob[at_glob..].iter().all(|byte| *byte == b'*')
}

/// Finds where a typed prompt starts its turn in a harness record: the first object that
/// holds a string equal to the typed text, both trimmed, looked for no deeper than
/// `TYPED_DEPTH_MAX` levels and in no more than `TYPED_VALUES_MAX` values of one object.
/// Inputs: the objects in record order and the typed text. Output: the object's index, or
/// none when no object holds it or the typed text is blank.
pub fn typed_at(objects: &[Value], typed: &str) -> Option<usize> {
    let typed = typed.trim();
    if typed.is_empty() {
        return None;
    }
    objects.iter().position(|object| holds(object, typed))
}

/// Tells whether one JSON value holds a string equal to the trimmed typed text, looking
/// no deeper than `TYPED_DEPTH_MAX` levels and at no more than `TYPED_VALUES_MAX` values.
/// Inputs: the value and the typed text. Output: true when it holds it.
fn holds(value: &Value, typed: &str) -> bool {
    let mut left = vec![(value, 0)];
    let mut looked = 0;
    while let Some((value, depth)) = left.pop() {
        looked += 1;
        assert!(looked + left.len() <= TYPED_VALUES_MAX);
        let room = TYPED_VALUES_MAX - looked - left.len();
        match value {
            Value::String(text) if text.trim() == typed => return true,
            Value::Array(items) if depth < TYPED_DEPTH_MAX => {
                left.extend(items.iter().take(room).map(|item| (item, depth + 1)));
            }
            Value::Object(fields) if depth < TYPED_DEPTH_MAX => {
                left.extend(fields.values().take(room).map(|item| (item, depth + 1)));
            }
            _ => {}
        }
    }
    false
}

#[cfg(all(test, unix))]
#[path = "json_files_tests.rs"]
pub(crate) mod tests;
