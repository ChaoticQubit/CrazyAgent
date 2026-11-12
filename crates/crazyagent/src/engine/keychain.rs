use crate::engine::EngineError;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SECURITY: &str = "/usr/bin/security";
const STEP_TIME: Duration = Duration::from_secs(10);
const ITEM_NOT_FOUND: i32 = 44;
const RECORDS_MAX: usize = 8;
const KEYCHAIN_SUFFIX: &str = ".keychain-db";
const AGENT_PREFIX: &str = "agent-";
const NONCE_HEX: usize = 16;
const LOGIN_KEYCHAIN: &str = "login.keychain-db";
const FOLDER_ENTRIES_MAX: usize = 64;

fn failure() -> EngineError {
    EngineError("native_auth_claude_keychain_unavailable".into())
}

/// Runs `security` once with only `HOME` set to the private home, within ten seconds.
/// Inputs: the home and the arguments, which name the keychain file. Output: the exit code,
/// or a failure when it cannot start, is killed or runs past its time.
fn exit_code(home: &Path, args: &[&std::ffi::OsStr]) -> Result<i32, EngineError> {
    let mut child = crate::spawn_gate::spawn(
        Command::new(SECURITY)
            .args(args)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .current_dir(home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .map_err(|_| failure())?;
    let deadline = Instant::now() + STEP_TIME;
    loop {
        if let Some(status) = child.try_wait().map_err(|_| failure())? {
            return status.code().ok_or_else(failure);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failure());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn security(home: &Path, args: &[&std::ffi::OsStr]) -> Result<(), EngineError> {
    match exit_code(home, args)? {
        0 => Ok(()),
        _ => Err(failure()),
    }
}

/// Whether `path` in a profile home an agent may have written exists as a real folder, or as
/// a regular file with one link, without following a link. Inputs: the path and which kind
/// it must be. Output: true or false, or a failure for a link or any other entry.
fn present(path: &Path, folder: bool) -> Result<bool, EngineError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if folder && metadata.is_dir() => Ok(true),
        Ok(metadata) if !folder && metadata.is_file() && metadata.nlink() == 1 => Ok(true),
        Ok(_) => Err(failure()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(failure()),
    }
}

/// Gives a private Claude profile home its own unlocked macOS keychain, so the official CLI
/// stores and reads its token there instead of asking macOS to create the absent login
/// keychain. Input: the absolute private profile home. Output: the keychain path, set as
/// that home's default and only search-list keychain; the owner's real keychain settings
/// are untouched.
pub(crate) fn prepare_profile_keychain(home: &Path) -> Result<PathBuf, EngineError> {
    if !home.is_absolute() || !home.is_dir() {
        return Err(failure());
    }
    let library = home.join("Library");
    let directory = library.join("Keychains");
    let preferences = library.join("Preferences");
    let keychain = directory.join("login.keychain-db");
    for folder in [&library, &directory, &preferences] {
        present(folder, true)?;
    }
    let existing = present(&keychain, false)?;
    present(&preferences.join("com.apple.security.plist"), false)?;
    for folder in [&library, &directory, &preferences] {
        if !present(folder, true)? {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(folder)
                .map_err(|_| failure())?;
        }
    }
    let path = keychain.as_os_str();
    let empty = std::ffi::OsStr::new("");
    if !existing {
        security(
            home,
            &["create-keychain".as_ref(), "-p".as_ref(), empty, path],
        )?;
    }
    let _ = security(
        home,
        &["unlock-keychain".as_ref(), "-p".as_ref(), empty, path],
    );
    let _ = security(home, &["set-keychain-settings".as_ref(), path]);
    security(
        home,
        &[
            "list-keychains".as_ref(),
            "-d".as_ref(),
            "user".as_ref(),
            "-s".as_ref(),
            path,
        ],
    )?;
    security(
        home,
        &[
            "default-keychain".as_ref(),
            "-d".as_ref(),
            "user".as_ref(),
            "-s".as_ref(),
            path,
        ],
    )?;
    for private in [home.join("Library"), directory.clone()] {
        std::fs::set_permissions(
            &private,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .map_err(|_| failure())?;
    }
    std::fs::set_permissions(
        &keychain,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .map_err(|_| failure())?;
    assert!(keychain.is_file());
    Ok(keychain)
}

/// Whether `name` is one this module gives an agent keychain: `agent-`, sixteen lowercase
/// hex digits and `.keychain-db`. No other keychain file is ever deleted. Input: the file
/// name. Output: true or false.
fn agent_keychain_name(name: &str) -> bool {
    name.strip_prefix(AGENT_PREFIX)
        .and_then(|rest| rest.strip_suffix(KEYCHAIN_SUFFIX))
        .is_some_and(|hex| {
            hex.len() == NONCE_HEX
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

/// The home the owner's account record names, read from the user database rather than from
/// `HOME`, canonical. Input: none. Output: the folder, or a failure when it cannot be read.
fn owner_home() -> Result<PathBuf, EngineError> {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid())
        .map_err(|_| failure())?
        .ok_or_else(failure)?;
    std::fs::canonicalize(&user.dir).map_err(|_| failure())
}

/// Refuses a home the engine may not give an agent keychain, before any `security` call: it
/// must be its own canonical path, so no link sits anywhere on it, lie strictly below the
/// canonical engine-owned `root`, not be the owner's own home, and hold no
/// `login.keychain-db`, which only a signed-in account's profile home or a real login home
/// has. Inputs: the root the engine keeps agent homes under and the home. Output: done, or a
/// failure.
fn agent_home_guard(root: &Path, home: &Path) -> Result<(), EngineError> {
    if !root.is_absolute() || !home.is_absolute() {
        return Err(failure());
    }
    let root = std::fs::canonicalize(root).map_err(|_| failure())?;
    if std::fs::canonicalize(home).map_err(|_| failure())? != home
        || !home.starts_with(&root)
        || home == root
        || home == owner_home()?
    {
        return Err(failure());
    }
    match std::fs::symlink_metadata(home.join("Library/Keychains").join(LOGIN_KEYCHAIN)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => return Err(failure()),
    }
    assert!(home.starts_with(&root) && home != root);
    Ok(())
}

/// The agent keychains an earlier launch left in a private home's keychain folder, read
/// without following a link; a keychain under any other name stays where it is. Input: the
/// folder, already checked as a real folder or absent. Output: their paths, or a failure
/// for an agent keychain name on a link, a hard-linked file or any other entry, or for a
/// folder of more than 64 entries.
fn earlier_keychains(directory: &Path) -> Result<Vec<PathBuf>, EngineError> {
    if !present(directory, true)? {
        return Ok(Vec::new());
    }
    let mut earlier = Vec::new();
    for (index, entry) in std::fs::read_dir(directory)
        .map_err(|_| failure())?
        .enumerate()
    {
        if index >= FOLDER_ENTRIES_MAX {
            return Err(failure());
        }
        let path = entry.map_err(|_| failure())?.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(agent_keychain_name)
            && present(&path, false)?
        {
            earlier.push(path);
        }
    }
    assert!(earlier.len() < FOLDER_ENTRIES_MAX);
    Ok(earlier)
}

/// Makes the `Library` and `Library/Preferences` folders a home's keychain settings live in,
/// refusing a link at either, at `Library/Keychains` or at the settings file. Input: the
/// home. Output: done, or a failure.
fn settings_folders(home: &Path) -> Result<(), EngineError> {
    let library = home.join("Library");
    let preferences = library.join("Preferences");
    for folder in [&library, &library.join("Keychains"), &preferences] {
        present(folder, true)?;
    }
    present(&preferences.join("com.apple.security.plist"), false)?;
    for folder in [&library, &preferences] {
        if !present(folder, true)? {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(folder)
                .map_err(|_| failure())?;
        }
    }
    Ok(())
}

/// Makes `keychain` the default and only search-list keychain of `home`, default first, so
/// the home's default always exists. Inputs: the home and the keychain. Output: done, or a
/// failure.
fn point_at(home: &Path, keychain: &Path) -> Result<(), EngineError> {
    for setting in ["default-keychain", "list-keychains"] {
        set_setting(home, setting, keychain)?;
    }
    Ok(())
}

/// Sets one of a home's own keychain settings, `default-keychain` or `list-keychains`, to
/// `keychain` alone, in that home's user domain. Inputs: the home, the setting and the
/// keychain. Output: done, or a failure.
fn set_setting(home: &Path, setting: &str, keychain: &Path) -> Result<(), EngineError> {
    assert!(matches!(setting, "default-keychain" | "list-keychains"));
    security(
        home,
        &[
            setting.as_ref(),
            "-d".as_ref(),
            "user".as_ref(),
            "-s".as_ref(),
            keychain.as_os_str(),
        ],
    )
}

/// Deletes one agent keychain this module created, through `security` and then from the
/// folder when `security` could not. Inputs: the home and the keychain. Output: done, or a
/// failure for a keychain under another name or one that will not go.
fn discard(home: &Path, keychain: &Path) -> Result<(), EngineError> {
    if !keychain
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(agent_keychain_name)
    {
        return Err(failure());
    }
    let _ = security(home, &["delete-keychain".as_ref(), keychain.as_os_str()]);
    if present(keychain, false)? {
        std::fs::remove_file(keychain).map_err(|_| failure())?;
    }
    assert!(!keychain.exists());
    Ok(())
}

/// Gives an engine-made agent home a newly created keychain under a name no earlier launch
/// used, makes it the default and only search-list keychain of that home and of every home
/// in `sharers`, such as a native lead's teammates' home, and only then deletes every agent
/// keychain an earlier launch left in the home. A harness still running in either home, as
/// a live agent taken over after an engine restart is, never finds it without a default,
/// which would make macOS ask the owner to create one. When a step fails before the home's
/// default moved, the new keychain is deleted again, so a failing handoff leaves nothing
/// behind; after that it stays as the default and the next handoff deletes it. An earlier
/// keychain may be locked, as every keychain is after a restart, and `security` cannot
/// unlock one with an empty password; nothing in it outlives a turn anyway. The new keychain
/// is unlocked and locks neither on a timeout nor on sleep. Only `agent-<16 hex>.keychain-db`
/// files are ever deleted. Inputs: the canonical root the engine keeps agent homes under,
/// the home and the homes that share its keychain. Output: the keychain path, or a failure,
/// with nothing written when a home fails [`agent_home_guard`] or a folder or keychain there
/// is a link.
pub(crate) fn fresh_profile_keychain(
    root: &Path,
    home: &Path,
    sharers: &[&Path],
) -> Result<PathBuf, EngineError> {
    agent_home_guard(root, home)?;
    for sharer in sharers {
        agent_home_guard(root, sharer)?;
        if *sharer == home {
            return Err(failure());
        }
    }
    let directory = home.join("Library/Keychains");
    settings_folders(home)?;
    for sharer in sharers {
        settings_folders(sharer)?;
    }
    let earlier = earlier_keychains(&directory)?;
    if !present(&directory, true)? {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| failure())?;
    }
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| failure())?;
    let keychain = directory.join(format!(
        "{AGENT_PREFIX}{:016x}{KEYCHAIN_SUFFIX}",
        u64::from_le_bytes(nonce)
    ));
    assert!(!earlier.contains(&keychain));
    assert_eq!(keychain.parent(), Some(directory.as_path()));
    assert!(keychain.starts_with(home));
    let path = keychain.as_os_str();
    let empty = std::ffi::OsStr::new("");
    security(
        home,
        &["create-keychain".as_ref(), "-p".as_ref(), empty, path],
    )?;
    let settled = security(home, &["set-keychain-settings".as_ref(), path])
        .and_then(|()| set_setting(home, "default-keychain", &keychain));
    if let Err(error) = settled {
        discard(home, &keychain)?;
        return Err(error);
    }
    set_setting(home, "list-keychains", &keychain)?;
    for sharer in sharers {
        point_at(sharer, &keychain)?;
    }
    for private in [&home.join("Library"), &directory] {
        std::fs::set_permissions(private, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .map_err(|_| failure())?;
    }
    std::fs::set_permissions(
        &keychain,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .map_err(|_| failure())?;
    assert!(present(&keychain, false)?);
    for old in &earlier {
        discard(home, old)?;
    }
    assert!(earlier.iter().all(|old| !old.exists()));
    Ok(keychain)
}

/// Deletes every item filed under `service` from one agent keychain in a private home's
/// keychain folder, so the harness there reads the credential file the engine wrote instead
/// of a stale copy. The keychain is named in each call and no link is followed to it; a
/// keychain or an item already gone is fine. Inputs: the absolute home, the agent keychain
/// file in its keychain folder and the service. Output: done, or a failure, also for a
/// keychain outside that folder or under a name this module never gives one.
pub(crate) fn forget_record(
    home: &Path,
    keychain: &Path,
    service: &str,
) -> Result<(), EngineError> {
    let directory = home.join("Library/Keychains");
    if !home.is_absolute()
        || keychain.parent() != Some(directory.as_path())
        || !keychain
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(agent_keychain_name)
    {
        return Err(failure());
    }
    if !present(&home.join("Library"), true)?
        || !present(&directory, true)?
        || !present(keychain, false)?
    {
        return Ok(());
    }
    let service = std::ffi::OsStr::new(service);
    for _ in 0..RECORDS_MAX {
        let args = [
            "delete-generic-password".as_ref(),
            "-s".as_ref(),
            service,
            keychain.as_os_str(),
        ];
        match exit_code(home, &args)? {
            0 => {}
            ITEM_NOT_FOUND => return Ok(()),
            _ => return Err(failure()),
        }
    }
    Err(failure())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::test_keychain::TestKeychain;

    fn read(home: &Path, args: &[&str]) -> String {
        let output = crate::spawn_gate::output(
            Command::new(SECURITY)
                .args(args)
                .env_clear()
                .env("HOME", home)
                .env("PATH", "/usr/bin:/bin")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )
        .unwrap();
        assert!(output.status.success(), "security {args:?} failed");
        String::from_utf8(output.stdout).unwrap()
    }

    fn shown(home: &Path, args: &[&str]) -> String {
        let output = crate::spawn_gate::output(
            Command::new(SECURITY)
                .args(args)
                .env_clear()
                .env("HOME", home)
                .env("PATH", "/usr/bin:/bin")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        )
        .unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn private_home_gets_its_own_keychain_that_no_other_home_sees() {
        let root = tempfile::Builder::new()
            .prefix("ca-keychain-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let home = root.path().join("home");
        let other = root.path().join("other");
        for folder in [&home, &other] {
            std::fs::create_dir(folder).unwrap();
        }
        let keychain = prepare_profile_keychain(&home).unwrap();
        assert_eq!(keychain, home.join("Library/Keychains/login.keychain-db"));
        let text = keychain.to_str().unwrap().to_owned();
        assert!(read(&home, &["default-keychain", "-d", "user"]).contains(&text));
        assert!(read(&home, &["list-keychains", "-d", "user"]).contains(&text));
        let adopted = TestKeychain::adopt(&home, keychain.clone());
        adopted.add("probe", "crazyagent-keychain-test", "value");
        assert_eq!(adopted.find("crazyagent-keychain-test"), "value");
        assert_eq!(prepare_profile_keychain(&home).unwrap(), keychain);
        assert_eq!(adopted.find("crazyagent-keychain-test"), "value");
        for args in [
            ["default-keychain", "-d", "user"],
            ["list-keychains", "-d", "user"],
        ] {
            assert!(
                !shown(&other, &args).contains(&text),
                "another home sees the profile keychain through {args:?}"
            );
        }
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&keychain).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(prepare_profile_keychain(Path::new("relative")).is_err());
    }

    fn entries(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn keychain_folder_links_are_refused_untouched_and_a_sandboxed_agent_cannot_plant_them() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::Builder::new()
            .prefix("ca-keychain-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let home = root.path().canonicalize().unwrap().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        let agent = crate::sandbox::test_agent::TestAgent::with_home(&home);
        let preferences = agent.peer.join("Preferences");
        let keychains = agent.peer.join("Keychains");
        std::fs::create_dir(&preferences).unwrap();
        std::fs::create_dir(&keychains).unwrap();
        let planting = agent.run(&format!(
            "mkdir -p '{home}/Library' && ln -s '{preferences}' '{home}/Library/Preferences' \
             && ln -s '{keychains}' '{home}/Library/Keychains'",
            home = home.display(),
            preferences = preferences.display(),
            keychains = keychains.display()
        ));
        assert!(!planting.status.success());
        assert!(std::fs::symlink_metadata(home.join("Library/Preferences")).is_err());
        assert!(std::fs::symlink_metadata(home.join("Library/Keychains")).is_err());

        std::fs::create_dir_all(home.join("Library")).unwrap();
        symlink(&preferences, home.join("Library/Preferences")).unwrap();
        symlink(&keychains, home.join("Library/Keychains")).unwrap();
        assert!(prepare_profile_keychain(&home).is_err());
        assert!(fresh_profile_keychain(root.path(), &home, &[]).is_err());
        assert!(
            forget_record(
                &home,
                &home.join("Library/Keychains/login.keychain-db"),
                "crazyagent-keychain-test"
            )
            .is_err()
        );
        assert_eq!(entries(&preferences), Vec::<String>::new());
        assert_eq!(entries(&keychains), Vec::<String>::new());

        let planted = agent.peer.join("keychain");
        std::fs::write(&planted, "peer").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o644)).unwrap();
        let replanting = agent.run(&format!(
            "cd '{home}/Library' && rm Preferences Keychains && mkdir Keychains \
             && ln -s '{planted}' Keychains/login.keychain-db",
            home = home.display(),
            planted = planted.display()
        ));
        assert!(!replanting.status.success());
        std::fs::remove_file(home.join("Library/Preferences")).unwrap();
        std::fs::remove_file(home.join("Library/Keychains")).unwrap();
        std::fs::create_dir(home.join("Library/Keychains")).unwrap();
        symlink(&planted, home.join("Library/Keychains/login.keychain-db")).unwrap();
        assert!(prepare_profile_keychain(&home).is_err());
        assert!(fresh_profile_keychain(root.path(), &home, &[]).is_err());
        assert!(
            forget_record(
                &home,
                &home.join("Library/Keychains/login.keychain-db"),
                "crazyagent-keychain-test"
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(&planted).unwrap(), "peer");
        assert_eq!(
            std::fs::metadata(&planted).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(entries(&home.join("Library")), ["Keychains"]);
    }

    fn private_home(root: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let home = root.canonicalize().unwrap().join(name);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        home
    }

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("ca-keychain-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap()
    }

    fn hex(text: &str) -> String {
        text.bytes().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn forgetting_a_record_deletes_every_item_of_that_service_and_nothing_else() {
        let root = scratch();
        let home = private_home(root.path(), "home");
        let bare = private_home(root.path(), "bare");
        forget_record(
            &bare,
            &bare.join("Library/Keychains/agent-0123456789abcdef.keychain-db"),
            "crazyagent-forget-test",
        )
        .unwrap();
        assert_eq!(entries(&bare), Vec::<String>::new());
        let keychain = fresh_profile_keychain(root.path(), &home, &[]).unwrap();
        let adopted = TestKeychain::adopt(&home, keychain.clone());
        adopted.add("probe", "crazyagent-forget-test", "stale");
        adopted.add("other", "crazyagent-forget-test", "stale too");
        adopted.add("probe", "crazyagent-keep-test", "kept");
        assert!(adopted.holds("crazyagent-forget-test"));

        forget_record(&home, &keychain, "crazyagent-forget-test").unwrap();
        assert!(!adopted.holds("crazyagent-forget-test"));
        assert_eq!(adopted.find("crazyagent-keep-test"), "kept");
        forget_record(&home, &keychain, "crazyagent-forget-test").unwrap();
        assert!(
            forget_record(
                Path::new("relative"),
                Path::new("relative/Library/Keychains/login.keychain-db"),
                "crazyagent-forget-test"
            )
            .is_err()
        );
        assert!(
            forget_record(
                &home,
                &bare.join("Library/Keychains/login.keychain-db"),
                "crazyagent-forget-test"
            )
            .is_err()
        );
        assert!(
            forget_record(
                &home,
                &home.join("login.keychain-db"),
                "crazyagent-forget-test"
            )
            .is_err()
        );
        let profile = private_home(root.path(), "profile");
        let login = TestKeychain::adopt(&profile, prepare_profile_keychain(&profile).unwrap());
        login.add("probe", "crazyagent-forget-test", "signed in");
        for named in [
            login.path().to_path_buf(),
            home.join("Library/Keychains/login.keychain-db"),
            home.join("Library/Keychains/agent-0123456789ABCDEF.keychain-db"),
            home.join("Library/Keychains/agent-0123.keychain-db"),
        ] {
            let owner = named.parent().unwrap().parent().unwrap().parent().unwrap();
            assert!(forget_record(owner, &named, "crazyagent-forget-test").is_err());
        }
        assert_eq!(login.find("crazyagent-forget-test"), "signed in");
    }

    fn keychain_files(home: &Path) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(home.join("Library/Keychains"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_str().unwrap().ends_with(".keychain-db"))
            .collect();
        found.sort();
        found
    }

    #[test]
    fn a_fresh_keychain_under_a_new_name_is_the_default_before_the_locked_one_is_deleted() {
        let root = scratch();
        let home = private_home(root.path(), "home");
        let earlier = TestKeychain::adopt(
            &home,
            fresh_profile_keychain(root.path(), &home, &[]).unwrap(),
        );
        let earlier_text = earlier.path().to_str().unwrap().to_owned();
        earlier.add("probe", "crazyagent-fresh-test", "old");
        earlier.security(&["lock-keychain", &earlier_text], None);

        let fresh = fresh_profile_keychain(root.path(), &home, &[]).unwrap();
        assert_ne!(fresh, earlier.path());
        assert_eq!(fresh.parent(), earlier.path().parent());
        assert!(!earlier.path().exists());
        assert_eq!(keychain_files(&home), [fresh.clone()]);
        let keychain = TestKeychain::adopt(&home, fresh.clone());
        let text = fresh.to_str().unwrap();
        for which in ["default-keychain", "list-keychains"] {
            let setting = keychain.setting(which);
            assert!(setting.contains(text) && !setting.contains(&earlier_text));
        }
        assert!(!keychain.holds("crazyagent-fresh-test"));
        keychain.add("probe", "crazyagent-fresh-test", "new");
        assert_eq!(keychain.find("crazyagent-fresh-test"), "new");

        let again = TestKeychain::adopt(
            &home,
            fresh_profile_keychain(root.path(), &home, &[]).unwrap(),
        );
        assert_ne!(again.path(), fresh);
        assert!(!fresh.exists());
        assert_eq!(keychain_files(&home), [again.path().to_path_buf()]);
        assert!(
            again
                .setting("default-keychain")
                .contains(again.path().to_str().unwrap())
        );
        assert!(fresh_profile_keychain(root.path(), Path::new("relative"), &[]).is_err());
    }

    #[test]
    fn preparing_one_home_leaves_another_prepared_homes_default_and_search_list_unchanged() {
        let root = scratch();
        let first = private_home(root.path(), "first");
        let second = private_home(root.path(), "second");
        let second_keychain =
            TestKeychain::adopt(&second, prepare_profile_keychain(&second).unwrap());
        let settings = |keychain: &TestKeychain| {
            [
                keychain.setting("default-keychain"),
                keychain.setting("list-keychains"),
            ]
        };
        let before = settings(&second_keychain);
        let first_keychain = TestKeychain::adopt(&first, prepare_profile_keychain(&first).unwrap());
        assert_eq!(settings(&second_keychain), before);
        let second_text = second_keychain.path().to_str().unwrap();
        let first_text = first_keychain.path().to_str().unwrap();
        for text in &before {
            assert!(text.contains(second_text) && !text.contains(first_text));
        }
        for text in settings(&first_keychain) {
            assert!(text.contains(first_text) && !text.contains(second_text));
        }
    }

    #[test]
    fn a_write_naming_no_keychain_lands_in_the_prepared_keychain_the_sandbox_sees_as_default() {
        let root = scratch();
        let home = private_home(root.path(), "home");
        let keychain = TestKeychain::adopt(&home, prepare_profile_keychain(&home).unwrap());
        let text = keychain.path().to_str().unwrap().to_owned();
        assert!(
            keychain.setting("default-keychain").contains(&text),
            "no write may name no keychain unless the home's default is the prepared one"
        );
        let line = format!(
            "add-generic-password -U -a \"probe\" -s \"crazyagent-unnamed-test\" -X {}\n",
            hex("unnamed")
        );
        let mut child = crate::spawn_gate::spawn(
            Command::new(SECURITY)
                .arg("-i")
                .env_clear()
                .env("HOME", &home)
                .env("PATH", "/usr/bin:/bin")
                .current_dir(&home)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        {
            use std::io::Write as _;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(line.as_bytes())
                .unwrap();
        }
        assert!(child.wait().unwrap().success());
        assert_eq!(keychain.find("crazyagent-unnamed-test"), "unnamed");

        let agent = crate::sandbox::test_agent::TestAgent::with_home(&home);
        let seen = agent.run("/usr/bin/security default-keychain -d user");
        assert!(seen.status.success());
        assert!(
            String::from_utf8_lossy(&seen.stdout).contains(&text),
            "inside the sandbox the harness would not see the prepared keychain as its default"
        );
    }

    #[test]
    fn a_sandboxed_agent_cannot_move_link_or_rewrite_its_keychain_or_search_list() {
        let root = scratch();
        let home = private_home(root.path(), "home");
        let keychain = TestKeychain::adopt(&home, prepare_profile_keychain(&home).unwrap());
        let before = [
            keychain.setting("default-keychain"),
            keychain.setting("list-keychains"),
        ];
        let plist = home.join("Library/Preferences/com.apple.security.plist");
        let settings = std::fs::read(&plist).ok();
        let agent = crate::sandbox::test_agent::TestAgent::with_home(&home);
        for script in [
            "mv \"$HOME/Library\" \"$HOME/moved\"",
            "mv \"$HOME/Library/Keychains\" \"$HOME/moved\"",
            "mv \"$HOME/Library/Preferences\" \"$HOME/moved\"",
            "rm \"$HOME/Library/Keychains/login.keychain-db\"",
            "mv \"$HOME/Library/Keychains/login.keychain-db\" \"$HOME/moved\"",
            "printf x >> \"$HOME/Library/Keychains/login.keychain-db\"",
            "ln -s /tmp \"$HOME/Library/Keychains/other.keychain-db\"",
            "printf x > \"$HOME/Library/Preferences/com.apple.security.plist\"",
            "rm \"$HOME/Library/Preferences/com.apple.security.plist\"",
            "ln -s /tmp \"$HOME/Library/preferences/COM.APPLE.SECURITY.PLIST.new\" && mv \"$HOME/Library/preferences/COM.APPLE.SECURITY.PLIST.new\" \"$HOME/Library/preferences/COM.APPLE.SECURITY.PLIST\"",
            "chmod 0755 \"$HOME/Library\"",
        ] {
            assert!(
                !agent.run(script).status.success(),
                "the agent ran: {script}"
            );
        }
        agent.shell(
            "mkdir -p \"$HOME/Library/Caches/tool\" && printf x > \"$HOME/Library/Caches/tool/file\" \
             && printf x > \"$HOME/Library/Preferences/tool.plist\"",
        );
        assert_eq!(std::fs::read(&plist).ok(), settings);
        assert_eq!(
            [
                keychain.setting("default-keychain"),
                keychain.setting("list-keychains"),
            ],
            before
        );
        assert_eq!(prepare_profile_keychain(&home).unwrap(), keychain.path());
    }

    #[test]
    fn removing_an_agents_files_takes_its_keychain_and_leaves_other_homes_untouched() {
        let root = scratch();
        let team = root.path().canonicalize().unwrap().join("team");
        std::fs::create_dir(&team).unwrap();
        let home = private_home(&team, "agents/agent-7/home");
        let other = private_home(root.path(), "other");
        let other_keychain = TestKeychain::adopt(&other, prepare_profile_keychain(&other).unwrap());
        let before = [
            other_keychain.setting("default-keychain"),
            other_keychain.setting("list-keychains"),
        ];
        let keychain = fresh_profile_keychain(&team, &home, &[]).unwrap();
        assert!(keychain.is_file());

        crate::team::cleanup::remove_agent_files(&team, 7).unwrap();
        assert!(!keychain.exists());
        assert!(!team.join("agents/agent-7").exists());
        let after = [
            other_keychain.setting("default-keychain"),
            other_keychain.setting("list-keychains"),
        ];
        assert_eq!(after, before);
        assert!(
            after
                .iter()
                .all(|text| !text.contains(keychain.to_str().unwrap()))
        );
    }

    #[test]
    fn an_agent_keychain_is_refused_outside_the_root_through_a_link_or_beside_a_login_keychain() {
        use std::os::unix::fs::symlink;
        let root = scratch();
        let engine = private_home(root.path(), "engine");
        let outside = private_home(root.path(), "outside");
        let profile = private_home(&engine, "native-profiles/k/home");
        let login = TestKeychain::adopt(&profile, prepare_profile_keychain(&profile).unwrap());
        login.add("probe", "crazyagent-guard-test", "signed in");
        let before = login.setting("default-keychain");
        let agent = private_home(&engine, "agents/agent-1/home");
        let linked = engine.join("linked");
        symlink(&agent, &linked).unwrap();
        let none: Vec<&Path> = Vec::new();
        for (home, sharers) in [
            (outside.as_path(), none.clone()),
            (engine.as_path(), none.clone()),
            (profile.as_path(), none.clone()),
            (linked.as_path(), none.clone()),
            (agent.as_path(), vec![profile.as_path()]),
            (agent.as_path(), vec![outside.as_path()]),
            (agent.as_path(), vec![agent.as_path()]),
        ] {
            assert!(
                fresh_profile_keychain(&engine, home, &sharers).is_err(),
                "{home:?} with {sharers:?} got an agent keychain"
            );
        }
        for home in [&outside, &agent] {
            assert!(
                std::fs::symlink_metadata(home.join("Library")).is_err(),
                "a refused handoff wrote into {home:?}"
            );
        }
        assert_eq!(login.find("crazyagent-guard-test"), "signed in");
        assert_eq!(login.setting("default-keychain"), before);
        assert_eq!(keychain_files(&profile), [login.path().to_path_buf()]);
    }

    #[test]
    fn the_owners_home_and_any_login_keychain_name_are_refused_before_any_security_call() {
        let owner = owner_home().unwrap();
        assert!(agent_home_guard(Path::new("/"), &owner).is_err());
        assert!(!agent_keychain_name(LOGIN_KEYCHAIN));
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
    fn leftover_agent_keychains_past_the_old_cap_are_swept_and_other_keychains_stay() {
        let root = scratch();
        let home = private_home(root.path(), "home");
        let directory = home.join("Library/Keychains");
        let mut leftovers = vec![fresh_profile_keychain(root.path(), &home, &[]).unwrap()];
        let empty = std::ffi::OsStr::new("");
        for index in 0..10_u64 {
            let leftover =
                directory.join(format!("agent-{:016x}.keychain-db", 0xfeed_0000 + index));
            assert!(
                security(
                    &home,
                    &[
                        "create-keychain".as_ref(),
                        "-p".as_ref(),
                        empty,
                        leftover.as_os_str()
                    ],
                )
                .is_ok()
            );
            leftovers.push(leftover);
        }
        let foreign = directory.join("notes.keychain-db");
        assert!(
            security(
                &home,
                &[
                    "create-keychain".as_ref(),
                    "-p".as_ref(),
                    empty,
                    foreign.as_os_str()
                ],
            )
            .is_ok()
        );
        let foreign = TestKeychain::adopt(&home, foreign);

        let fresh = TestKeychain::adopt(
            &home,
            fresh_profile_keychain(root.path(), &home, &[]).unwrap(),
        );
        assert!(leftovers.iter().all(|old| !old.exists()));
        assert_eq!(
            keychain_files(&home),
            [fresh.path().to_path_buf(), foreign.path().to_path_buf()]
        );
        for which in ["default-keychain", "list-keychains"] {
            let setting = fresh.setting(which);
            assert!(setting.contains(fresh.path().to_str().unwrap()));
            assert!(!setting.contains(foreign.path().to_str().unwrap()));
        }
    }

    #[test]
    fn a_sharing_home_follows_the_new_keychain_before_the_old_one_is_deleted() {
        let root = scratch();
        let lead = private_home(root.path(), "agents/agent-3/home");
        let mates = private_home(root.path(), "agents/agent-3/mates-home");
        let first = TestKeychain::adopt(
            &lead,
            fresh_profile_keychain(root.path(), &lead, &[&mates]).unwrap(),
        );
        let first_text = first.path().to_str().unwrap().to_owned();
        for which in [
            ["default-keychain", "-d", "user"],
            ["list-keychains", "-d", "user"],
        ] {
            assert!(read(&mates, &which).contains(&first_text));
        }

        let second = TestKeychain::adopt(
            &lead,
            fresh_profile_keychain(root.path(), &lead, &[&mates]).unwrap(),
        );
        assert!(!first.path().exists());
        let second_text = second.path().to_str().unwrap();
        for which in [
            ["default-keychain", "-d", "user"],
            ["list-keychains", "-d", "user"],
        ] {
            let setting = read(&mates, &which);
            assert!(setting.contains(second_text) && !setting.contains(&first_text));
        }
        assert!(
            std::fs::read_dir(mates.join("Library/Keychains")).map_or(true, |found| found
                .map(|entry| entry.unwrap().file_name())
                .all(|name| !name.to_string_lossy().ends_with(KEYCHAIN_SUFFIX))),
            "the sharing home got a keychain of its own"
        );
    }
}
