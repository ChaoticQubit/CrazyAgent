#![cfg(windows)]
use super::*;

/// Makes a junction at `link` that leads to the folder `target`, with `mklink /J`. Inputs:
/// the junction and its target, both absolute with backslashes.
fn junction(link: &Path, target: &Path) {
    let made = crate::spawn_gate::collected(
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target),
    )
    .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
}

/// A junction in a private folder that leads through a junction in a folder Users may
/// write, back to a private folder, is changeable at that shared folder, as whoever writes
/// it can swap the middle junction; the private end alone is not. Fails naming the variable
/// when USERPROFILE is not set.
#[test]
fn a_junction_chain_through_a_shared_folder_is_changeable_though_both_ends_are_private()
-> Result<(), String> {
    let users = "*S-1-5-32-545";
    let profile = std::env::var_os("USERPROFILE").ok_or("USERPROFILE is not set")?;
    let directory = tempfile::tempdir_in(profile).unwrap();
    let private = directory.path().join("private");
    let real = private.join("real");
    std::fs::create_dir_all(&real).unwrap();
    let shared = directory.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    let middle = shared.join("l2");
    junction(&middle, &real);
    let first = private.join("l1");
    junction(&first, &middle);
    icacls(&shared, &["/grant", &format!("{users}:(W)")]);
    let by_others = changeable_by_others(&first);
    let past_sticky = changeable_past_sticky(&first);
    let real_alone = changeable_by_others(&real);
    icacls(&shared, &["/remove:g", users]);
    assert_eq!(real_alone, None);
    assert_eq!(by_others, Some(shared.clone()));
    assert_eq!(past_sticky, Some(shared));
    Ok(())
}

/// A link's target walks from its drive's root, from the current drive's root for a target
/// with a root and no drive, or from the link's folder for a relative one; a share, volume,
/// device or drive-relative target names no start, so the link counts as changeable.
#[test]
fn a_link_target_walks_from_a_drive_root_or_counts_as_changeable() {
    let at = Path::new(r"D:\from\here");
    for (target, start) in [
        (r"C:\x", Some(r"C:\")),
        (r"\\?\C:\x", Some(r"C:\")),
        (r"\x", Some(r"D:\")),
        (r"x\y", Some(r"D:\from\here")),
        (r"..\y", Some(r"D:\from\here")),
        (r"C:x", None),
        (r"\\server\share\x", None),
        (r"\\?\UNC\server\share\x", None),
        (r"\\?\Volume{0b1c2d3e-0000-0000-0000-000000000000}\x", None),
        (r"\\.\pipe\x", None),
    ] {
        assert_eq!(
            changeable::link_start(at, Path::new(target)),
            start.map(PathBuf::from),
            "{target}"
        );
    }
}
