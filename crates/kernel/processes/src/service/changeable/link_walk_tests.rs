use super::*;

/// The link walk names, in order, the link a path passes, the folder a `..` leaves and the
/// first missing part, and keeps one `lstat` read for every place it found, each the same
/// object a fresh `lstat` reads there: for `<home>/l/b/../c/missing` with `l` a relative
/// link to `a`.
#[test]
fn the_link_walk_names_each_place_and_keeps_each_read() {
    let home = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(home.path()).unwrap();
    std::fs::create_dir_all(home.join("a").join("b")).unwrap();
    std::fs::create_dir_all(home.join("a").join("c")).unwrap();
    std::os::unix::fs::symlink("a", home.join("l")).unwrap();
    let path = home
        .join("l")
        .join("b")
        .join("..")
        .join("c")
        .join("missing");
    let (hops, known) = link_chain_read(&path).unwrap();
    assert_eq!(
        hops,
        [
            home.join("l"),
            home.join("a").join("b"),
            home.join("a").join("c").join("missing"),
        ]
    );
    let mut places: Vec<PathBuf> = home
        .ancestors()
        .filter(|place| place.parent().is_some())
        .map(Path::to_path_buf)
        .collect();
    places.reverse();
    places.extend([
        home.join("l"),
        home.join("a"),
        home.join("a").join("b"),
        home.join("a").join("c"),
    ]);
    let found: Vec<&PathBuf> = known.iter().map(|(place, _)| place).collect();
    assert_eq!(found, places.iter().collect::<Vec<_>>());
    for (place, read) in &known {
        use std::os::unix::fs::MetadataExt;
        let fresh = std::fs::symlink_metadata(place).unwrap();
        assert_eq!(
            (read.dev(), read.ino()),
            (fresh.dev(), fresh.ino()),
            "{}",
            place.display()
        );
    }
}
