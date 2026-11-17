use super::{BoundaryError, Words, acl};
use crate::grants::{self, RECORD_BYTES_MAX};
use crate::plan::{Ace, same};
use crate::snapshot::{self, Snapshot};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

const LOCK_ATTEMPTS: u32 = 3000;
const LOCK_PAUSE: Duration = Duration::from_millis(100);
const SHARED_LOCK: &str = "protected.lock";
const SHARED_RECORD: &str = "protected.dacls";
const FOLDER_ITEMS_MAX: usize = 4096;

/// The access entries the boundary has put on paths for one container, read from its record
/// in the grants folder and held under that container's lock, so two launches of one agent
/// never interleave their changes.
pub(super) struct Record {
    folder: PathBuf,
    path: PathBuf,
    entries: Vec<Ace>,
    _lock: File,
}

impl Record {
    /// Takes the container's lock, waiting up to five minutes for another launch of the same
    /// agent to finish setting its access, then reads its record; a missing record is empty.
    /// The grants folder must be a plain folder, not a link or junction. Inputs: the grants
    /// folder and the container name. Output: the record, or the refusal.
    pub(super) fn open(folder: &str, container: &str) -> Result<Self, BoundaryError> {
        let folder = PathBuf::from(folder);
        let meta = fs::symlink_metadata(&folder)
            .map_err(|_| BoundaryError::Refused("the grants folder cannot be read"))?;
        if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(BoundaryError::Refused(
                "the grants folder is not a plain folder",
            ));
        }
        let lock = lock(
            &folder.join(format!("container-{container}.lock")),
            "another launch of this agent is still setting its access",
        )?;
        let path = folder.join(format!("container-{container}.grants"));
        let entries = match File::open(&path) {
            Ok(file) => {
                let text = text(file, RECORD_BYTES_MAX).map_err(|_| {
                    BoundaryError::Refused("the record of granted paths cannot be read")
                })?;
                grants::decode(&text).map_err(BoundaryError::Refused)?
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(_) => {
                return Err(BoundaryError::Refused(
                    "the record of granted paths cannot be read",
                ));
            }
        };
        Ok(Self {
            folder,
            path,
            entries,
            _lock: lock,
        })
    }

    /// Takes back what the recorded profile granted and the plan does not, then applies the
    /// plan in its order, guarded paths first (DEC-193 D1). The record first lists both, so a
    /// launch interrupted at any step still knows every path to withdraw, and lists only the
    /// plan once every entry is in place. Guarded paths change under the shared lock. Inputs:
    /// the container SID and the plan. Output: Ok, or the refusal or failed step, with the
    /// record kept for the next launch.
    pub(super) fn reconcile(
        mut self,
        container: &Words,
        aces: &[Ace],
    ) -> Result<(), BoundaryError> {
        let withdrawn: Vec<String> = grants::withdrawn(&self.entries, aces)
            .into_iter()
            .map(str::to_owned)
            .collect();
        self.save(grants::union(&self.entries, aces))?;
        let mut shared = Shared::open(&self.folder)?;
        let others = Others::read(&self.folder, &self.path);
        for path in &withdrawn {
            shared.withdraw(container, path, others.guard(path))?;
        }
        for ace in aces {
            shared.apply(container, ace)?;
        }
        drop(shared);
        self.save(aces.to_vec())?;
        assert_eq!(self.entries, aces);
        Ok(())
    }

    /// Takes back every recorded entry, a guarded path through its snapshot, then removes the
    /// record. Input: the container SID. Output: Ok, or the refusal or failed step, with the
    /// record kept for a retry.
    pub(super) fn release(self, container: &Words) -> Result<(), BoundaryError> {
        let mut shared = Shared::open(&self.folder)?;
        let others = Others::read(&self.folder, &self.path);
        for path in grants::withdrawn(&self.entries, &[]) {
            shared.withdraw(container, path, others.guard(path))?;
        }
        drop(shared);
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(_) => Err(BoundaryError::Refused(
                "the record of granted paths cannot be removed",
            )),
        }
    }

    /// Replaces the record with `entries` in one step.
    fn save(&mut self, entries: Vec<Ace>) -> Result<(), BoundaryError> {
        let text = grants::encode(&entries).map_err(BoundaryError::Refused)?;
        if !replace(&self.path, "grants-new", &text) {
            return Err(BoundaryError::Refused(
                "the record of granted paths cannot be written",
            ));
        }
        self.entries = entries;
        Ok(())
    }
}

/// The snapshots of guarded paths every container on the machine shares, read and written
/// under the shared lock, which a launch takes after its container's lock (DEC-193 D2).
struct Shared {
    path: PathBuf,
    snapshots: Vec<Snapshot>,
    _lock: File,
}

impl Shared {
    /// Takes the shared lock, waiting up to five minutes, then reads the shared record; a
    /// missing record is empty. Input: the grants folder. Output: the record, or the refusal.
    fn open(folder: &Path) -> Result<Self, BoundaryError> {
        const UNREADABLE: &str = "the record of guarded paths cannot be read";
        let lock = lock(
            &folder.join(SHARED_LOCK),
            "another agent's launch is still setting access on guarded paths",
        )?;
        let path = folder.join(SHARED_RECORD);
        let snapshots = match File::open(&path) {
            Ok(file) => {
                let text = text(file, snapshot::RECORD_BYTES_MAX)
                    .map_err(|_| BoundaryError::Refused(UNREADABLE))?;
                snapshot::decode(&text).map_err(BoundaryError::Refused)?
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(_) => return Err(BoundaryError::Refused(UNREADABLE)),
        };
        assert!(snapshots.len() <= snapshot::PATHS_MAX);
        Ok(Self {
            path,
            snapshots,
            _lock: lock,
        })
    }

    /// Applies one planned role, saving a guarded path's snapshot before its list changes, so
    /// a launch stopped between the two still knows the original.
    fn apply(&mut self, container: &Words, ace: &Ace) -> Result<(), BoundaryError> {
        let step = acl::role(container, ace, self.find(&ace.path))?;
        let cut = ace.access.cuts();
        assert!(!cut || step.snapshot.is_some() || step.change.is_none());
        if let Some(snapshot) = step.snapshot {
            self.keep(&ace.path, Some(snapshot))?;
        }
        if let Some(change) = &step.change {
            acl::write(change)?;
        }
        Ok(())
    }

    /// Takes the container off one path, then updates the path's snapshot, so a launch stopped
    /// between the two finds the path as released and its snapshot as it was. When the owner
    /// changed a guarded path's list, it says so on standard error, the helper's log.
    fn withdraw(
        &mut self,
        container: &Words,
        path: &str,
        guarded_by_others: bool,
    ) -> Result<(), BoundaryError> {
        let saved = self.find(path).cloned();
        let withdrawal = acl::withdrawal(container, path, saved.as_ref(), guarded_by_others)?;
        assert!(saved.is_some() || withdrawal.snapshot.is_none());
        if let Some(change) = &withdrawal.change {
            acl::write(change)?;
        }
        if withdrawal.owner_changed {
            eprintln!(
                "crazyagent-win-boundary: the access list of {path} changed while it was \
                 guarded; it stays as changed, without this agent's entries"
            );
        }
        if saved.is_some() {
            self.keep(path, withdrawal.snapshot)?;
        }
        Ok(())
    }

    fn find(&self, path: &str) -> Option<&Snapshot> {
        self.snapshots.iter().find(|saved| same(&saved.path, path))
    }

    /// Puts what the shared record keeps for `path` in place, None removing it, and writes
    /// the record in one step when that changed it.
    fn keep(&mut self, path: &str, snapshot: Option<Snapshot>) -> Result<(), BoundaryError> {
        let at = self
            .snapshots
            .iter()
            .position(|saved| same(&saved.path, path));
        let before = self.snapshots.len();
        match (at, snapshot) {
            (Some(at), Some(snapshot)) if self.snapshots[at] == snapshot => return Ok(()),
            (Some(at), Some(snapshot)) => self.snapshots[at] = snapshot,
            (Some(at), None) => {
                self.snapshots.remove(at);
            }
            (None, Some(snapshot)) => self.snapshots.push(snapshot),
            (None, None) => return Ok(()),
        }
        assert!(self.snapshots.len().abs_diff(before) <= 1);
        let text = snapshot::encode(&self.snapshots).map_err(BoundaryError::Refused)?;
        if !replace(&self.path, "dacls-new", &text) {
            return Err(BoundaryError::Refused(
                "the record of guarded paths cannot be written",
            ));
        }
        Ok(())
    }
}

/// The guarded paths the other containers' records name, or every path when one of them, or
/// the folder, cannot be read, so a release never brings back an original list another
/// container may still rely on.
enum Others {
    Paths(Vec<String>),
    All,
}

impl Others {
    /// Reads every other container's record in the grants folder, keeping the paths a role
    /// that cuts names; a record removed meanwhile was released and guards nothing. Inputs:
    /// the grants folder and this container's record. Output: the paths.
    fn read(folder: &Path, own: &Path) -> Self {
        let Ok(listing) = fs::read_dir(folder) else {
            return Self::All;
        };
        let mut paths: Vec<String> = Vec::new();
        for (index, item) in listing.enumerate() {
            let Ok(item) = item else {
                return Self::All;
            };
            if index == FOLDER_ITEMS_MAX {
                return Self::All;
            }
            let path = item.path();
            let name = item.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("container-") || !name.ends_with(".grants") || path == own {
                continue;
            }
            let file = match File::open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(_) => return Self::All,
            };
            let read = text(file, RECORD_BYTES_MAX).ok();
            let Some(entries) = read.and_then(|text| grants::decode(&text).ok()) else {
                return Self::All;
            };
            let guarded = entries.into_iter().filter(|entry| entry.access.cuts());
            paths.extend(guarded.map(|entry| entry.path));
        }
        assert!(paths.len() <= FOLDER_ITEMS_MAX * grants::ENTRIES_MAX);
        Self::Paths(paths)
    }

    /// Whether another container still guards `path`.
    fn guard(&self, path: &str) -> bool {
        match self {
            Self::All => true,
            Self::Paths(paths) => paths.iter().any(|guarded| same(guarded, path)),
        }
    }
}

/// Opens the lock file with no sharing, which only one launch can hold at a time; while
/// another launch holds it, retries every 100 ms for at most five minutes. Inputs: the lock
/// file and the refusal when it stays busy. Output: the held lock, or the refusal.
fn lock(path: &Path, busy: &'static str) -> Result<File, BoundaryError> {
    for _ in 0..LOCK_ATTEMPTS {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(path)
        {
            Ok(file) => return Ok(file),
            Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) => {
                std::thread::sleep(LOCK_PAUSE);
            }
            Err(_) => {
                return Err(BoundaryError::Refused(
                    "the grants folder cannot be written",
                ));
            }
        }
    }
    Err(BoundaryError::Refused(busy))
}

/// Reads a record's text, at most one byte past its size limit so a longer one is refused
/// when decoded. Inputs: the open file and the limit. Output: the text, or the read error.
fn text(file: File, max: usize) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    assert!(bytes.len() <= max + 1);
    String::from_utf8(bytes).map_err(|error| std::io::Error::new(ErrorKind::InvalidData, error))
}

/// Replaces a record with `text` in one step: written beside it under the `staging`
/// extension, flushed to disk, then renamed over it, so a crash leaves the old record or the
/// new one, never a mix. Inputs: the record, the staging extension and the text. Output:
/// whether it was written.
fn replace(path: &Path, staging: &str, text: &str) -> bool {
    let staging = path.with_extension(staging);
    assert_ne!(staging, path);
    let written = File::create(&staging)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&staging, path));
    if written.is_err() {
        let _ = fs::remove_file(&staging);
    }
    written.is_ok()
}
