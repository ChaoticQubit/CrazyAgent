use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;

/// A private temporary folder named through the `/var` link, as macOS names its temporary
/// folders. Output: the folder's guard and its path through `/var`.
fn home_through_var() -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(home.path()).unwrap();
    let rest = real
        .strip_prefix("/private/var")
        .expect("macOS keeps temporary folders under /private/var");
    assert!(
        std::fs::symlink_metadata("/var")
            .unwrap()
            .file_type()
            .is_symlink()
    );
    (home, Path::new("/var").join(rest))
}

/// Makes a folder with exactly the mode. Inputs: the folder and the mode.
fn folder(path: &Path, mode: u32) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// The socket check names, for sockets under the `/var` link: a folder others can write
/// above the socket's own; nothing through a link to such a folder, whose target is never a
/// part of the path as written; the socket itself past a file, a link loop or a folder this
/// user cannot search; and nothing past a missing part or a dangling link.
#[test]
fn the_socket_check_reads_each_path_shape_as_it_does_now() {
    let (_guard, home) = home_through_var();
    folder(&home.join("open"), 0o777);
    folder(&home.join("open/shut"), 0o700);
    let _listener = UnixListener::bind(home.join("open/shut/sock")).unwrap();
    std::fs::write(home.join("file"), b"x").unwrap();
    symlink(home.join("open"), home.join("to-open")).unwrap();
    symlink(home.join("gone"), home.join("dangling")).unwrap();
    symlink("loop", home.join("loop")).unwrap();
    folder(&home.join("locked"), 0o700);
    folder(&home.join("locked/in"), 0o700);
    std::fs::set_permissions(home.join("locked"), std::fs::Permissions::from_mode(0)).unwrap();
    let cases = [
        ("open/shut/sock", Some("open")),
        ("to-open/shut/sock", None),
        ("file/x/sock", Some("file/x/sock")),
        ("gone/x/sock", None),
        ("dangling/x/sock", None),
        ("loop/x/sock", Some("loop/x/sock")),
        ("locked/in/sock", Some("locked/in/sock")),
    ];
    let seen: Vec<Option<PathBuf>> = cases
        .iter()
        .map(|(socket, _)| changeable_above_socket(&home.join(socket)))
        .collect();
    std::fs::set_permissions(home.join("locked"), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(seen.len(), cases.len());
    for ((socket, wanted), seen) in cases.iter().zip(seen) {
        assert_eq!(seen, wanted.map(|part| home.join(part)), "{socket}");
    }
}

/// With two folders others can change above a socket, the check names the one nearest the
/// socket.
#[test]
fn the_socket_check_names_the_changeable_folder_nearest_the_socket() {
    let (_guard, home) = home_through_var();
    folder(&home.join("a"), 0o777);
    folder(&home.join("a/b"), 0o777);
    folder(&home.join("a/b/c"), 0o700);
    let socket = home.join("a/b/c/sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    assert_eq!(changeable_above_socket(&socket), Some(home.join("a/b")));
}

/// The group and world write bits of the socket and its own folder are set aside, and the
/// same bits on any folder further up still count.
#[test]
fn the_socket_check_sets_aside_only_the_socket_and_its_folder_write_bits() {
    let (_guard, home) = home_through_var();
    folder(&home.join("m"), 0o777);
    let near = home.join("m/sock");
    let _near = UnixListener::bind(&near).unwrap();
    std::fs::set_permissions(&near, std::fs::Permissions::from_mode(0o777)).unwrap();
    folder(&home.join("m2"), 0o777);
    folder(&home.join("m2/n"), 0o700);
    let far = home.join("m2/n/sock");
    let _far = UnixListener::bind(&far).unwrap();
    assert_eq!(changeable_above_socket(&near), None);
    assert_eq!(changeable_above_socket(&far), Some(home.join("m2")));
}

/// The parts with only their permission bits kept in each mode, as the ownership rule reads
/// them; `lstat` also carries the type bits. Input: the parts. Output: the parts.
fn shape(parts: Vec<Part>) -> Vec<Part> {
    parts
        .into_iter()
        .map(|part| match part {
            Part::Found {
                owner,
                group,
                mode,
                folder,
                link,
            } => Part::Found {
                owner,
                group,
                mode: mode & 0o7777,
                folder,
                link,
            },
            other => other,
        })
        .collect()
}

/// The held walk reads every part as `lstat` reads it, part for part: through the `/var`
/// link and a link to a folder, past a file, a missing part, a dangling link, a link loop
/// and a folder this user cannot search.
#[test]
fn the_held_walk_reads_every_part_as_lstat_does() {
    let (_guard, home) = home_through_var();
    folder(&home.join("open"), 0o777);
    folder(&home.join("open/shut"), 0o700);
    let _listener = UnixListener::bind(home.join("open/shut/sock")).unwrap();
    std::fs::write(home.join("file"), b"x").unwrap();
    symlink(home.join("open"), home.join("to-open")).unwrap();
    symlink(home.join("gone"), home.join("dangling")).unwrap();
    symlink("loop", home.join("loop")).unwrap();
    folder(&home.join("locked"), 0o700);
    folder(&home.join("locked/in"), 0o700);
    std::fs::set_permissions(home.join("locked"), std::fs::Permissions::from_mode(0)).unwrap();
    let paths = [
        "open/shut/sock",
        "to-open/shut/sock",
        "file/x/sock",
        "gone/x/sock",
        "dangling/x/sock",
        "loop/x/sock",
        "locked/in/sock",
    ];
    let seen: Vec<(PathBuf, Vec<Part>, Vec<Part>)> = paths
        .iter()
        .map(|path| {
            let path = home.join(path);
            let mut walked = changeable::held_parts(&path).expect("a plain chain of names");
            walked.reverse();
            let entries: Vec<&Path> = path.ancestors().collect();
            let read = changeable::read_parts(&entries);
            (path, shape(walked), shape(read))
        })
        .collect();
    std::fs::set_permissions(home.join("locked"), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(seen.len(), paths.len());
    for (path, walked, read) in seen {
        assert_eq!(walked, read, "{}", path.display());
    }
}
