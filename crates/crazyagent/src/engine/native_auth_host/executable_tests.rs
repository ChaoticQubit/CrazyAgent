use super::RegisteredNativeAuthHost;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn executable(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::write(&path, b"registered-host-bytes").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[test]
fn native_auth_registered_executable_retains_real_file_identity() {
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "host");
    let registered = RegisteredNativeAuthHost::capture(&path).unwrap();
    assert_eq!(registered.verified_path().unwrap(), path);
    assert_eq!(registered.verified_path().unwrap(), path);
}

#[test]
fn native_auth_registered_executable_verified_sha256_matches_actual_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "codex");
    let registered = RegisteredNativeAuthHost::capture(&path).unwrap();
    let digest = registered.verified_sha256().unwrap();
    assert_eq!(
        digest,
        format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap()))
    );
    assert_eq!(digest.len(), 64);
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

#[test]
fn native_auth_registered_executable_verified_sha256_rejects_mutation() {
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "codex");
    let registered = RegisteredNativeAuthHost::capture(&path).unwrap();
    std::fs::write(&path, b"changed-executable").unwrap();
    assert!(registered.verified_sha256().is_err());
}

#[test]
fn native_auth_registered_executable_verified_sha256_rejects_replacement() {
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "codex");
    let registered = RegisteredNativeAuthHost::capture(&path).unwrap();
    let replacement = executable(root.path(), "replacement");
    std::fs::rename(replacement, &path).unwrap();
    assert_eq!(
        registered.verified_sha256().unwrap_err().0,
        "native_auth_host_executable_changed"
    );
}

/// Off macOS a request that verifies the claude and codex harnesses, the way a harness catalog
/// or a launch does, hashes each one again only until one matching hash starts
/// [`super::SETTLE_AFTER`] past its seen identity and past its change time; after that a
/// request hashes nothing. The row waits that time for real: the change time is the file's
/// own, which no test can move back.
#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_a_request_hashes_each_harness_only_until_it_settles() {
    use std::sync::atomic::Ordering;
    let root = tempfile::tempdir().unwrap();
    let harnesses = ["claude", "codex"]
        .map(|name| RegisteredNativeAuthHost::capture(&executable(root.path(), name)).unwrap());
    let request = |harnesses: &[RegisteredNativeAuthHost; 2]| {
        for registered in harnesses {
            registered.verified_path().unwrap();
            let digest = registered.verified_sha256().unwrap();
            assert_eq!(
                digest,
                format!(
                    "{:x}",
                    Sha256::digest(std::fs::read(&registered.path).unwrap())
                )
            );
        }
        harnesses
            .iter()
            .map(|registered| registered.rehashes.load(Ordering::Relaxed))
            .sum::<usize>()
    };
    let fresh = request(&harnesses);
    if harnesses
        .iter()
        .all(|registered| registered.seen.elapsed() < super::SETTLE_AFTER)
    {
        assert_eq!(fresh, 4);
    }
    let unsettled = harnesses
        .iter()
        .filter(|registered| !registered.settled.load(Ordering::Acquire))
        .count();
    std::thread::sleep(super::SETTLE_AFTER);
    assert!(
        harnesses
            .iter()
            .all(|registered| registered.seen.elapsed() >= super::SETTLE_AFTER)
    );
    let settling = request(&harnesses);
    assert_eq!(settling, fresh + unsettled);
    assert!(
        harnesses
            .iter()
            .all(|registered| registered.settled.load(Ordering::Acquire))
    );
    assert_eq!(request(&harnesses), settling);
}

/// A matching hash settles a harness only when it starts [`super::SETTLE_AFTER`] past the
/// first look by the monotonic clock and past the recorded change time by the wall clock, so
/// a change time ahead of the clock, as after the clock steps back, keeps the harness hashing.
#[test]
fn a_change_time_ahead_of_the_wall_clock_never_settles_a_harness() {
    let started = std::time::Instant::now();
    let seen = started.checked_sub(super::SETTLE_AFTER).unwrap();
    let wall = std::time::SystemTime::now();
    let since = wall.duration_since(std::time::UNIX_EPOCH).unwrap();
    let changed = |offset: i64| {
        (
            i64::try_from(since.as_secs()).unwrap() + offset,
            i64::from(since.subsec_nanos()),
        )
    };
    assert!(super::settles(seen, started, wall, changed(-3)));
    assert!(!super::settles(seen, started, wall, changed(60)));
    assert!(!super::settles(seen, started, wall, changed(-2)));
    assert!(!super::settles(
        seen + std::time::Duration::from_millis(1),
        started,
        wall,
        changed(-60)
    ));
}

/// Off macOS a same-size write before the identity settles is refused even when the hash
/// that sees it starts past the settle time, and the harness never settles.
#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_a_same_size_write_never_settles_the_harness() {
    use std::sync::atomic::Ordering;
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "codex");
    let mut registered = RegisteredNativeAuthHost::capture(&path).unwrap();
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all(b"REGISTERED-HOST-BYTES").unwrap();
    drop(file);
    registered.seen = registered.seen.checked_sub(super::SETTLE_AFTER).unwrap();
    assert_eq!(
        registered.verified_path().unwrap_err().0,
        "native_auth_host_executable_changed"
    );
    assert!(!registered.settled.load(Ordering::Acquire));
}

#[test]
fn native_auth_custodian_command_keeps_self_and_codex_pins_distinct() {
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "crazyagent");
    let codex_path = executable(root.path(), "codex");
    let codex = RegisteredNativeAuthHost::capture(&codex_path).unwrap();
    let command = super::custodian_command(&path, root.path(), Some(&codex)).unwrap();
    assert_eq!(command.get_program(), path.as_os_str());
    let arguments: Vec<_> = command.get_args().map(|value| value.to_owned()).collect();
    assert_eq!(
        arguments,
        vec![
            std::ffi::OsString::from("native-auth-custodian"),
            root.path().as_os_str().to_owned(),
            std::ffi::OsString::from("--codex-executable"),
            codex_path.into_os_string(),
            std::ffi::OsString::from("--codex-sha256"),
            codex.verified_sha256().unwrap().into(),
        ]
    );
    assert_eq!(command.get_envs().count(), 0);
    assert_eq!(command.get_current_dir(), Some(root.path()));
}

#[test]
fn native_auth_custodian_command_without_codex_preserves_default_entry() {
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "crazyagent");
    let command = super::custodian_command(&path, root.path(), None).unwrap();
    let arguments: Vec<_> = command.get_args().map(|value| value.to_owned()).collect();
    assert_eq!(
        arguments,
        vec![
            std::ffi::OsString::from("native-auth-custodian"),
            root.path().as_os_str().to_owned(),
        ]
    );
}

#[test]
fn native_auth_custodian_command_rejects_changed_codex_pin() {
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "crazyagent");
    let codex_path = executable(root.path(), "codex");
    let codex = RegisteredNativeAuthHost::capture(&codex_path).unwrap();
    std::fs::write(codex_path, b"changed").unwrap();
    assert!(super::custodian_command(&path, root.path(), Some(&codex)).is_err());
}

#[test]
fn native_auth_registered_executable_rejects_relative_path() {
    assert!(RegisteredNativeAuthHost::capture(Path::new("host")).is_err());
}

#[test]
fn native_auth_registered_executable_rejects_symlink() {
    let root = tempfile::tempdir().unwrap();
    let real = executable(root.path(), "real");
    let link = root.path().join("link");
    symlink(real, &link).unwrap();
    assert!(RegisteredNativeAuthHost::capture(&link).is_err());
}

#[test]
fn native_auth_registered_entry_pins_the_real_file_behind_its_link() {
    let root = tempfile::tempdir().unwrap();
    let real = executable(root.path(), "claude-2.1.322");
    let link = root.path().join("claude");
    symlink(&real, &link).unwrap();
    let registered = RegisteredNativeAuthHost::capture_entry(&link).unwrap();
    assert_eq!(
        registered.verified_path().unwrap(),
        std::fs::canonicalize(&real).unwrap()
    );
    std::fs::remove_file(&link).unwrap();
    assert_eq!(
        RegisteredNativeAuthHost::capture_entry(&link)
            .map(|_| ())
            .unwrap_err()
            .0,
        "native_auth_host_unavailable"
    );
}

#[test]
fn native_auth_registered_entry_refuses_a_rewrite_of_its_pinned_file_in_place() {
    let root = tempfile::tempdir().unwrap();
    let real = executable(root.path(), "claude-2.1.322");
    let link = root.path().join("claude");
    symlink(&real, &link).unwrap();
    let registered = Arc::new(RegisteredNativeAuthHost::capture_entry(&link).unwrap());
    let mut file = OpenOptions::new().write(true).open(&real).unwrap();
    file.write_all(b"tampered-host-bytes").unwrap();
    drop(file);
    assert_eq!(
        registered.current().map(|_| ()).unwrap_err().0,
        "native_auth_host_executable_changed"
    );
}

#[test]
fn native_auth_registered_entry_keeps_a_pinned_file_whose_metadata_alone_changed() {
    let root = tempfile::tempdir().unwrap();
    let real = executable(root.path(), "claude-2.1.322");
    let link = root.path().join("claude");
    symlink(&real, &link).unwrap();
    let registered = Arc::new(RegisteredNativeAuthHost::capture_entry(&link).unwrap());
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert!(registered.verified_path().is_err());
    let current = registered.current().unwrap();
    assert_eq!(
        current.verified_path().unwrap(),
        std::fs::canonicalize(&real).unwrap()
    );
    assert_eq!(current.digest, registered.digest);
    assert!(Arc::ptr_eq(&current, &current.current().unwrap()));
}

#[test]
fn native_auth_registered_entry_refuses_a_pinned_file_whose_metadata_turned_unsafe() {
    let root = tempfile::tempdir().unwrap();
    let real = executable(root.path(), "claude-2.1.322");
    let link = root.path().join("claude");
    symlink(&real, &link).unwrap();
    let registered = Arc::new(RegisteredNativeAuthHost::capture_entry(&link).unwrap());
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o720)).unwrap();
    assert_eq!(
        registered.current().map(|_| ()).unwrap_err().0,
        "native_auth_host_executable_unsafe"
    );
}

#[test]
fn native_auth_registered_entry_moves_to_a_new_install_while_the_old_one_is_kept() {
    let root = tempfile::tempdir().unwrap();
    let old = executable(root.path(), "claude-2.1.309");
    let new = executable(root.path(), "claude-2.1.322");
    let link = root.path().join("claude");
    symlink(&old, &link).unwrap();
    let registered = Arc::new(RegisteredNativeAuthHost::capture_entry(&link).unwrap());
    assert!(Arc::ptr_eq(&registered, &registered.current().unwrap()));
    std::fs::remove_file(&link).unwrap();
    symlink(&new, &link).unwrap();
    let current = registered.current().unwrap();
    assert_eq!(
        current.verified_path().unwrap(),
        std::fs::canonicalize(&new).unwrap()
    );
    assert_eq!(
        registered.verified_path().unwrap(),
        std::fs::canonicalize(&old).unwrap()
    );
}

#[test]
fn native_auth_registered_executable_rejects_directory() {
    let root = tempfile::tempdir().unwrap();
    assert!(RegisteredNativeAuthHost::capture(root.path()).is_err());
}

#[test]
fn native_auth_registered_executable_rejects_unsafe_modes() {
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "host");
    for mode in [0o600, 0o720, 0o702] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(RegisteredNativeAuthHost::capture(&path).is_err());
    }
}

#[test]
fn native_auth_registered_executable_rejects_replaced_path() {
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "host");
    let registered = RegisteredNativeAuthHost::capture(&path).unwrap();
    let replacement = executable(root.path(), "replacement");
    std::fs::rename(replacement, &path).unwrap();
    assert!(registered.verified_path().is_err());
}

#[test]
fn native_auth_registered_executable_rejects_retained_file_mutation() {
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "host");
    let registered = RegisteredNativeAuthHost::capture(&path).unwrap();
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"x").unwrap();
    file.sync_all().unwrap();
    assert_eq!(
        registered.verified_path().unwrap_err().0,
        "native_auth_host_executable_changed"
    );
}

#[test]
fn native_auth_registered_executable_rejects_mode_change() {
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "host");
    let registered = RegisteredNativeAuthHost::capture(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o720)).unwrap();
    assert!(registered.verified_path().is_err());
}

#[test]
fn native_auth_registered_executable_rejects_empty_and_oversize_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("host");
    let file = File::create(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(RegisteredNativeAuthHost::capture(&path).is_err());
    file.set_len(super::EXECUTABLE_BYTES_MAX + 1).unwrap();
    assert!(RegisteredNativeAuthHost::capture(&path).is_err());
}

#[test]
fn native_auth_registered_executable_accepts_claude_code_2_1_309_size() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("claude");
    let file = File::create(&path).unwrap();
    file.set_len(275_923_296).unwrap();
    drop(file);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let registered = RegisteredNativeAuthHost::capture(&path).unwrap();
    assert_eq!(registered.verified_path().unwrap(), path);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 275_923_296);
}

/// Sixteen catalog-sized rounds of checks on a harness of Claude Code's size take under a
/// second once the harness is settled. macOS never hashes again after capture; elsewhere the
/// row first waits [`super::SETTLE_AFTER`] and lets one check settle the harness, since every
/// check before that hashes the whole file by design.
#[test]
fn native_auth_registered_executable_checks_at_harness_size_fit_the_owner_deadline() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("claude");
    let file = File::create(&path).unwrap();
    file.set_len(275_923_296).unwrap();
    drop(file);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let registered = RegisteredNativeAuthHost::capture(&path).unwrap();
    let digest = registered.verified_sha256().unwrap();
    #[cfg(not(target_os = "macos"))]
    {
        std::thread::sleep(super::SETTLE_AFTER);
        assert_eq!(registered.verified_path().unwrap(), path);
        assert!(
            registered
                .settled
                .load(std::sync::atomic::Ordering::Acquire),
            "a harness seen and unchanged for the settle time settles at its next check"
        );
    }
    let started = std::time::Instant::now();
    for _ in 0..16 {
        assert_eq!(registered.verified_path().unwrap(), path);
        assert_eq!(registered.verified_sha256().unwrap(), digest);
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "16 catalog-sized rounds of executable checks took {elapsed:?}"
    );
}

#[test]
#[should_panic(expected = "assertion failed")]
fn native_auth_registered_executable_internal_hash_length_is_bounded() {
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "host");
    let file = File::open(path).unwrap();
    let _ = RegisteredNativeAuthHost::hash(&file, 0);
}

#[test]
fn native_auth_registers_the_installed_claude_code_executable() {
    let entries = crate::harnesses::catalog(&crate::harnesses::Search::from_environment());
    let claude = entries
        .into_iter()
        .find(|entry| entry.id == "claude-code")
        .and_then(|entry| entry.executable)
        .expect("installed Claude Code");
    let claude = std::fs::canonicalize(claude).unwrap();
    let registered = RegisteredNativeAuthHost::capture(&claude).unwrap();
    assert_eq!(registered.verified_path().unwrap(), claude);
    assert_eq!(registered.verified_sha256().unwrap().len(), 64);
}

#[test]
fn native_auth_executable_bound_admits_large_harness_builds_and_refuses_beyond_it() {
    let root = tempfile::tempdir().unwrap();
    let large = executable(root.path(), "large");
    OpenOptions::new()
        .write(true)
        .open(&large)
        .unwrap()
        .set_len(300 * 1024 * 1024)
        .unwrap();
    assert!(RegisteredNativeAuthHost::capture(&large).is_ok());
    let oversized = executable(root.path(), "oversized");
    OpenOptions::new()
        .write(true)
        .open(&oversized)
        .unwrap()
        .set_len(super::EXECUTABLE_BYTES_MAX + 1)
        .unwrap();
    assert_eq!(
        RegisteredNativeAuthHost::capture(&oversized)
            .err()
            .unwrap()
            .0,
        "native_auth_host_executable_unsafe"
    );
}
