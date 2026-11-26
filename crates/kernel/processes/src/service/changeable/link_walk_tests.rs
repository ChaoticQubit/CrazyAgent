use super::*;

/// The device and file id one walk read names. Input: the read. Output: both, as `lstat`
/// gives them.
#[cfg(target_os = "macos")]
fn identity(read: &Read) -> (u64, u64) {
    (read.device, read.inode)
}

/// The device and file id one walk read names. Input: the read. Output: both, as `lstat`
/// gives them.
#[cfg(not(target_os = "macos"))]
fn identity(read: &Read) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (read.dev(), read.ino())
}

/// The link walk names, in order, the link a path passes, the folder a `..` leaves and the
/// first missing part, and keeps one read for every place it found, each the same object a
/// fresh `lstat` reads there: for `<home>/l/b/../c/missing` with `l` a relative link to
/// `a`.
#[test]
fn the_link_walk_names_each_place_and_keeps_each_read() -> std::io::Result<()> {
    let held = tempfile::tempdir()?;
    let home = std::fs::canonicalize(held.path())?;
    std::fs::create_dir_all(home.join("a").join("b"))?;
    std::fs::create_dir_all(home.join("a").join("c"))?;
    std::os::unix::fs::symlink("a", home.join("l"))?;
    let path = home
        .join("l")
        .join("b")
        .join("..")
        .join("c")
        .join("missing");
    let (hops, known) = link_chain_read(&path)
        .map_err(|at| std::io::Error::other(format!("the walk refused {}", at.display())))?;
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
        let fresh = std::fs::symlink_metadata(place)?;
        assert_eq!(
            identity(read),
            (fresh.dev(), fresh.ino()),
            "{}",
            place.display()
        );
    }
    Ok(())
}
