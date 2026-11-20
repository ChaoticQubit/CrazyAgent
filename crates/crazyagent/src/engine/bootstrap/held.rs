use crate::engine::EngineError;
use std::ffi::OsStr;
use std::fs::File;
use std::path::Path;

/// One file's identity, compared to tell a swapped name from the held entry: device and
/// inode on Unix, volume serial number and file id on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Pin {
    pub(super) device: u64,
    pub(super) inode: u64,
}

/// A folder native profile custody holds open and opens its children through, so a name
/// is always looked up in the held folder, never through a link (DESIGN-0515 D1).
pub(super) struct Folder {
    file: File,
    pin: Pin,
}

fn unavailable() -> EngineError {
    EngineError("native_profile_custody_unavailable".into())
}

impl Folder {
    /// Holds the first folder of a chain. Input: its absolute path. Output: the held folder,
    /// or the refusal.
    pub(super) fn root(_path: &Path) -> Result<Self, EngineError> {
        Err(unavailable())
    }

    /// Opens and holds one child folder. Input: its name. Output: the held child, or the
    /// refusal.
    pub(super) fn open_dir(&self, _name: &OsStr) -> Result<Self, EngineError> {
        Err(unavailable())
    }

    /// Makes one private child folder and holds it. Input: its name. Output: the held child,
    /// or the refusal.
    pub(super) fn make_dir(&self, _name: &OsStr) -> Result<Self, EngineError> {
        Err(unavailable())
    }

    /// Opens one child file for reading. Input: its name. Output: the file and its identity,
    /// or the refusal.
    pub(super) fn open_read(&self, _name: &OsStr) -> Result<(File, Pin), EngineError> {
        Err(unavailable())
    }

    /// Checks that a name in this folder still names a held entry, safely. Inputs: the name,
    /// the held entry, its identity and whether it is a folder. Output: Ok, or the refusal.
    pub(super) fn verify(
        &self,
        _name: &OsStr,
        _held: &File,
        _pin: Pin,
        _directory: bool,
    ) -> Result<(), EngineError> {
        Err(unavailable())
    }

    /// The held folder's identity. Output: the pin.
    pub(super) fn pin(&self) -> Pin {
        self.pin
    }

    /// The held folder's handle. Output: the file.
    pub(super) fn file(&self) -> &File {
        &self.file
    }
}

/// Takes one lock on a held file without waiting. Inputs: the file and whether the lock is
/// shared. Output: Ok, `native_profile_in_use` while a conflicting lock is held, or the lock
/// failure.
pub(super) fn lock(_file: &File, _shared: bool) -> Result<(), EngineError> {
    Err(unavailable())
}

/// Releases a lock this handle took, now and not at the handle's close. Input: the file.
pub(super) fn unlock(_file: &File) {}

#[cfg(test)]
#[path = "held_tests.rs"]
mod tests;
