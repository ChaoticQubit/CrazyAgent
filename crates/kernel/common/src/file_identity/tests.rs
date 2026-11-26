use super::*;
use std::os::unix::fs::MetadataExt;

/// Stats a real file, folder and SQLite database in one temp folder, so every pair shares
/// the one volume they sit on. Input: none. Output: the folder guard and the three pairs.
fn three_on_one_volume() -> (tempfile::TempDir, Vec<(u64, u64)>) {
    let folder = tempfile::tempdir().unwrap();
    let file = folder.path().join("state.json");
    std::fs::write(&file, b"{}").unwrap();
    let directory = folder.path().join("home");
    std::fs::create_dir(&directory).unwrap();
    let database = folder.path().join("engine.db");
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch("CREATE TABLE t (x INTEGER)")
        .unwrap();
    let pairs = [file, directory, database]
        .iter()
        .map(|path| {
            let metadata = std::fs::metadata(path).unwrap();
            (metadata.dev(), metadata.ino())
        })
        .collect();
    (folder, pairs)
}

#[test]
fn pinned_identities_equal_to_the_live_files_match() {
    let (_folder, live) = three_on_one_volume();
    assert!(same_files(&live.clone(), &live));
}

#[test]
fn a_volume_renumbered_as_one_with_every_inode_kept_matches() {
    let (_folder, live) = three_on_one_volume();
    let pinned: Vec<_> = live.iter().map(|(dev, ino)| (dev ^ 1, *ino)).collect();
    assert!(same_files(&pinned, &live));
}

#[test]
fn a_pinned_device_that_moved_alone_is_refused() {
    let (_folder, live) = three_on_one_volume();
    let mut pinned = live.clone();
    pinned[1].0 ^= 1;
    assert!(!same_files(&pinned, &live));
}

#[test]
fn a_changed_inode_is_refused_even_on_a_renumbered_volume() {
    let (_folder, live) = three_on_one_volume();
    let mut pinned = live.clone();
    pinned[2].1 += 1;
    assert!(!same_files(&pinned, &live));
    let renumbered: Vec<_> = pinned.iter().map(|(dev, ino)| (dev ^ 1, *ino)).collect();
    assert!(!same_files(&renumbered, &live));
}

#[test]
fn no_pinned_identities_match_no_live_files() {
    assert!(same_files(&[], &[]));
}

#[test]
#[should_panic(expected = "file identities are paired")]
fn pinned_and_live_lists_of_different_lengths_are_a_caller_bug() {
    let (_folder, live) = three_on_one_volume();
    same_files(&live[..2], &live);
}
