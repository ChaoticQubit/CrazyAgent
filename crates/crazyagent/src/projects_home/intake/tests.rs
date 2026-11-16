use super::*;

#[test]
fn a_move_deletes_only_the_source_entries_still_as_its_check_saw_them() {
    let root = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(root.path()).unwrap();
    let source = base.join("source");
    fs::create_dir_all(source.join("sub/deep")).unwrap();
    fs::write(source.join("a.txt"), "a\n").unwrap();
    fs::write(source.join("sub/b.txt"), "b\n").unwrap();
    fs::write(source.join("sub/deep/c.txt"), "c\n").unwrap();
    std::os::unix::fs::symlink("b.txt", source.join("sub/link")).unwrap();
    let landed = base.join("landed");
    let verified = copy_into(
        &source,
        &landed,
        Copying::Exact,
        COPY_BOUNDS,
        &Tally::default(),
    )
    .unwrap();
    fs::write(source.join("sub/synced.txt"), "from another Mac\n").unwrap();
    fs::write(source.join("a.txt"), "A\n").unwrap();

    let left = remove_verified(&source, &landed, &verified, COPY_BOUNDS).unwrap_err();
    assert_eq!(left.0, "home_move_source_left");
    assert_eq!(
        fs::read_to_string(source.join("sub/synced.txt")).unwrap(),
        "from another Mac\n"
    );
    assert_eq!(fs::read_to_string(source.join("a.txt")).unwrap(), "A\n");
    for gone in ["sub/b.txt", "sub/link", "sub/deep"] {
        assert!(fs::symlink_metadata(source.join(gone)).is_err(), "{gone}");
    }
    assert_eq!(fs::read_to_string(landed.join("a.txt")).unwrap(), "a\n");
    assert_eq!(
        fs::read_to_string(landed.join("sub/deep/c.txt")).unwrap(),
        "c\n"
    );
    assert_eq!(
        fs::read_link(landed.join("sub/link")).unwrap(),
        Path::new("b.txt")
    );

    let again = base.join("again");
    move_within(&landed, &again, COPY_BOUNDS).unwrap();
    assert!(fs::symlink_metadata(&landed).is_err());
    assert_eq!(fs::read_to_string(again.join("sub/b.txt")).unwrap(), "b\n");
}

/// Sets a file's modify time. Inputs: the file and the time.
fn set_modified(path: &Path, time: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

/// DEC-137 G2: a file system that stamps from a coarse clock gives a same-size rewrite made
/// right after the check the modify time the check saw, so the stamp alone would delete it.
#[test]
fn a_move_keeps_a_source_file_rewritten_with_the_same_size_and_modify_time() {
    let root = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(root.path()).unwrap();
    let source = base.join("source");
    fs::create_dir_all(source.join("sub")).unwrap();
    for name in ["a.txt", "sub/b.txt", "old.txt", "lost.txt"] {
        fs::write(source.join(name), "a\n").unwrap();
    }
    std::os::unix::fs::symlink("b.txt", source.join("sub/link")).unwrap();
    let long_ago = SystemTime::now() - Duration::from_secs(3600);
    set_modified(&source.join("old.txt"), long_ago);
    let landed = base.join("landed");
    let verified = copy_into(
        &source,
        &landed,
        Copying::Exact,
        COPY_BOUNDS,
        &Tally::default(),
    )
    .unwrap();

    let checked = fs::metadata(source.join("a.txt")).unwrap();
    fs::write(source.join("a.txt"), "A\n").unwrap();
    set_modified(&source.join("a.txt"), checked.modified().unwrap());
    let rewritten = fs::metadata(source.join("a.txt")).unwrap();
    assert_eq!(rewritten.modified().unwrap(), checked.modified().unwrap());
    assert_eq!(rewritten.len(), checked.len());
    fs::remove_file(landed.join("old.txt")).unwrap();
    fs::remove_file(landed.join("lost.txt")).unwrap();

    let left = remove_verified(&source, &landed, &verified, COPY_BOUNDS).unwrap_err();
    assert_eq!(left.0, "home_move_source_left");
    assert_eq!(
        fs::read_to_string(source.join("a.txt")).unwrap(),
        "A\n",
        "a file changed under an unchanged stamp is kept"
    );
    assert_eq!(
        fs::read_to_string(source.join("lost.txt")).unwrap(),
        "a\n",
        "a fresh file with no landed copy to compare with is kept"
    );
    for gone in ["sub/b.txt", "sub/link", "old.txt"] {
        assert!(
            fs::symlink_metadata(source.join(gone)).is_err(),
            "{gone}: unchanged entries still go, an old one by its stamp alone"
        );
    }
    assert_eq!(fs::read_to_string(landed.join("a.txt")).unwrap(), "a\n");
}

#[test]
fn a_move_removes_every_hard_link_of_a_file_it_checked() {
    let root = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(root.path()).unwrap();
    let source = base.join("app");
    fs::create_dir_all(source.join("target/debug/deps")).unwrap();
    fs::write(source.join("target/debug/deps/app-0f3e"), "binary\n").unwrap();
    fs::hard_link(
        source.join("target/debug/deps/app-0f3e"),
        source.join("target/debug/app"),
    )
    .unwrap();
    fs::write(source.join("Cargo.toml"), "[package]\n").unwrap();
    let landed = base.join("landed");

    move_by_copy(&source, &landed).unwrap();
    assert!(fs::symlink_metadata(&source).is_err(), "no source is left");
    for copied in ["target/debug/app", "target/debug/deps/app-0f3e"] {
        assert_eq!(
            fs::read_to_string(landed.join(copied)).unwrap(),
            "binary\n",
            "{copied}"
        );
    }
}

/// A source of `folders` folders holding `files` small files each, under `root/source`.
fn small_tree(root: &Path, folders: usize, files: usize) -> PathBuf {
    let source = root.join("source");
    for folder in 0..folders {
        let path = source.join(format!("d{folder}"));
        fs::create_dir_all(&path).unwrap();
        for file in 0..files {
            fs::write(
                path.join(format!("f{file}.txt")),
                format!("{folder} {file}\n"),
            )
            .unwrap();
        }
    }
    source
}

#[test]
fn a_copy_stopped_by_its_tally_ends_cancelled_with_no_staging_and_the_source_whole() {
    let root = tempfile::tempdir().unwrap();
    let source = small_tree(root.path(), 3, 5);
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    for mode in [AddMode::Copy, AddMode::Move] {
        let tally = Tally::default();
        tally.stop();
        let plan = AddPlan::new(source.clone(), home.join("source"), mode, COPY_BOUNDS);
        let placed = match mode {
            AddMode::Copy => plan.run_tallied(&tally),
            AddMode::Move => plan.place_by_copy(&tally).map(drop),
        };
        assert_eq!(placed.unwrap_err().0, "home_add_cancelled", "{mode:?}");
        assert_eq!(
            fs::read_dir(&home).unwrap().count(),
            0,
            "no copy, no staging"
        );
        assert_eq!(
            fs::read_to_string(source.join("d2/f4.txt")).unwrap(),
            "2 4\n"
        );
        assert!(!tally.clearing());
    }
}

#[test]
fn a_copy_counts_each_entry_it_copies_and_each_it_checks() {
    let root = tempfile::tempdir().unwrap();
    let source = small_tree(root.path(), 3, 5);
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let tally = Tally::default();
    let plan = AddPlan::new(
        source.clone(),
        home.join("source"),
        AddMode::Move,
        COPY_BOUNDS,
    );
    let Placed::Copied(old) = plan.place_by_copy(&tally).unwrap() else {
        panic!("a copy leaves the source to clear");
    };
    assert_eq!(tally.copied(), 18, "3 folders and 15 files");
    assert_eq!(tally.checked(), 19, "the folder itself and each entry");
    assert!(!tally.clearing());
    old.clear().unwrap();
    assert_eq!(
        fs::read_to_string(home.join("source/d0/f0.txt")).unwrap(),
        "0 0\n"
    );
}

#[test]
fn a_stop_asked_while_a_move_copies_ends_it_between_entries_and_keeps_the_source() {
    let root = tempfile::tempdir().unwrap();
    let source = small_tree(root.path(), 20, 100);
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let tally = Tally::default();
    let plan = AddPlan::new(
        source.clone(),
        home.join("source"),
        AddMode::Move,
        COPY_BOUNDS,
    );
    let placed = std::thread::scope(|scope| {
        let copying = scope.spawn(|| plan.place_by_copy(&tally).map(drop));
        let deadline = Instant::now() + Duration::from_secs(30);
        while tally.copied() < 10 && Instant::now() < deadline && !copying.is_finished() {
            std::hint::spin_loop();
        }
        tally.stop();
        copying.join().unwrap()
    });
    assert_eq!(placed.unwrap_err().0, "home_add_cancelled");
    assert!(tally.copied() >= 10, "the stop came after the copy began");
    assert!(tally.checked() < 2021, "the check never finished");
    assert_eq!(
        fs::read_dir(&home).unwrap().count(),
        0,
        "no copy, no staging"
    );
    assert_eq!(
        fs::read_to_string(source.join("d19/f99.txt")).unwrap(),
        "19 99\n"
    );
    assert!(!tally.clearing());
}

#[test]
fn a_move_on_one_disk_stopped_before_it_starts_ends_cancelled_and_moves_nothing() {
    let root = tempfile::tempdir().unwrap();
    let source = small_tree(root.path(), 2, 3);
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let tally = Tally::default();
    tally.stop();
    let plan = AddPlan::new(
        source.clone(),
        home.join("source"),
        AddMode::Move,
        COPY_BOUNDS,
    );
    assert_eq!(
        plan.run_tallied(&tally).unwrap_err().0,
        "home_add_cancelled"
    );
    assert_eq!(fs::read_dir(&home).unwrap().count(), 0, "nothing moved in");
    assert_eq!(
        fs::read_to_string(source.join("d1/f2.txt")).unwrap(),
        "1 2\n"
    );
    assert!(!tally.clearing());
}

fn regular_files(folder: &Path) -> Vec<Vec<u8>> {
    let mut files = Vec::new();
    let mut stack = vec![folder.to_path_buf()];
    while let Some(folder) = stack.pop() {
        let Ok(listing) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in listing.flatten() {
            let path = entry.path();
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() => stack.push(path),
                Ok(metadata) if metadata.is_file() => files.push(fs::read(&path).unwrap()),
                _ => {}
            }
        }
    }
    files
}

/// Source entries swapped for links to things outside the source while a copy runs, a file
/// for a link to an owner secret and a folder for a link to a folder holding one, never put
/// the secret's bytes in the copy (RV-DS241-41): each entry is read through the folder the
/// copy opened, never through its path again. A copy that finishes holds the source's own
/// bytes.
#[test]
fn a_copy_never_reads_through_a_source_entry_swapped_for_a_link() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let root = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(root.path()).unwrap();
    let source = base.join("source");
    let outside = base.join("outside");
    fs::create_dir_all(source.join("sub")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(source.join("kept"), "inside\n").unwrap();
    fs::write(source.join("data"), "inside\n").unwrap();
    fs::write(source.join("sub/data"), "inside\n").unwrap();
    fs::write(outside.join("data"), "secret\n").unwrap();
    std::os::unix::fs::symlink(outside.join("data"), source.join("spare")).unwrap();
    std::os::unix::fs::symlink(&outside, source.join("spare-folder")).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let stop = Arc::clone(&stop);
        let source = source.clone();
        std::thread::spawn(move || {
            let mut swaps = 0_u64;
            while !stop.load(Ordering::Relaxed) {
                for (one, other) in [("data", "spare"), ("sub", "spare-folder")] {
                    rustix::fs::renameat_with(
                        rustix::fs::CWD,
                        source.join(one),
                        rustix::fs::CWD,
                        source.join(other),
                        rustix::fs::RenameFlags::EXCHANGE,
                    )
                    .unwrap();
                }
                swaps += 1;
            }
            swaps
        })
    };
    let mut reached = Vec::new();
    let mut finished = 0_u32;
    for attempt in 0..4000 {
        let copy = base.join(format!("copy-{attempt}"));
        let copied = copy_tree(&source, &copy);
        let files = regular_files(&copy);
        if files.iter().any(|bytes| bytes == b"secret\n") {
            reached.push(attempt);
        } else if let Err(refusal) = &copied {
            assert_eq!(refusal.0, "home_copy_entry_swapped", "attempt {attempt}");
        } else {
            finished += 1;
            assert_eq!(
                fs::read(copy.join("kept")).unwrap(),
                b"inside\n",
                "attempt {attempt}"
            );
            assert!(files.iter().all(|bytes| bytes == b"inside\n"));
        }
        let _ = fs::remove_dir_all(&copy);
    }
    stop.store(true, Ordering::Relaxed);
    assert!(swapper.join().unwrap() > 0);
    assert_eq!(
        reached,
        Vec::<i32>::new(),
        "attempts whose copy held the outside file's bytes"
    );
    assert!(finished > 0, "some copies finished");
}

#[test]
fn the_staging_sweep_removes_only_add_staging_folders_and_follows_no_link() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(root.path()).unwrap();
    let home = base.join("home");
    let staging = home.join(".crazyagent-adding-0123456789abcdef");
    fs::create_dir_all(staging.join("deep")).unwrap();
    fs::write(staging.join("deep/a.txt"), "a\n").unwrap();
    fs::set_permissions(staging.join("deep"), fs::Permissions::from_mode(0o500)).unwrap();
    fs::create_dir_all(home.join("site")).unwrap();
    fs::write(home.join(".crazyagent-adding-file"), "not a folder\n").unwrap();
    let outside = base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep.txt"), "keep\n").unwrap();
    std::os::unix::fs::symlink(&outside, home.join(".crazyagent-adding-link")).unwrap();
    assert_eq!(sweep_staging(&home), 1);
    assert!(!staging.exists());
    assert!(home.join("site").is_dir());
    assert!(home.join(".crazyagent-adding-file").is_file());
    assert_eq!(
        fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "keep\n"
    );
    assert_eq!(sweep_staging(&base.join("missing")), 0);
}

#[test]
fn an_add_cut_short_reads_from_disk_whether_it_landed_and_what_it_left() {
    let root = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(root.path()).unwrap();
    let (source, landed) = (base.join("source"), base.join("home/source"));
    fs::create_dir_all(&source).unwrap();
    assert_eq!(add_left(&source, &landed, true), "home_add_interrupted");
    assert_eq!(add_left(&source, &landed, false), "home_add_interrupted");
    fs::create_dir_all(&landed).unwrap();
    assert_eq!(add_left(&source, &landed, true), "home_move_source_left");
    assert_eq!(add_left(&source, &landed, false), "done");
    fs::remove_dir(&source).unwrap();
    assert_eq!(add_left(&source, &landed, true), "done");
}

/// The permission bits of an entry, its links not followed. Input: the path. Output: the bits.
fn bits(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

/// DS-INTAKE: a folder swapped for a link to an owner folder after the walk that makes a
/// tree writable listed it, and before it touched it, is never walked through: the folder
/// the link names and the folders in it keep their modes.
#[test]
fn making_a_tree_writable_never_walks_through_a_folder_swapped_for_a_link() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(root.path()).unwrap();
    let (tree, outside) = (base.join("tree"), base.join("outside"));
    fs::create_dir_all(tree.join("a")).unwrap();
    fs::create_dir_all(outside.join("kept")).unwrap();
    fs::set_permissions(outside.join("kept"), fs::Permissions::from_mode(0o500)).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(&tree, fs::Permissions::from_mode(0o555)).unwrap();
    let (swapped, target) = (tree.join("a"), outside.clone());
    BEFORE_WRITABLE_FOLDER.with(|step| {
        *step.borrow_mut() = Some(Box::new(move || {
            fs::remove_dir(&swapped).unwrap();
            std::os::unix::fs::symlink(&target, &swapped).unwrap();
        }));
    });
    let made = make_writable(&tree);
    let (kept, named) = (bits(&outside.join("kept")), bits(&outside));
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(outside.join("kept"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(kept, 0o500, "a folder behind the link was opened up");
    assert_eq!(named, 0o555, "the folder the link names was opened up");
    assert!(made.is_ok(), "{made:?}");
    assert_eq!(bits(&tree), 0o755, "the tree's root was opened up");
}

/// DS-INTAKE: a refill that puts a moved folder back never writes through a folder of its
/// target swapped for a link after the refill found it there: nothing lands behind the
/// link, the folder the link names keeps its mode, and the refill answers
/// `home_copy_entry_swapped`.
#[test]
fn a_refill_never_writes_through_a_target_folder_swapped_for_a_link() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(root.path()).unwrap();
    let (copy, target, outside) = (base.join("copy"), base.join("source"), base.join("outside"));
    fs::create_dir_all(copy.join("sub")).unwrap();
    fs::write(copy.join("sub/new.txt"), "copied\n").unwrap();
    fs::set_permissions(copy.join("sub"), fs::Permissions::from_mode(0o750)).unwrap();
    fs::create_dir_all(target.join("sub")).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
    let (swapped, named) = (target.join("sub"), outside.clone());
    BEFORE_REFILL_FOLDER.with(|step| {
        *step.borrow_mut() = Some(Box::new(move || {
            fs::remove_dir(&swapped).unwrap();
            std::os::unix::fs::symlink(&named, &swapped).unwrap();
        }));
    });
    let filled = fill_tree(&copy, &target);
    assert!(
        fs::symlink_metadata(outside.join("new.txt")).is_err(),
        "a file landed behind the link"
    );
    assert_eq!(
        bits(&outside),
        0o755,
        "the folder the link names took a mode"
    );
    assert_eq!(filled.unwrap_err().0, "home_copy_entry_swapped");
}
