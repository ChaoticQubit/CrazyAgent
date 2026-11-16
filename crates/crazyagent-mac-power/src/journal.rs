//! The helper's journal and its folder (threat model section 8): every value the helper changed,
//! on disk before `pmset` writes it, removed only after `pmset` put it back. The folder and
//! small-file rules here serve the uid pin too.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
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
        let folder = checked_folder(folder, owner_uid)?;
        let path = folder.join(JOURNAL_FILE);
        let entries = match read_small(&path, owner_uid, FILE_BYTES_MAX)? {
            Some(bytes) => parse_entries(&bytes)?,
            None => Vec::new(),
        };
        assert!(entries.len() <= ENTRIES_MAX);
        Ok(Self {
            folder,
            owner_uid,
            entries,
        })
    }

    /// Every entry, oldest first.
    pub fn entries(&self) -> &[Entry] {
        assert!(self.entries.len() <= ENTRIES_MAX);
        &self.entries
    }

    /// The entry for `key`, if one is recorded.
    pub fn entry(&self, key: Key) -> Option<Entry> {
        self.entries.iter().find(|entry| entry.key == key).copied()
    }

    /// Records `entry` on disk (new file, fsync, rename, fsync of the folder).
    pub fn record(&mut self, entry: Entry) -> Result<(), StoreError> {
        assert!(entry.before <= 1);
        assert!(self.entry(entry.key).is_none());
        let mut next = self.entries.clone();
        next.push(entry);
        self.store(next)?;
        assert_eq!(self.entry(entry.key), Some(entry));
        Ok(())
    }

    /// Removes the entry for `key` from disk.
    pub fn remove(&mut self, key: Key) -> Result<(), StoreError> {
        let next: Vec<Entry> = self
            .entries
            .iter()
            .filter(|entry| entry.key != key)
            .copied()
            .collect();
        self.store(next)?;
        assert!(self.entry(key).is_none());
        Ok(())
    }

    /// Writes `next` over the journal file after checking the folder again, then keeps it.
    fn store(&mut self, next: Vec<Entry>) -> Result<(), StoreError> {
        assert!(next.len() <= ENTRIES_MAX);
        let bytes = serde_json::to_vec(&next).expect("entries always serialize");
        assert!(bytes.len() as u64 <= FILE_BYTES_MAX);
        let folder = checked_folder(&self.folder, self.owner_uid)?;
        write_atomic(&folder, JOURNAL_FILE, &bytes)?;
        self.entries = next;
        Ok(())
    }
}

/// Parses the journal file: a JSON array of at most `ENTRIES_MAX` entries, each `before` 0 or
/// 1, one entry per key. Output: the entries, or `Unreadable`.
fn parse_entries(bytes: &[u8]) -> Result<Vec<Entry>, StoreError> {
    let entries: Vec<Entry> =
        serde_json::from_slice(bytes).map_err(|error| unreadable(&error.to_string()))?;
    if entries.len() > ENTRIES_MAX {
        return Err(unreadable("too many entries"));
    }
    if entries.iter().any(|entry| entry.before > 1) {
        return Err(unreadable("a value other than 0 or 1"));
    }
    let twice = entries.iter().enumerate().any(|(index, entry)| {
        let earlier = &entries[..index];
        earlier.iter().any(|other| other.key == entry.key)
    });
    if twice {
        return Err(unreadable("a key recorded twice"));
    }
    Ok(entries)
}

/// An `Unreadable` error with its reason.
fn unreadable(reason: &str) -> StoreError {
    StoreError::Unreadable(reason.to_string())
}

/// Checks `folder` is a real folder (not a link) owned by `owner_uid` with mode 0700, making it
/// so when missing. Output: the folder's path, or why it is refused (never repaired).
pub fn checked_folder(folder: &Path, owner_uid: u32) -> Result<PathBuf, StoreError> {
    let refused = |reason: String| StoreError::Folder(format!("{}: {reason}", folder.display()));
    let metadata = match fs::symlink_metadata(folder) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            make_folder(folder).map_err(|error| refused(error.to_string()))?;
            fs::symlink_metadata(folder).map_err(|error| refused(error.to_string()))?
        }
        Err(error) => return Err(refused(error.to_string())),
    };
    if !metadata.file_type().is_dir() {
        return Err(refused("not a real folder".to_string()));
    }
    if metadata.uid() != owner_uid {
        return Err(refused(format!("owner {}", metadata.uid())));
    }
    if metadata.mode() & 0o7777 != 0o700 {
        return Err(refused(format!("mode {:o}", metadata.mode() & 0o7777)));
    }
    Ok(folder.to_path_buf())
}

/// Makes a missing folder with mode 0700. Input: the folder. Output: the result.
fn make_folder(folder: &Path) -> io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(folder)?;
    fs::set_permissions(folder, fs::Permissions::from_mode(0o700))
}

/// Reads a small file in a checked folder without following links: owned by `owner_uid`, mode
/// 0600, at most `bytes_max`. Output: its bytes, `None` when missing, or `Unreadable`.
pub fn read_small(
    path: &Path,
    owner_uid: u32,
    bytes_max: u64,
) -> Result<Option<Vec<u8>>, StoreError> {
    let opened = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path);
    let file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unreadable(&error.to_string())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| unreadable(&error.to_string()))?;
    if !metadata.is_file() || metadata.uid() != owner_uid {
        return Err(unreadable("not a regular file of the owner"));
    }
    if metadata.mode() & 0o7777 != 0o600 || metadata.len() > bytes_max {
        return Err(unreadable("wrong mode or too big"));
    }
    let mut bytes = Vec::new();
    let read = (&file).take(bytes_max + 1).read_to_end(&mut bytes);
    read.map_err(|error| unreadable(&error.to_string()))?;
    if bytes.len() as u64 > bytes_max {
        return Err(unreadable("too big"));
    }
    Ok(Some(bytes))
}

/// Replaces a file in a checked folder: a new 0600 file, fsync, rename over, fsync of the
/// folder. Input: the folder, the file name and the bytes. Output: the write result.
pub fn write_atomic(folder: &Path, name: &str, bytes: &[u8]) -> Result<(), StoreError> {
    assert!(!name.is_empty());
    assert!(!name.contains('/'));
    assert!(bytes.len() as u64 <= FILE_BYTES_MAX);
    let temp = folder.join(format!(".{name}.new"));
    match fs::remove_file(&temp) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(StoreError::Write(error)),
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)
        .map_err(StoreError::Write)?;
    file.write_all(bytes).map_err(StoreError::Write)?;
    file.sync_all().map_err(StoreError::Write)?;
    drop(file);
    fs::rename(&temp, folder.join(name)).map_err(StoreError::Write)?;
    sync_folder(folder)
}

/// Flushes a folder's entries to disk after a rename or unlink. Input: the folder.
pub fn sync_folder(folder: &Path) -> Result<(), StoreError> {
    let handle = fs::File::open(folder).map_err(StoreError::Write)?;
    handle.sync_all().map_err(StoreError::Write)
}
