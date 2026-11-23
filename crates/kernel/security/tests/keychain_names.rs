#![cfg(target_os = "macos")]

use kernel_security::keychain::{
    ASIDE_KEYCHAIN, KEYCHAIN_SUFFIX, LOGIN_KEYCHAIN, PROFILE_KEYCHAIN, SECURITY,
    agent_keychain_name,
};

#[test]
fn keychain_names_are_byte_equal() {
    assert_eq!(SECURITY, "/usr/bin/security");
    assert_eq!(LOGIN_KEYCHAIN, "login.keychain-db");
    assert_eq!(PROFILE_KEYCHAIN, "profile.keychain-db");
    assert_eq!(ASIDE_KEYCHAIN, "profile-locked.keychain-db");
    assert_eq!(KEYCHAIN_SUFFIX, ".keychain-db");
    assert!(agent_keychain_name("agent-0123456789abcdef.keychain-db"));
    for name in [
        LOGIN_KEYCHAIN,
        PROFILE_KEYCHAIN,
        ASIDE_KEYCHAIN,
        "agent-0123456789ABCDEF.keychain-db",
        "agent-0123456789abcdef.keychain",
        "agent-0123456789abcdef0.keychain-db",
        "agent-0123456789abcde.keychain-db",
        "xagent-0123456789abcdef.keychain-db",
    ] {
        assert!(!agent_keychain_name(name), "{name}");
    }
}
