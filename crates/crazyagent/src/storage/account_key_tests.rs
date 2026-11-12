use super::{ACCOUNT_KEYS, read_key_file};
use crate::protocol::CredentialRef;
use std::os::unix::fs::PermissionsExt;

/// Makes an engine root with the owner-only key folder and one 0600 key in it.
/// Input: the key text. Output: the root folder, removed when dropped.
fn root_with(key: &[u8]) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join(ACCOUNT_KEYS);
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = folder.join("anthropic-key-0123456789abcdef0123");
    std::fs::write(&file, key).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    root
}

fn reference() -> CredentialRef {
    CredentialRef::new("anthropic-key-0123456789abcdef0123").unwrap()
}

#[test]
fn a_saved_key_is_read_back_and_a_missing_one_is_none() {
    let root = root_with(b"sk-ant-test-key");
    let key = read_key_file(root.path(), &reference()).unwrap().unwrap();
    assert_eq!(key.expose(), "sk-ant-test-key");
    let other = CredentialRef::new("openai-key-0123456789abcdef0123").unwrap();
    assert!(read_key_file(root.path(), &other).unwrap().is_none());
    let empty = tempfile::tempdir().unwrap();
    assert!(read_key_file(empty.path(), &reference()).unwrap().is_none());
}

#[test]
fn a_key_store_another_user_could_change_is_refused() {
    let root = root_with(b"sk-ant-test-key");
    let file = root
        .path()
        .join(ACCOUNT_KEYS)
        .join("anthropic-key-0123456789abcdef0123");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let refused = read_key_file(root.path(), &reference()).unwrap_err();
    assert_eq!(refused.0, "account_key_store_unsafe");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let folder = root.path().join(ACCOUNT_KEYS);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    let refused = read_key_file(root.path(), &reference()).unwrap_err();
    assert_eq!(refused.0, "account_key_store_unsafe");
}

#[test]
fn a_linked_key_and_a_hidden_name_are_refused() {
    let root = root_with(b"sk-ant-test-key");
    let folder = root.path().join(ACCOUNT_KEYS);
    std::os::unix::fs::symlink(
        folder.join("anthropic-key-0123456789abcdef0123"),
        folder.join("linked-key"),
    )
    .unwrap();
    let linked = CredentialRef::new("linked-key").unwrap();
    assert_eq!(
        read_key_file(root.path(), &linked).unwrap_err().0,
        "account_key_store_unavailable"
    );
    let hidden = CredentialRef::new(".hidden").unwrap();
    assert_eq!(
        read_key_file(root.path(), &hidden).unwrap_err().0,
        "account_key_reference_invalid"
    );
}
