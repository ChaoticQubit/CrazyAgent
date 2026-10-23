use super::RegisteredNativeAuthHost;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

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
    assert_eq!(digest, format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap())));
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
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
    assert!(registered.verified_sha256().is_err());
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
    assert_eq!(arguments, vec![
        std::ffi::OsString::from("native-auth-custodian"),
        root.path().as_os_str().to_owned(),
        std::ffi::OsString::from("--codex-executable"),
        codex_path.into_os_string(),
        std::ffi::OsString::from("--codex-sha256"),
        codex.verified_sha256().unwrap().into(),
    ]);
    assert_eq!(command.get_envs().count(), 0);
    assert_eq!(command.get_current_dir(), Some(root.path()));
}

#[test]
fn native_auth_custodian_command_without_codex_preserves_default_entry() {
    let root = tempfile::tempdir().unwrap();
    let path = executable(root.path(), "crazyagent");
    let command = super::custodian_command(&path, root.path(), None).unwrap();
    let arguments: Vec<_> = command.get_args().map(|value| value.to_owned()).collect();
    assert_eq!(arguments, vec![
        std::ffi::OsString::from("native-auth-custodian"),
        root.path().as_os_str().to_owned(),
    ]);
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
    assert!(registered.verified_path().is_err());
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
        RegisteredNativeAuthHost::capture(&oversized).err().unwrap().0,
        "native_auth_host_executable_unsafe"
    );
}
