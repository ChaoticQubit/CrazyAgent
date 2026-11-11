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
    let verified = copy_into(&source, &landed, Copying::Exact, COPY_BOUNDS).unwrap();
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
    let verified = copy_into(&source, &landed, Copying::Exact, COPY_BOUNDS).unwrap();

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
