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
    .unwrap()
    .verified;
    fs::write(source.join("sub/synced.txt"), "from another Mac\n").unwrap();
    fs::write(source.join("a.txt"), "A\n").unwrap();

    let left = remove_verified(&source, &landed, &verified, COPY_BOUNDS, &[]).unwrap_err();
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
    .unwrap()
    .verified;

    let checked = fs::metadata(source.join("a.txt")).unwrap();
    fs::write(source.join("a.txt"), "A\n").unwrap();
    set_modified(&source.join("a.txt"), checked.modified().unwrap());
    let rewritten = fs::metadata(source.join("a.txt")).unwrap();
    assert_eq!(rewritten.modified().unwrap(), checked.modified().unwrap());
    assert_eq!(rewritten.len(), checked.len());
    fs::remove_file(landed.join("old.txt")).unwrap();
    fs::remove_file(landed.join("lost.txt")).unwrap();

    let left = remove_verified(&source, &landed, &verified, COPY_BOUNDS, &[]).unwrap_err();
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

/// V243-INODE-STAMPS: a source file rewritten in place after the check, with its old size
/// and a modify time older than [`STAMP_SETTLE`] put back, keeps the device, inode, size and
/// modify time the check saw, and only its change time tells it apart: the move keeps it.
/// Every link of an old hard-linked file still goes, its bytes equal to the landed copy,
/// though deleting the first link moves the change time of the others.
#[test]
fn a_move_keeps_an_old_source_file_rewritten_in_place_with_its_size_and_modify_time() {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(root.path()).unwrap();
    let source = base.join("source");
    fs::create_dir_all(source.join("sub")).unwrap();
    fs::write(source.join("a.txt"), "a\n").unwrap();
    fs::write(source.join("sub/linked"), "binary\n").unwrap();
    fs::hard_link(source.join("sub/linked"), source.join("linked")).unwrap();
    let long_ago = SystemTime::now() - Duration::from_secs(3600);
    for name in ["a.txt", "sub/linked"] {
        set_modified(&source.join(name), long_ago);
    }
    let landed = base.join("landed");
    let verified = copy_into(
        &source,
        &landed,
        Copying::Exact,
        COPY_BOUNDS,
        &Tally::default(),
    )
    .unwrap()
    .verified;

    let file = source.join("a.txt");
    let checked = fs::symlink_metadata(&file).unwrap();
    let mut rewritten = checked.clone();
    for _ in 0..1000 {
        fs::write(&file, "A\n").unwrap();
        set_modified(&file, long_ago);
        rewritten = fs::symlink_metadata(&file).unwrap();
        if (rewritten.ctime(), rewritten.ctime_nsec()) != (checked.ctime(), checked.ctime_nsec()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        (
            rewritten.dev(),
            rewritten.ino(),
            rewritten.len(),
            rewritten.modified().unwrap()
        ),
        (
            checked.dev(),
            checked.ino(),
            checked.len(),
            checked.modified().unwrap()
        ),
        "the rewrite kept the device, inode, size and modify time"
    );
    assert_ne!(
        (rewritten.ctime(), rewritten.ctime_nsec()),
        (checked.ctime(), checked.ctime_nsec()),
        "the rewrite moved the change time"
    );

    let left = remove_verified(&source, &landed, &verified, COPY_BOUNDS, &[]);
    assert_eq!(
        fs::read_to_string(&file).ok().as_deref(),
        Some("A\n"),
        "a file rewritten in place under its old size and modify time is kept"
    );
    for gone in ["sub/linked", "linked"] {
        assert!(
            fs::symlink_metadata(source.join(gone)).is_err(),
            "{gone}: every link of an old hard-linked file goes by its bytes"
        );
    }
    assert_eq!(left.unwrap_err().0, "home_move_source_left");
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

/// An iCloud project for the DEC-198 rows: `node_modules` holds a file still only in iCloud
/// (marked for [`super::super::icloud::fake`]), `.venv` is fully on this Mac, and the source
/// files have landed. Output: the folder holding it and the project's path.
fn icloud_project() -> (tempfile::TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let project = fs::canonicalize(root.path()).unwrap().join("shop");
    for dir in ["src", "node_modules/left-pad", ".venv/lib"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    for (file, text) in [
        ("src/main.rs", "fn main() {}\n"),
        ("node_modules/left-pad/index.js", "module.exports = 1;\n"),
        ("node_modules/left-pad/package.json", "{}\n"),
        (".venv/lib/site.py", "x = 1\n"),
    ] {
        fs::write(project.join(file), text).unwrap();
    }
    let mark = fs::Permissions::from_mode(super::super::icloud::fake::MODE);
    fs::set_permissions(project.join("node_modules/left-pad/index.js"), mark).unwrap();
    fs::set_permissions(
        project.join("node_modules"),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    (root, project)
}

fn skipping() -> CopyBounds {
    CopyBounds {
        skip_rebuildable: true,
        ..COPY_BOUNDS
    }
}

#[test]
fn a_move_out_of_icloud_leaves_rebuildable_folders_still_in_icloud_at_the_source_and_ends_ok() {
    super::super::icloud::fake::placeholders();
    let (_root, source) = icloud_project();
    let home = tempfile::tempdir().unwrap();
    let destination = fs::canonicalize(home.path()).unwrap().join("shop");
    let plan = AddPlan::new(
        source.clone(),
        destination.clone(),
        AddMode::Move,
        skipping(),
    );
    let Placed::Copied(old) = plan.place_by_copy(&Tally::default()).unwrap() else {
        panic!("a move by copy leaves its source to clear");
    };
    old.clear().unwrap();
    assert_eq!(
        fs::read_to_string(destination.join("src/main.rs")).unwrap(),
        "fn main() {}\n"
    );
    assert!(destination.join(".venv/lib/site.py").is_file());
    assert!(fs::symlink_metadata(destination.join("node_modules")).is_err());
    let names: Vec<_> = fs::read_dir(&source)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["node_modules"], "only the skipped folder stays");
    assert_eq!(bits(&source.join("node_modules")), 0o750);
    assert_eq!(
        bits(&source.join("node_modules/left-pad/index.js")),
        super::super::icloud::fake::MODE
    );
    assert_eq!(
        fs::read_to_string(source.join("node_modules/left-pad/package.json")).unwrap(),
        "{}\n"
    );
}

#[test]
fn a_copy_out_of_icloud_leaves_the_same_folders_out_and_download_everything_still_refuses() {
    super::super::icloud::fake::placeholders();
    let (_root, source) = icloud_project();
    let home = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(home.path()).unwrap();
    let copied = base.join("shop");
    AddPlan::new(source.clone(), copied.clone(), AddMode::Copy, skipping())
        .run()
        .unwrap();
    assert!(copied.join("src/main.rs").is_file());
    assert!(copied.join(".venv/lib/site.py").is_file());
    assert!(fs::symlink_metadata(copied.join("node_modules")).is_err());
    assert!(source.join("node_modules/left-pad/package.json").is_file());
    assert!(source.join("src/main.rs").is_file());
    let everything = AddPlan::new(source.clone(), base.join("all"), AddMode::Copy, COPY_BOUNDS);
    assert_eq!(
        everything.run().unwrap_err().0,
        "home_icloud_not_downloaded"
    );
    assert!(fs::symlink_metadata(base.join("all")).is_err());
}

#[test]
fn folder_size_names_rebuildable_folders_still_in_icloud_with_their_file_counts() {
    super::super::icloud::fake::placeholders();
    let (_root, source) = icloud_project();
    fs::write(source.join("src/data.bin"), "only in iCloud").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let mark = fs::Permissions::from_mode(super::super::icloud::fake::MODE);
        fs::set_permissions(source.join("src/data.bin"), mark).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let measured = folder_size(source.to_str().unwrap(), deadline).unwrap();
    assert_eq!(
        measured,
        Measured {
            bytes: ("fn main() {}\n".len() + "x = 1\n".len() + "only in iCloud".len()) as u64,
            placeholders: 1,
            skipped: vec![super::super::icloud::Skipped {
                path: "node_modules".into(),
                files: Some(2),
            }],
        }
    );
}

#[test]
fn download_everything_goes_with_either_mode_and_turns_the_skip_off() {
    use super::super::{AddOverride, add_bounds, check_overrides};
    assert!(add_bounds(&[]).skip_rebuildable);
    assert!(!add_bounds(&[AddOverride::DownloadEverything]).skip_rebuildable);
    let forced = add_bounds(&[AddOverride::TooLarge, AddOverride::DownloadEverything]);
    assert_eq!(
        (forced.entries, forced.depth, forced.skip_rebuildable),
        (FORCED_COPY_BOUNDS.entries, FORCED_COPY_BOUNDS.depth, false)
    );
    for mode in [AddMode::Move, AddMode::Copy] {
        assert!(check_overrides(mode, &[AddOverride::DownloadEverything]).is_ok());
    }
    assert_eq!(
        serde_json::to_string(&AddOverride::DownloadEverything).unwrap(),
        "\"download_everything\""
    );
}
