use super::*;
use crate::store::reference;

/// An in-memory keyring-core store, so a row needs no Secret Service or Credential Manager.
/// Output: the store.
fn mock() -> keyring_core::Result<Arc<CredentialStore>> {
    let store: Arc<CredentialStore> = keyring_core::mock::Store::new()?;
    Ok(store)
}

/// Builds one entry in the mock store. Inputs: the store, service and account.
/// Output: the entry.
fn mock_entry(
    store: &CredentialStore,
    service: &str,
    account: &str,
) -> keyring_core::Result<Entry> {
    store.build(service, account, None)
}

#[cfg(target_os = "linux")]
const EXECUTABLE: &str = "/opt/crazyagent/bin/crazyagent";
#[cfg(target_os = "linux")]
const SHOWN: &str = "/opt/crazyagent/bin/crazyagent";
#[cfg(target_os = "windows")]
const EXECUTABLE: &str = r"C:\Program Files\CrazyAgent\crazyagent.exe";
#[cfg(target_os = "windows")]
const SHOWN: &str = "C:/Program Files/CrazyAgent/crazyagent.exe";

#[test]
fn the_git_helper_value_is_byte_equal() {
    let namespace = "dev.crazyagent.git";
    let store = OsKeyring::new(namespace, Path::new(EXECUTABLE), mock, mock_entry, None).unwrap();
    let found = reference("dev.crazyagent.git.github", "token").unwrap();
    assert_eq!(
        store
            .git_credential_helper(&found, "x-access-token")
            .unwrap(),
        format!(
            "!'{SHOWN}' git-credential 'dev.crazyagent.git' 'dev.crazyagent.git.github:token' \
             'x-access-token'"
        )
    );
    assert_eq!(GIT_HELPER_COMMAND, "git-credential");
}
