use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// The moved staging helper names its folder from the id its caller gives, so the engine's
/// own id source stays with the engine: the folder carries the id's first 24 characters,
/// is private, and a second folder from the same id is refused with the store code.
#[cfg(unix)]
#[test]
fn the_moved_helper_takes_its_id_from_the_caller() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("setup");
    let id = "0123456789abcdef0123456789abcdef";
    let made = staging_folder(&parent, "pack", id).unwrap();
    assert_eq!(made, parent.join(".pack-0123456789abcdef01234567"));
    assert!(made.is_dir());
    assert_eq!(
        std::fs::metadata(&made).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        staging_folder(&parent, "pack", id),
        Err(InstallError("setup_store_unavailable"))
    );
}

/// An id too short to name a staging folder is a caller bug, so it stops the helper
/// before anything is made.
#[cfg(unix)]
#[test]
#[should_panic(expected = "id.len() >= 24")]
fn a_staging_id_shorter_than_24_characters_stops_the_helper() {
    let parent = tempfile::tempdir().unwrap();
    let _ = staging_folder(parent.path(), "pack", "0123456789abcdef");
}

#[test]
fn version_reports_from_tmux_uv_and_graphify_read_the_same_way() {
    for (report, version) in [
        ("tmux 3.4\n", "3.4"),
        ("tmux next-3.5\n", "next-3.5"),
        ("uv 0.9.5 (0a1b2c3 2025-10-01)\n", "0.9.5"),
        ("graphify v0.9.9\n", "0.9.9"),
        ("0.9.9", "0.9.9"),
    ] {
        assert_eq!(
            reported_version(report.as_bytes()).unwrap().as_str(),
            version
        );
    }
    for report in ["", "tmux\n", "tmux 3.4;rm\n"] {
        assert_eq!(
            reported_version(report.as_bytes()),
            Err(InstallError("setup_version_unreadable"))
        );
    }
    assert_eq!(
        reported_version(&[0xff, b'3']),
        Err(InstallError("setup_version_unreadable"))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_setup_command_keeps_its_report_fails_plainly_and_stops_at_its_limit() {
    let limit = Duration::from_secs(10);
    let report = run_bounded(Command::new("/bin/echo").args(["tmux", "3.4"]), limit, true).unwrap();
    assert_eq!(report, b"tmux 3.4\n");
    assert_eq!(
        run_bounded(Command::new("/bin/echo").arg("quiet"), limit, false),
        Ok(Vec::new())
    );
    assert_eq!(
        run_bounded(&mut Command::new("/bin/false"), limit, false),
        Err(InstallError("setup_install_failed"))
    );
    assert_eq!(
        run_bounded(
            &mut Command::new("/nonexistent/crazyagent-setup"),
            limit,
            false
        ),
        Err(InstallError("setup_install_failed"))
    );
    let started = Instant::now();
    assert_eq!(
        run_bounded(
            Command::new("/bin/sleep").arg("30"),
            Duration::from_millis(200),
            false
        ),
        Err(InstallError("setup_timeout"))
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// A setup command that exits at once while a process it left behind still
/// holds its standard output is stopped at the limit, not held until that process ends.
#[cfg(unix)]
#[test]
fn a_report_held_open_by_a_background_process_stops_at_the_limit() {
    let started = Instant::now();
    assert_eq!(
        run_bounded(
            Command::new("/bin/sh").args(["-c", "/bin/sleep 5 & echo tmux 3.4"]),
            Duration::from_millis(500),
            true
        ),
        Err(InstallError("setup_timeout"))
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}
