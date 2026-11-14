use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};

static BUSY: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());
static FREED: Condvar = Condvar::new();

/// The engine's hold on one owner repository: landing work on it runs one at a time in
/// this engine. Dropping it lets the next one start.
#[derive(Debug)]
pub(super) struct Held(PathBuf);

impl Drop for Held {
    fn drop(&mut self) {
        let mut busy = BUSY.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(busy.remove(&self.0));
        FREED.notify_all();
    }
}

/// Waits for and takes the engine's hold on one owner repository.
/// Input: the repository root. Output: the hold.
#[cfg(unix)]
pub(super) fn hold(repository: &Path) -> Held {
    let mut busy = BUSY.lock().unwrap_or_else(PoisonError::into_inner);
    while busy.contains(repository) {
        busy = FREED.wait(busy).unwrap_or_else(PoisonError::into_inner);
    }
    assert!(busy.insert(repository.to_path_buf()));
    Held(repository.to_path_buf())
}

/// Takes the engine's hold on one owner repository, waiting no longer than `wait` for
/// landing work that has it. Inputs: the repository root and the wait. Output: the hold,
/// or none when the work did not end within the wait.
pub(super) fn hold_within(repository: &Path, wait: std::time::Duration) -> Option<Held> {
    let deadline = std::time::Instant::now() + wait;
    let mut busy = BUSY.lock().unwrap_or_else(PoisonError::into_inner);
    while busy.contains(repository) {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return None;
        }
        busy = FREED
            .wait_timeout(busy, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    assert!(busy.insert(repository.to_path_buf()));
    Some(Held(repository.to_path_buf()))
}

/// Tells whether someone holds the engine's hold on one owner repository right now.
/// Input: the repository root. Output: the answer.
#[cfg(all(test, unix))]
pub(super) fn busy(repository: &Path) -> bool {
    BUSY.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(repository)
}
