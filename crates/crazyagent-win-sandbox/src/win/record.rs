use super::{BoundaryError, Words, acl};
use crate::grants::{self, RECORD_BYTES_MAX};
use crate::plan::Ace;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

const LOCK_ATTEMPTS: u32 = 3000;
const LOCK_PAUSE: Duration = Duration::from_millis(100);

/// The access entries the boundary has put on paths for one container, read from its record
/// in the grants folder and held under that container's lock, so two launches of one agent
/// never interleave their changes.
pub(super) struct Record {
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
        let folder = Path::new(folder);
        let meta = fs::symlink_metadata(folder)
            .map_err(|_| BoundaryError::Refused("the grants folder cannot be read"))?;
        if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(BoundaryError::Refused(
                "the grants folder is not a plain folder",
            ));
        }
        let lock = lock(&folder.join(format!("container-{container}.lock")))?;
        let path = folder.join(format!("container-{container}.grants"));
        let entries = match File::open(&path) {
            Ok(file) => read(file)?,
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(_) => {
                return Err(BoundaryError::Refused(
                    "the record of granted paths cannot be read",
                ));
            }
        };
        Ok(Self {
            path,
            entries,
            _lock: lock,
        })
    }

    /// Takes back what the recorded profile granted and the plan does not, then applies the
    /// plan. The record first lists both, so a launch interrupted at any step still knows
    /// every path to withdraw, and lists only the plan once every entry is in place. Inputs:
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
        for path in &withdrawn {
            acl::revoke(container, path)?;
        }
        acl::apply(container, aces)?;
        self.save(aces.to_vec())?;
        assert_eq!(self.entries, aces);
        Ok(())
    }

    /// Takes back every recorded entry, then removes the record. Input: the container SID.
    /// Output: Ok, or the refusal or failed step, with the record kept for a retry.
    pub(super) fn release(self, container: &Words) -> Result<(), BoundaryError> {
        for path in grants::withdrawn(&self.entries, &[]) {
            acl::revoke(container, path)?;
        }
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(_) => Err(BoundaryError::Refused(
                "the record of granted paths cannot be removed",
            )),
        }
    }

    /// Replaces the record with `entries` in one step: written beside it, flushed to disk, then
    /// renamed over it, so a crash leaves the old record or the new one, never a mix.
    fn save(&mut self, entries: Vec<Ace>) -> Result<(), BoundaryError> {
        let text = grants::encode(&entries).map_err(BoundaryError::Refused)?;
        let staging = self.path.with_extension("grants-new");
        let written = File::create(&staging)
            .and_then(|mut file| {
                file.write_all(text.as_bytes())?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&staging, &self.path));
        if written.is_err() {
            let _ = fs::remove_file(&staging);
            return Err(BoundaryError::Refused(
                "the record of granted paths cannot be written",
            ));
        }
        self.entries = entries;
        Ok(())
    }
}

/// Opens the lock file with no sharing, which only one launch can hold at a time; while
/// another launch holds it, retries every 100 ms for at most five minutes.
fn lock(path: &Path) -> Result<File, BoundaryError> {
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
    Err(BoundaryError::Refused(
        "another launch of this agent is still setting its access",
    ))
}

/// Reads a record, at most one byte past its size limit, and decodes it.
fn read(file: File) -> Result<Vec<Ace>, BoundaryError> {
    let mut bytes = Vec::new();
    file.take(RECORD_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| BoundaryError::Refused("the record of granted paths cannot be read"))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| BoundaryError::Refused("the record of granted paths is damaged"))?;
    grants::decode(&text).map_err(BoundaryError::Refused)
}
