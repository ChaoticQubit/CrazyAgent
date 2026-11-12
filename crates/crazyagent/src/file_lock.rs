use rustix::fs::{FlockOperation, flock};
use std::fs::File;

/// A file lock held through one open file description and unlocked when dropped, before
/// the file closes, so a child that another thread is starting, and that shares the
/// description until its exec, cannot keep the lock after the holder lets it go. Holds the
/// locked file. Dropping it ends the lock for every descriptor that shares it.
#[must_use = "the lock ends as soon as this guard is dropped"]
pub struct HeldLock(pub(crate) File);

impl HeldLock {
    /// Takes a shared or exclusive lock on `file`, blocking or not as `operation` says, and
    /// keeps it until the guard drops. Inputs: the open file and the lock operation. Output:
    /// the guard, or the lock error with the file closed and nothing locked.
    pub fn take(file: File, operation: FlockOperation) -> rustix::io::Result<Self> {
        assert!(
            !matches!(operation, FlockOperation::Unlock),
            "a lock guard takes a lock"
        );
        flock(&file, operation)?;
        Ok(Self(file))
    }
}

impl Drop for HeldLock {
    /// Unlocks the open file description through the guard's file, ending the lock for
    /// every descriptor that shares it. Input: the guard being dropped. Output: none.
    fn drop(&mut self) {
        let _ = flock(&self.0, FlockOperation::Unlock);
    }
}

#[cfg(test)]
#[path = "file_lock/tests.rs"]
mod tests;
