use super::super::manifest::{Manifest, Refused, VALUE};
use super::super::transcript::FileKey;
use serde_json::Value;
use std::fs::File;
use std::path::{Path, PathBuf};

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

/// Where a harness writes its terminal record: the manifest's `terminal.record.path` with
/// `{home}` and `{session}` filled in, always below the private home. A part is a plain
/// name, a name with `*` wildcards, or `**` for any number of folders, at most once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    home: PathBuf,
}

/// One file a record path matches: its place below the home, its last change and which
/// file it is.
struct Found;

impl Pattern {
    /// Reads a manifest's record path and fills it in. Inputs: the manifest, the absolute
    /// private home and the session id. Output: the pattern, or a refusal that names the
    /// setting: no path, a path not below `{home}/`, an empty, `.` or `..` part, another
    /// placeholder, a misplaced `**`, too many parts, or a session id that is no plain word.
    pub fn read(manifest: &Manifest, home: &Path, session: &str) -> Result<Self, Refused> {
        let message = "terminal.record.path: not read yet.".to_owned();
        Err(Refused {
            code: VALUE,
            message,
        })
    }

    /// Lists the files this record path matches, never following a link, looking at no more
    /// than `ENTRIES_SCANNED_MAX` folder entries and no deeper than `DEPTH_MAX` levels.
    /// Only regular files count. Output: each file's place below the home, last change and
    /// identity.
    fn found(&self) -> Vec<Found> {
        Vec::new()
    }

    /// The identities of every file this record path matches now, which a reader started
    /// before its harness never takes. Output: the identities.
    pub(super) fn keys(&self) -> Vec<FileKey> {
        Vec::new()
    }

    /// Opens the newest file this record path matches that is none of `earlier`, through
    /// `open_plain_beneath`, so no link below the home is followed. A file whose identity
    /// changed between the search and the open is not taken. Input: the identities to pass
    /// over. Output: the open file, its place below the home and its identity, or none.
    pub(super) fn newest(&self, earlier: &[FileKey]) -> Option<(File, PathBuf, FileKey)> {
        None
    }

    /// Opens one file below the home as itself: no link followed on the way, no FIFO
    /// waited on. Input: the file's place below the home. Output: the file and its
    /// identity, or none for anything but a regular file.
    pub(super) fn open(&self, below: &Path) -> Option<(File, FileKey)> {
        None
    }

    /// Tells which file is at one place below the home now, without opening it or
    /// following a link there. Input: the place. Output: its identity, or none when no
    /// regular file is there.
    pub(super) fn key_at(&self, below: &Path) -> Option<FileKey> {
        None
    }

    /// The full path of one place below the home. Input: the place. Output: the path.
    pub(super) fn full(&self, below: &Path) -> PathBuf {
        self.home.join(below)
    }
}

/// Finds where a typed prompt starts its turn in a harness record: the first object that
/// holds a string equal to the typed text, both trimmed, looked for no deeper than
/// `TYPED_DEPTH_MAX` levels and in no more than `TYPED_VALUES_MAX` values of one object.
/// Inputs: the objects in record order and the typed text. Output: the object's index, or
/// none when no object holds it or the typed text is blank.
pub fn typed_at(objects: &[Value], typed: &str) -> Option<usize> {
    None
}

#[cfg(all(test, unix))]
#[path = "json_files_tests.rs"]
pub(crate) mod tests;
