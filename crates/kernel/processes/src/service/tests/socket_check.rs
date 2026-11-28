use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

/// A private temporary folder named through the `/var` link, made in this user's own
/// temporary folder as the system names it (`getconf DARWIN_USER_TEMP_DIR`), never as
/// `TMPDIR` names it, so a run with another `TMPDIR` reads the same folders. Output: the
/// folder's guard and its path through `/var`, or the error a read of the temporary folder,
/// `getconf` or `/var` gave.
fn home_through_var() -> std::io::Result<(tempfile::TempDir, PathBuf)> {
    let answer = crate::spawn_gate::collected(
        std::process::Command::new("/usr/bin/getconf")
            .arg("DARWIN_USER_TEMP_DIR")
            .env_remove("TMPDIR"),
    )?;
    if !answer.status.success() {
        return Err(std::io::Error::other(format!(
            "getconf DARWIN_USER_TEMP_DIR: {}",
            answer.status
        )));
    }
    let user_temp = String::from_utf8(answer.stdout).map_err(std::io::Error::other)?;
    let home = tempfile::Builder::new().tempdir_in(user_temp.trim_end_matches('\n'))?;
    let real = std::fs::canonicalize(home.path())?;
    let rest = real
        .strip_prefix("/private/var")
        .expect("macOS keeps temporary folders under /private/var");
    assert!(std::fs::symlink_metadata("/var")?.file_type().is_symlink());
    let through_var = Path::new("/var").join(rest);
    Ok((home, through_var))
}

/// Makes a folder with exactly the mode. Inputs: the folder and the mode. Output: the error
/// the create or the mode change gave, if any.
fn folder(path: &Path, mode: u32) -> std::io::Result<()> {
    std::fs::create_dir(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// A folder shut to mode 000 that is opened again to 0700 when dropped, so the home it sits
/// in can be removed whether the row passes or fails.
struct Shut(PathBuf);

impl Drop for Shut {
    /// Puts mode 0700 back on the folder; the change must succeed unless the row is already
    /// failing. Input: none. Output: none.
    fn drop(&mut self) {
        let opened = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        if let Err(error) = opened
            && !std::thread::panicking()
        {
            panic!("the shut folder {} opens again: {error}", self.0.display());
        }
    }
}

/// Shuts a folder to mode 000. Input: the folder. Output: the guard that opens it again, or
/// the error the mode change gave.
fn shut(path: &Path) -> std::io::Result<Shut> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o0))?;
    Ok(Shut(path.to_path_buf()))
}

/// A home whose setup fails after a folder in it is shut is gone once the failure unwinds.
/// Input: none. Output: none; it panics when the setup broke before its planned panic, or
/// when the home is left behind, after removing it itself.
#[test]
fn a_home_with_a_shut_folder_is_gone_after_a_failed_setup() {
    let mut made = None;
    let failed =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> std::io::Result<()> {
            let (_guard, home) = home_through_var()?;
            made = Some(home.clone());
            folder(&home.join("locked"), 0o700)?;
            folder(&home.join("locked/in"), 0o700)?;
            let _shut = shut(&home.join("locked"))?;
            panic!("the setup fails");
        }));
    let home = made.unwrap_or_else(|| panic!("the setup made no home: {failed:?}"));
    let left = home.exists();
    if left {
        let _ =
            std::fs::set_permissions(home.join("locked"), std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&home);
    }
    let payload = match failed {
        Err(payload) => payload,
        Ok(Ok(())) => unreachable!("the setup ends in its planned panic"),
        Ok(Err(error)) => panic!("the setup failed before its planned panic: {error}"),
    };
    assert_eq!(
        payload.downcast_ref::<&str>(),
        Some(&"the setup fails"),
        "the setup failed before its planned panic"
    );
    assert!(!left, "the failed setup left {} behind", home.display());
}

/// The socket check names, for sockets under the `/var` link: a folder others can write
/// above the socket's own; nothing through a link to such a folder, whose target is never a
/// part of the path as written; the socket itself past a file, a link loop or a folder this
/// user cannot search; and nothing past a missing part or a dangling link.
#[test]
fn the_socket_check_reads_each_path_shape_as_it_does_now() -> std::io::Result<()> {
    let (_guard, home) = home_through_var()?;
    folder(&home.join("open"), 0o777)?;
    folder(&home.join("open/shut"), 0o700)?;
    let _listener = crate::spawn_gate::bind_unix_std(home.join("open/shut/sock"))?;
    std::fs::write(home.join("file"), b"x")?;
    symlink(home.join("open"), home.join("to-open"))?;
    symlink(home.join("gone"), home.join("dangling"))?;
    symlink("loop", home.join("loop"))?;
    folder(&home.join("locked"), 0o700)?;
    folder(&home.join("locked/in"), 0o700)?;
    let locked = shut(&home.join("locked"))?;
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
    drop(locked);
    assert_eq!(seen.len(), cases.len());
    for ((socket, wanted), seen) in cases.iter().zip(seen) {
        assert_eq!(seen, wanted.map(|part| home.join(part)), "{socket}");
    }
    Ok(())
}

/// With two folders others can change above a socket, the check names the one nearest the
/// socket.
#[test]
fn the_socket_check_names_the_changeable_folder_nearest_the_socket() -> std::io::Result<()> {
    let (_guard, home) = home_through_var()?;
    folder(&home.join("a"), 0o777)?;
    folder(&home.join("a/b"), 0o777)?;
    folder(&home.join("a/b/c"), 0o700)?;
    let socket = home.join("a/b/c/sock");
    let _listener = crate::spawn_gate::bind_unix_std(&socket)?;
    assert_eq!(changeable_above_socket(&socket), Some(home.join("a/b")));
    Ok(())
}

/// The group and world write bits of the socket and its own folder are set aside, and the
/// same bits on any folder further up still count.
#[test]
fn the_socket_check_sets_aside_only_the_socket_and_its_folder_write_bits() -> std::io::Result<()> {
    let (_guard, home) = home_through_var()?;
    folder(&home.join("m"), 0o777)?;
    let near = home.join("m/sock");
    let _near = crate::spawn_gate::bind_unix_std(&near)?;
    std::fs::set_permissions(&near, std::fs::Permissions::from_mode(0o777))?;
    folder(&home.join("m2"), 0o777)?;
    folder(&home.join("m2/n"), 0o700)?;
    let far = home.join("m2/n/sock");
    let _far = crate::spawn_gate::bind_unix_std(&far)?;
    assert_eq!(changeable_above_socket(&near), None);
    assert_eq!(changeable_above_socket(&far), Some(home.join("m2")));
    Ok(())
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
fn the_held_walk_reads_every_part_as_lstat_does() -> std::io::Result<()> {
    type Seen = (PathBuf, Vec<Part>, Vec<Part>, Vec<Part>);
    let (_guard, home) = home_through_var()?;
    folder(&home.join("open"), 0o777)?;
    folder(&home.join("open/shut"), 0o700)?;
    let _listener = crate::spawn_gate::bind_unix_std(home.join("open/shut/sock"))?;
    std::fs::write(home.join("file"), b"x")?;
    symlink(home.join("open"), home.join("to-open"))?;
    symlink(home.join("gone"), home.join("dangling"))?;
    symlink("loop", home.join("loop"))?;
    folder(&home.join("locked"), 0o700)?;
    folder(&home.join("locked/in"), 0o700)?;
    let locked = shut(&home.join("locked"))?;
    let paths = [
        "open/shut/sock",
        "to-open/shut/sock",
        "file/x/sock",
        "gone/x/sock",
        "dangling/x/sock",
        "loop/x/sock",
        "locked/in/sock",
    ];
    let seen = paths
        .iter()
        .map(|path| -> std::io::Result<Seen> {
            let path = home.join(path);
            let mut walked = changeable::held_parts(&path).ok_or_else(|| {
                std::io::Error::other(format!("no walk from / read {}", path.display()))
            })?;
            walked.reverse();
            let entries: Vec<&Path> = path.ancestors().collect();
            let read = changeable::read_parts(&entries);
            let by_name = changeable::read_parts_known(&entries, &[]);
            Ok((path, shape(walked), shape(read), shape(by_name)))
        })
        .collect::<std::io::Result<Vec<Seen>>>()?;
    drop(locked);
    assert_eq!(seen.len(), paths.len());
    for (path, walked, read, by_name) in seen {
        assert_eq!(walked, read, "{}", path.display());
        assert_eq!(walked, by_name, "{}", path.display());
    }
    Ok(())
}
