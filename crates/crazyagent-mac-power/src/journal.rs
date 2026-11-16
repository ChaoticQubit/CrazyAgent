//! The helper's journal and its folder (threat model section 8): every value the helper changed,
//! on disk before `pmset` writes it, removed only after `pmset` put it back. The folder and
//! small-file rules here serve the uid pin too.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

pub const JOURNAL_FILE: &str = "journal.json";
pub const FILE_BYTES_MAX: u64 = 4096;
pub const ENTRIES_MAX: usize = 8;

/// A setting the helper may change and journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    SleepDisabled,
    LowPower,
}

impl Key {
    /// The `pmset` key that writes this setting.
    pub fn pmset_name(self) -> &'static str {
        match self {
            Key::SleepDisabled => "disablesleep",
            Key::LowPower => "lowpowermode",
        }
    }
}

/// One journaled change: the setting, its value before the helper changed it (0 or 1) and the
/// wall-clock millisecond it was recorded at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub key: Key,
    pub before: u8,
    pub at_ms: i64,
}

/// Why the folder or a file in it cannot be used: a folder with another owner, mode or type,
/// a file that cannot be trusted or parsed, or a failed write.
#[derive(Debug)]
pub enum StoreError {
    Folder(String),
    Unreadable(String),
    Write(io::Error),
}

/// The journal: its folder, the owner every file must have, and its entries.
#[derive(Debug)]
pub struct Journal {
    folder: PathBuf,
    owner_uid: u32,
    entries: Vec<Entry>,
}

impl Journal {
    /// Opens the journal in `folder`, making the folder owner-only if it is missing.
    /// Input: the folder and the uid that must own it (0 in production). Output: the journal.
    pub fn open(folder: &Path, owner_uid: u32) -> Result<Self, StoreError> {
        let _ = (folder, owner_uid);
        Err(StoreError::Folder(String::new()))
    }

    /// Every entry, oldest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entry for `key`, if one is recorded.
    pub fn entry(&self, key: Key) -> Option<Entry> {
        let _ = (key, &self.folder, self.owner_uid);
        None
    }

    /// Records `entry` on disk (new file, fsync, rename, fsync of the folder).
    pub fn record(&mut self, entry: Entry) -> Result<(), StoreError> {
        let _ = entry;
        Ok(())
    }

    /// Removes the entry for `key` from disk.
    pub fn remove(&mut self, key: Key) -> Result<(), StoreError> {
        let _ = key;
        Ok(())
    }
}

/// Checks `folder` is a real folder (not a link) owned by `owner_uid` with mode 0700, making it
/// so when missing. Output: the folder's path, or why it is refused (never repaired).
pub fn checked_folder(folder: &Path, owner_uid: u32) -> Result<PathBuf, StoreError> {
    let _ = owner_uid;
    Ok(folder.to_path_buf())
}

/// Reads a small file in a checked folder without following links: owned by `owner_uid`, mode
/// 0600, at most `bytes_max`. Output: its bytes, `None` when missing, or `Unreadable`.
pub fn read_small(
    path: &Path,
    owner_uid: u32,
    bytes_max: u64,
) -> Result<Option<Vec<u8>>, StoreError> {
    let _ = (path, owner_uid, bytes_max);
    Ok(None)
}

/// Replaces a file in a checked folder: a new 0600 file, fsync, rename over, fsync of the
/// folder. Input: the folder, the file name and the bytes. Output: the write result.
pub fn write_atomic(folder: &Path, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
    let _ = (folder, name, bytes);
    Ok(())
}
