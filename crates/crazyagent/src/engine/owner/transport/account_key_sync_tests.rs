use super::{Root, read_key, replace_key, store_key};
use crate::engine::owner::AccountSecret;
use crate::protocol::CredentialRef;
use crate::storage::accounts::{ACCOUNT_KEYS, fail_next_sync};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const NAME: &str = "anthropic-key-0123456789abcdef0123";

/// A private owner root in a fresh 0700 temp folder, beside which the key folder goes.
/// Output: the temp folder, kept alive by the caller, its real path and the held root.
fn owner_root() -> (tempfile::TempDir, PathBuf, Root) {
    let temp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(temp.path()).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = base.join("root");
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = Root::open(&path).unwrap();
    (temp, base, root)
}

fn secret(text: &str) -> AccountSecret {
    AccountSecret::new(text.as_bytes().to_vec()).unwrap()
}

/// The names in the key folder beside the owner root, sorted. Input: the folder holding the
/// root. Output: the names.
fn names(base: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(base.join(ACCOUNT_KEYS))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    found.sort();
    found
}

#[test]
fn a_store_whose_folder_sync_fails_leaves_no_key_file() {
    let (_temp, base, root) = owner_root();
    let credential = CredentialRef::new(NAME).unwrap();
    fail_next_sync();
    let refused = store_key(&root, &credential, secret("sk-ant-unsynced")).unwrap_err();
    assert_eq!(refused.0, "account_key_store_unavailable");
    assert!(names(&base).is_empty());
    assert!(read_key(&root, &credential).unwrap().is_none());
    store_key(&root, &credential, secret("sk-ant-synced")).unwrap();
    assert_eq!(read_key(&root, &credential).unwrap().unwrap(), b"sk-ant-synced");
}

#[test]
fn a_replace_whose_folder_sync_fails_leaves_the_new_key_and_no_staged_file() {
    let (_temp, base, root) = owner_root();
    let credential = CredentialRef::new(NAME).unwrap();
    store_key(&root, &credential, secret("sk-ant-old")).unwrap();
    fail_next_sync();
    let refused = replace_key(&root, &credential, secret("sk-ant-new")).unwrap_err();
    assert_eq!(refused.0, "account_key_store_unavailable");
    assert_eq!(read_key(&root, &credential).unwrap().unwrap(), b"sk-ant-new");
    assert_eq!(names(&base), [NAME]);
    replace_key(&root, &credential, secret("sk-ant-again")).unwrap();
    assert_eq!(read_key(&root, &credential).unwrap().unwrap(), b"sk-ant-again");
}
