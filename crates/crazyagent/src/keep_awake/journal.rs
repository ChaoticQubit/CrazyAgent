use crate::engine::EngineError;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

const FOLDER: &str = "keep-awake";
const FILE: &str = "journal.json";
const DRAFT: &str = ".new";
const LOCK: &str = "journal.lock";

/// The lid-closed lane's own journal file: the apps it froze and the sound it muted
/// (DEC-167 Task 17).
pub const LID_CLOSED: &str = "lid-closed.json";

/// Every journal file below `<engine root>/keep-awake`, one writer each (DEC-167 Task 17
/// ASK item 1). Engine start undoes each on its own.
pub const NAMES: [&str; 2] = [FILE, LID_CLOSED];

/// The most entries one journal holds.
pub const ENTRIES_MAX: usize = 64;

const KIND_BYTES_MAX: usize = 64;
const RESTORE_BYTES_MAX: usize = 4096;
const FILE_BYTES_MAX: u64 = 2 * 1024 * 1024;

/// One persistent change Keep awake made to this computer and has to undo: the kind of
/// change and the value that puts the old state back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub kind: String,
    pub restore: String,
}

/// The list of persistent changes not undone yet (DEC-167), in one file of `NAMES` below
/// `<engine root>/keep-awake`. An entry is on disk, synced, before its change is made, so
/// an engine that dies in between still finds it at its next start.
#[derive(Debug)]
pub struct Journal {
    folder: PathBuf,
    file: &'static str,
    entries: Vec<Entry>,
}

fn error(code: &str, problem: impl std::fmt::Display) -> EngineError {
    assert!(!code.is_empty());
    EngineError(format!("{code}: {problem}"))
}

fn unreadable(problem: impl std::fmt::Display) -> EngineError {
    error("keep_awake_journal_unreadable", problem)
}

fn unwritable(problem: impl std::fmt::Display) -> EngineError {
    error("keep_awake_journal_unwritable", problem)
}

/// Checks one entry's bounds. Input: the entry. Output: Ok, or
/// `keep_awake_journal_entry_invalid` for an empty or too long kind or a too long value.
fn valid(entry: &Entry) -> Result<(), EngineError> {
    let kind = (1..=KIND_BYTES_MAX).contains(&entry.kind.len());
    if !kind || entry.restore.len() > RESTORE_BYTES_MAX {
        return Err(EngineError("keep_awake_journal_entry_invalid".into()));
    }
    Ok(())
}

/// Creates the journal folder for the owner alone, or accepts a real folder already there.
/// Input: the folder path. Output: Ok, or why the folder cannot be used.
fn private_folder(folder: &Path) -> Result<(), EngineError> {
    #[cfg(unix)]
    let created = {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(folder)
    };
    #[cfg(not(unix))]
    let created = std::fs::DirBuilder::new().create(folder);
    match created {
        Ok(()) => Ok(()),
        Err(problem) if problem.kind() == ErrorKind::AlreadyExists => {
            let found = std::fs::symlink_metadata(folder).map_err(unwritable)?;
            if !found.file_type().is_dir() {
                return Err(unwritable("the journal folder is not a folder"));
            }
            Ok(())
        }
        Err(problem) => Err(unwritable(problem)),
    }
}

/// Opens the draft file for writing, owner-only, never through a link. Input: its path.
/// Output: the empty file.
fn draft(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// Reads a journal file, at most `FILE_BYTES_MAX`. Input: the open file. Output: its
/// entries, or `keep_awake_journal_unreadable` for a file that is too large, not a
/// journal, or holds an entry out of bounds.
fn read(file: File) -> Result<Vec<Entry>, EngineError> {
    let mut bytes = Vec::new();
    file.take(FILE_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() as u64 > FILE_BYTES_MAX {
        return Err(unreadable("the journal is too large"));
    }
    let entries: Vec<Entry> = serde_json::from_slice(&bytes).map_err(unreadable)?;
    if entries.len() > ENTRIES_MAX {
        return Err(unreadable("the journal has too many entries"));
    }
    for entry in &entries {
        valid(entry).map_err(unreadable)?;
    }
    assert!(entries.len() <= ENTRIES_MAX);
    Ok(entries)
}

/// An exclusive hold on the journal's lock file (rv-sec3 KA T14 r1 F2): the engine holds
/// it for its whole life and `crazyagent keep-awake restore` for its undo, so the journal
/// has one writing process at a time. Dropping it lets go.
#[derive(Debug)]
pub struct JournalLock {
    _file: File,
}

impl JournalLock {
    /// Takes the lock without waiting, creating the journal folder and an owner-only lock
    /// file that is never replaced, so every holder locks the same file. Input: the engine
    /// root. Output: the lock, none while another holds it, or why the lock file cannot be
    /// opened.
    pub fn try_take(root: &Path) -> Result<Option<Self>, EngineError> {
        assert!(!root.as_os_str().is_empty());
        let folder = root.join(FOLDER);
        private_folder(&folder)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(folder.join(LOCK)).map_err(unwritable)?;
        let taken = match file.try_lock() {
            Ok(()) => Some(Self { _file: file }),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Error(problem)) => return Err(unwritable(problem)),
        };
        assert!(folder.starts_with(root));
        Ok(taken)
    }
}

/// Creates the journal folder below the engine root for the owner alone, or accepts the
/// real folder already there. Input: the engine root. Output: `<engine root>/keep-awake`.
pub fn folder(root: &Path) -> Result<PathBuf, EngineError> {
    assert!(!root.as_os_str().is_empty());
    let folder = root.join(FOLDER);
    private_folder(&folder)?;
    Ok(folder)
}

impl Journal {
    /// Opens the journal below the engine root, creating its folder. A missing file is an
    /// empty journal. A file that cannot be read stays where it is and is an error, so no
    /// undo is skipped. Input: the engine root. Output: the journal.
    pub fn open(root: &Path) -> Result<Self, EngineError> {
        Self::open_named(root, FILE)
    }

    /// Opens one journal file of `NAMES` below the engine root, as `open` does. Inputs: the
    /// engine root and the file name, which must be one of `NAMES`. Output: the journal.
    pub fn open_named(root: &Path, file: &'static str) -> Result<Self, EngineError> {
        assert!(NAMES.contains(&file));
        assert!(file.ends_with(".json") && !file.contains('/'));
        let folder = folder(root)?;
        let entries = match File::open(folder.join(file)) {
            Ok(found) => read(found)?,
            Err(problem) if problem.kind() == ErrorKind::NotFound => Vec::new(),
            Err(problem) => return Err(unreadable(problem)),
        };
        assert!(entries.len() <= ENTRIES_MAX);
        Ok(Self {
            folder,
            file,
            entries,
        })
    }

    /// The changes not undone yet, oldest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Records one change before it is made: the entry is written and synced to disk when
    /// this returns Ok. Input: the entry. Output: Ok, `keep_awake_journal_entry_invalid`,
    /// `keep_awake_journal_full`, or why the file could not be written (then nothing is
    /// recorded and the change must not be made).
    pub fn record(&mut self, entry: Entry) -> Result<(), EngineError> {
        valid(&entry)?;
        if self.entries.len() >= ENTRIES_MAX {
            return Err(EngineError("keep_awake_journal_full".into()));
        }
        let mut next = self.entries.clone();
        next.push(entry);
        self.persist(&next)?;
        self.entries = next;
        assert!(!self.entries.is_empty());
        assert!(self.entries.len() <= ENTRIES_MAX);
        Ok(())
    }

    /// Undoes every recorded change, newest first, and forgets the ones whose undo worked.
    /// Input: the undo for one entry. Output: Ok when the journal is empty afterwards, else
    /// the first failure; the entries that failed stay recorded.
    pub fn reconcile(
        &mut self,
        undo: impl FnMut(&Entry) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        self.undo_where(|_| true, undo)
    }

    /// Undoes the recorded changes of one kind, newest first, and forgets the ones whose
    /// undo worked; entries of every other kind stay as they are (DEC-167 addendum 32).
    /// Inputs: the kind and the undo for one entry. Output: Ok when no entry of that kind
    /// is left, else the first failure; the entries that failed stay recorded.
    pub fn settle(
        &mut self,
        kind: &str,
        undo: impl FnMut(&Entry) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        assert!(!kind.is_empty());
        self.undo_where(|entry| entry.kind == kind, undo)
    }

    /// Undoes the chosen entries, newest first, and forgets the ones whose undo worked;
    /// every other entry stays, in its place. Inputs: which entries to undo and the undo for
    /// one entry. Output: Ok when no chosen entry is left, else the first failure.
    fn undo_where(
        &mut self,
        chosen: impl Fn(&Entry) -> bool,
        mut undo: impl FnMut(&Entry) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        let mut kept = Vec::new();
        let mut first = None;
        for entry in self.entries.iter().rev() {
            if !chosen(entry) {
                kept.push(entry.clone());
            } else if let Err(problem) = undo(entry) {
                kept.push(entry.clone());
                first = first.or(Some(problem));
            }
        }
        kept.reverse();
        assert!(kept.len() <= self.entries.len());
        assert_eq!(kept.iter().any(&chosen), first.is_some());
        if kept.len() != self.entries.len() {
            self.persist(&kept)?;
            self.entries = kept;
        }
        first.map_or(Ok(()), Err)
    }

    /// Undoes and forgets the entries of one kind, newest first, and keeps every other
    /// kind. Inputs: the kind and the undo for one entry. Output: Ok when no entry of that
    /// kind is left, else the first failure; the entries that failed stay recorded.
    pub fn settle(
        &mut self,
        kind: &str,
        mut undo: impl FnMut(&Entry) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        assert!(!kind.is_empty());
        let mut kept = Vec::new();
        let mut first = None;
        for entry in self.entries.iter().rev() {
            if entry.kind != kind {
                kept.push(entry.clone());
            } else if let Err(problem) = undo(entry) {
                kept.push(entry.clone());
                first = first.or(Some(problem));
            }
        }
        kept.reverse();
        assert!(kept.len() <= self.entries.len());
        if kept.len() != self.entries.len() {
            self.persist(&kept)?;
            self.entries = kept;
        }
        assert!(first.is_some() || self.entries.iter().all(|entry| entry.kind != kind));
        first.map_or(Ok(()), Err)
    }

    /// Replaces the journal file with the given entries: writes a draft, syncs it, renames
    /// it over the file and syncs the folder. Input: the entries. Output: Ok, or why not.
    fn persist(&self, entries: &[Entry]) -> Result<(), EngineError> {
        assert!(entries.len() <= ENTRIES_MAX);
        let bytes = serde_json::to_vec(entries).map_err(unwritable)?;
        if bytes.len() as u64 > FILE_BYTES_MAX {
            return Err(EngineError("keep_awake_journal_full".into()));
        }
        let path = self.folder.join(format!("{}{DRAFT}", self.file));
        let mut file = draft(&path).map_err(unwritable)?;
        file.write_all(&bytes).map_err(unwritable)?;
        file.sync_all().map_err(unwritable)?;
        drop(file);
        std::fs::rename(&path, self.folder.join(self.file)).map_err(unwritable)?;
        #[cfg(unix)]
        File::open(&self.folder)
            .and_then(|folder| folder.sync_all())
            .map_err(unwritable)?;
        Ok(())
    }
}
