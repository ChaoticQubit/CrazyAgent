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
