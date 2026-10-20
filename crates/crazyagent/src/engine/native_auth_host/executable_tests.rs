use super::RegisteredNativeAuthHost;
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
    file.set_len(256 * 1024 * 1024 + 1).unwrap();
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
