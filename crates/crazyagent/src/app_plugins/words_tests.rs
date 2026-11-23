use kernel_plugin_host::generated::permissions::PERMISSIONS;

use super::words::{WORDS, words};

/// Every permission a plugin may hold has plain words for the owner, and the table holds
/// no row for a permission that no longer exists.
#[test]
fn every_permission_has_plain_words_and_no_row_is_stale() {
    for permission in PERMISSIONS {
        let said = words(permission.name);
        assert!(
            said.is_some_and(|said| !said.trim().is_empty()),
            "{} has no words",
            permission.name
        );
    }
    for (name, _) in WORDS {
        assert!(
            PERMISSIONS.iter().any(|permission| permission.name == name),
            "{name} is no permission"
        );
    }
    assert_eq!(WORDS.len(), PERMISSIONS.len());
}
