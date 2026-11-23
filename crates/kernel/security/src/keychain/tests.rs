use super::*;

/// The same folder reached through the data volume, `/System/Volumes/Data/<path>`, as the
/// engine's projects home names it. Input: an absolute path. Output: that spelling when it
/// is the same entry, else `None`, in which case the row skips its twin assert.
fn data_twin(path: &Path) -> Option<PathBuf> {
    let twin = Path::new("/System/Volumes/Data").join(path.strip_prefix("/").ok()?);
    let (entry, other) = (
        std::fs::metadata(path).ok()?,
        std::fs::metadata(&twin).ok()?,
    );
    ((entry.dev(), entry.ino()) == (other.dev(), other.ino())).then_some(twin)
}

#[test]
fn keychain_names_are_byte_equal() {
    assert_eq!(SECURITY, "/usr/bin/security");
    assert_eq!(LOGIN_KEYCHAIN, "login.keychain-db");
    assert_eq!(PROFILE_KEYCHAIN, "profile.keychain-db");
    assert_eq!(ASIDE_KEYCHAIN, "profile-locked.keychain-db");
    assert_eq!(KEYCHAIN_SUFFIX, ".keychain-db");
    assert_eq!(STEP_TIME, Duration::from_secs(10));
    assert_eq!((ITEM_NOT_FOUND, AUTH_FAILED), (44, 51));
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

#[test]
fn the_owners_home_and_any_login_keychain_name_are_refused_before_any_security_call() {
    let owner = owner_home().unwrap();
    assert!(agent_home_guard(Path::new("/"), &owner).is_err());
    if let Some(long) = data_twin(&owner) {
        assert!(agent_home_guard(Path::new("/System/Volumes/Data"), &long).is_err());
    }
    assert!(!agent_keychain_name(LOGIN_KEYCHAIN));
    assert!(!agent_keychain_name(PROFILE_KEYCHAIN));
    assert!(agent_keychain_name("agent-0123456789abcdef.keychain-db"));
    for name in [
        "agent-0123456789ABCDEF.keychain-db",
        "agent-0123456789abcdef.keychain",
        "agent-0123456789abcdef0.keychain-db",
        "agent-0123456789abcde.keychain-db",
        "xagent-0123456789abcdef.keychain-db",
        "agent-0123456789abcdeg.keychain-db",
    ] {
        assert!(
            !agent_keychain_name(name),
            "{name} passed as an agent keychain"
        );
    }
}

#[test]
fn the_owners_own_home_is_refused_as_a_profile_home_before_any_rename() {
    let owner = owner_home().unwrap();
    assert_eq!(profile_home_guard(&owner), Err(failure()));
    if let Some(long) = data_twin(&owner) {
        assert_eq!(
            profile_home_guard(&long),
            Err(failure()),
            "the owner's home in its data volume spelling passed as a profile home"
        );
    }
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let home = base.join("home");
    std::fs::create_dir(&home).unwrap();
    assert_eq!(profile_home_guard(&home), Ok(()));
    let link = base.join("link");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    assert_eq!(profile_home_guard(&link), Err(failure()));
    assert_eq!(profile_home_guard(Path::new("home")), Err(failure()));
}

#[test]
#[should_panic(expected = "the owner home check takes an absolute path")]
fn the_owner_home_check_trips_on_a_relative_path() {
    let _ = is_owner_home(Path::new("home"));
}

#[test]
#[should_panic(expected = "assertion failed: directory.is_absolute()")]
fn setting_aside_trips_on_a_relative_folder() {
    let _ = set_aside(Path::new("Library/Keychains"));
}

#[test]
#[should_panic(expected = "assertion failed: keychain.is_absolute()")]
fn unlocking_trips_on_a_relative_keychain() {
    let root = tempfile::tempdir().unwrap();
    let _ = unlock(
        root.path(),
        Path::new("Library/Keychains/profile.keychain-db"),
    );
}

#[test]
#[should_panic(expected = "assertion `left != right` failed")]
fn unlocking_trips_on_a_login_keychain_macos_26_refuses_to_unlock() {
    let root = tempfile::tempdir().unwrap();
    let _ = unlock(
        root.path(),
        &root.path().join("Library/Keychains").join(LOGIN_KEYCHAIN),
    );
}
