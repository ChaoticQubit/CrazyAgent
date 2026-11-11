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

    let left = remove_verified(&source, &verified, COPY_BOUNDS).unwrap_err();
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
