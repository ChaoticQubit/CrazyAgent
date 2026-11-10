use super::*;

/// A lock file in a fresh temp folder. Input: none. Output: the folder guard and the path.
fn lock_file() -> (tempfile::TempDir, std::path::PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("held.lock");
    std::fs::write(&path, b"").unwrap();
    (folder, path)
}

/// Says whether a new open file description can take an exclusive lock on `path` now.
/// Input: the path. Output: true when the lock is free.
fn free(path: &std::path::Path) -> bool {
    let probe = File::open(path).unwrap();
    flock(&probe, FlockOperation::NonBlockingLockExclusive).is_ok()
}

#[test]
fn a_dropped_guard_frees_the_lock_while_a_shared_descriptor_stays_open() {
    let (_folder, path) = lock_file();
    let file = File::open(&path).unwrap();
    let shared = file.try_clone().unwrap();
    let held = HeldLock::take(file, FlockOperation::NonBlockingLockExclusive).unwrap();
    assert!(!free(&path));
    drop(held);
    assert!(free(&path));
    drop(shared);
}

#[test]
fn a_lock_another_holder_has_is_refused_and_leaves_nothing_held() {
    let (_folder, path) = lock_file();
    let first = HeldLock::take(File::open(&path).unwrap(), FlockOperation::LockShared).unwrap();
    let refused = HeldLock::take(
        File::open(&path).unwrap(),
        FlockOperation::NonBlockingLockExclusive,
    );
    assert!(refused.is_err());
    drop(first);
    assert!(free(&path));
}

#[test]
#[should_panic(expected = "a lock guard takes a lock")]
fn an_unlock_is_not_a_lock_to_guard() {
    let (_folder, path) = lock_file();
    let _ = HeldLock::take(File::open(&path).unwrap(), FlockOperation::Unlock);
}
