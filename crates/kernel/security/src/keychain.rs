use crate::error::StoreError;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The one `security` binary every keychain call runs.
pub const SECURITY: &str = "/usr/bin/security";
const STEP_TIME: Duration = Duration::from_secs(10);
const ITEM_NOT_FOUND: i32 = 44;
const RECORDS_MAX: usize = 8;
/// The file name ending of every keychain the engine makes or reads.
pub const KEYCHAIN_SUFFIX: &str = ".keychain-db";
const AGENT_PREFIX: &str = "agent-";
const NONCE_HEX: usize = 16;
/// The keychain name an earlier engine gave a profile home; macOS 26 refuses a password
/// unlock of any file with this name, so it is renamed before use.
pub const LOGIN_KEYCHAIN: &str = "login.keychain-db";
/// The keychain name of a profile home.
pub const PROFILE_KEYCHAIN: &str = "profile.keychain-db";
/// The name a profile keychain the empty password cannot open is moved aside to.
pub const ASIDE_KEYCHAIN: &str = "profile-locked.keychain-db";
const AUTH_FAILED: i32 = 51;
const FOLDER_ENTRIES_MAX: usize = 64;

fn failure() -> StoreError {
    StoreError::code("native_auth_claude_keychain_unavailable")
}

/// Runs `security` once with only `HOME` set to the private home, within ten seconds.
/// Inputs: the home and the arguments, which name the keychain file. Output: the exit code,
/// or a failure when it cannot start, is killed or runs past its time.
fn exit_code(home: &Path, args: &[&std::ffi::OsStr]) -> Result<i32, StoreError> {
    let mut child = kernel_processes::spawn_gate::spawn(
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

fn security(home: &Path, args: &[&std::ffi::OsStr]) -> Result<(), StoreError> {
    match exit_code(home, args)? {
        0 => Ok(()),
        _ => Err(failure()),
    }
}

/// Whether `path` in a profile home an agent may have written exists as a real folder, or as
/// a regular file with one link, without following a link. Inputs: the path and which kind
/// it must be. Output: true or false, or a failure for a link or any other entry.
fn present(path: &Path, folder: bool) -> Result<bool, StoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if folder && metadata.is_dir() => Ok(true),
        Ok(metadata) if !folder && metadata.is_file() && metadata.nlink() == 1 => Ok(true),
        Ok(_) => Err(failure()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(failure()),
    }
}

/// Tries the empty password the engine gives every keychain it makes on one keychain. On
/// macOS 26 `security` refuses a password unlock of any file named `login.keychain-db`, so no
/// keychain this module opens has that name. Inputs: the home and the keychain file. Output:
/// true when it opened, false when `security` refused that password, or a failure otherwise.
fn opens(home: &Path, keychain: &Path) -> Result<bool, StoreError> {
    assert!(keychain.is_absolute());
    assert_ne!(
        keychain.file_name(),
        Some(std::ffi::OsStr::new(LOGIN_KEYCHAIN))
    );
    let empty = std::ffi::OsStr::new("");
    let args = [
        "unlock-keychain".as_ref(),
        "-p".as_ref(),
        empty,
        keychain.as_os_str(),
    ];
    match exit_code(home, &args)? {
        0 => Ok(true),
        AUTH_FAILED => Ok(false),
        _ => Err(failure()),
    }
}

/// Unlocks one keychain with the empty password, then keeps it from locking itself on a
/// timeout or on sleep. Inputs: the home and the keychain file. Output: done, or a failure,
/// after which the caller makes no call that could ask the owner for a password.
fn unlock(home: &Path, keychain: &Path) -> Result<(), StoreError> {
    if !opens(home, keychain)? {
        return Err(failure());
    }
    security(
        home,
        &["set-keychain-settings".as_ref(), keychain.as_os_str()],
    )
}

/// Moves a profile keychain the empty password cannot open aside at sign-in, never deleting
/// it, so the owner keeps any password they set on it: a regular file with one link owned by
/// this uid is renamed in its folder, following no link, to the one aside name, which must be
/// free. Input: the home's checked keychain folder. Output: done, or a failure, nothing moved.
fn set_aside(directory: &Path) -> Result<(), StoreError> {
    assert!(directory.is_absolute());
    let keychain = directory.join(PROFILE_KEYCHAIN);
    let aside = directory.join(ASIDE_KEYCHAIN);
    let metadata = std::fs::symlink_metadata(&keychain).map_err(|_| failure())?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || present(&aside, false)?
    {
        return Err(failure());
    }
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        &keychain,
        rustix::fs::CWD,
        &aside,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|_| failure())
}

/// The keychain a profile home holds, under its current name: a `login.keychain-db` an
/// earlier engine made is first renamed `profile.keychain-db` in its folder, replacing no
/// file and following no link. Input: the home's keychain folder. Output: the keychain, or
/// none when the home has neither; a failure when it has both, or either is not a regular
/// file with one link.
fn profile_keychain(directory: &Path) -> Result<Option<PathBuf>, StoreError> {
    let legacy = directory.join(LOGIN_KEYCHAIN);
    let keychain = directory.join(PROFILE_KEYCHAIN);
    match (present(&legacy, false)?, present(&keychain, false)?) {
        (false, false) => return Ok(None),
        (true, true) => return Err(failure()),
        (false, true) => {}
        (true, false) => {
            let renamed = rustix::fs::renameat_with(
                rustix::fs::CWD,
                &legacy,
                rustix::fs::CWD,
                &keychain,
                rustix::fs::RenameFlags::NOREPLACE,
            );
            if renamed.is_err() && (present(&legacy, false)? || !present(&keychain, false)?) {
                return Err(failure());
            }
        }
    }
    Ok(Some(keychain))
}

/// Opens a signed-in profile home's keychain before the engine reads it: settles its name,
/// unlocks it, then points the home's default and search list at it again, so a cut-short
/// rename heals. Input: the canonical profile home, never the owner's own, checked first.
/// Output: the keychain, or a failure, also for a home with none, after which the caller
/// makes no call that could ask the owner for a password.
pub fn open_profile_keychain(home: &Path) -> Result<PathBuf, StoreError> {
    profile_home_guard(home)?;
    let directory = home.join("Library/Keychains");
    if !present(&home.join("Library"), true)? || !present(&directory, true)? {
        return Err(failure());
    }
    let keychain = profile_keychain(&directory)?.ok_or_else(failure)?;
    settings_folders(home)?;
    unlock(home, &keychain)?;
    point_at(home, &keychain)?;
    Ok(keychain)
}

/// Gives a private Claude profile home its own unlocked macOS keychain, so the official CLI
/// uses it instead of asking macOS for a login keychain; an earlier `login.keychain-db` is
/// renamed, and on a `login` sign-in one "" cannot open is set aside, never deleted. Inputs:
/// the canonical profile home, never the owner's own, and whether a login follows. Output:
/// the keychain, the home's default and only search-list one; the owner's stay untouched.
pub fn prepare_profile_keychain(home: &Path, login: bool) -> Result<PathBuf, StoreError> {
    profile_home_guard(home)?;
    if !home.is_dir() {
        return Err(failure());
    }
    let directory = home.join("Library/Keychains");
    let preferences = home.join("Library/Preferences");
    for folder in [&home.join("Library"), &directory, &preferences] {
        present(folder, true)?;
    }
    present(&preferences.join("com.apple.security.plist"), false)?;
    let existing = if present(&directory, true)? {
        profile_keychain(&directory)?
    } else {
        None
    };
    settings_folders(home)?;
    if !present(&directory, true)? {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| failure())?;
    }
    let keychain = match existing {
        Some(keychain) => keychain,
        None => create_keychain(home, &directory)?,
    };
    if !opens(home, &keychain)? {
        if !login {
            return Err(failure());
        }
        set_aside(&directory)?;
        create_keychain(home, &directory)?;
    }
    unlock(home, &keychain)?;
    point_at(home, &keychain)?;
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

/// Makes a new empty-password profile keychain in a home's checked keychain folder, where no
/// keychain of that name is left. Inputs: the home and its keychain folder. Output: the new
/// keychain, or a failure.
fn create_keychain(home: &Path, directory: &Path) -> Result<PathBuf, StoreError> {
    let keychain = directory.join(PROFILE_KEYCHAIN);
    let empty = std::ffi::OsStr::new("");
    security(
        home,
        &[
            "create-keychain".as_ref(),
            "-p".as_ref(),
            empty,
            keychain.as_os_str(),
        ],
    )?;
    Ok(keychain)
}

/// Whether `name` is one this module gives an agent keychain: `agent-`, sixteen lowercase
/// hex digits and `.keychain-db`. No other keychain file is ever deleted. Input: the file
/// name. Output: true or false.
pub fn agent_keychain_name(name: &str) -> bool {
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
fn owner_home() -> Result<PathBuf, StoreError> {
    let user = nix::unistd::User::from_uid(nix::unistd::getuid())
        .map_err(|_| failure())?
        .ok_or_else(failure)?;
    std::fs::canonicalize(&user.dir).map_err(|_| failure())
}

/// Tells whether a folder is the owner's own home by its device and inode, so every name
/// macOS gives that folder, the data volume's `/System/Volumes/Data` spelling included,
/// counts. Input: an absolute folder. Output: whether it is the owner's home, or a failure
/// when either folder cannot be read.
fn is_owner_home(home: &Path) -> Result<bool, StoreError> {
    use std::os::unix::fs::MetadataExt;
    assert!(
        home.is_absolute(),
        "the owner home check takes an absolute path"
    );
    let owner = std::fs::metadata(owner_home()?).map_err(|_| failure())?;
    let entry = std::fs::metadata(home).map_err(|_| failure())?;
    Ok((entry.dev(), entry.ino()) == (owner.dev(), owner.ino()))
}

/// Refuses a profile home the engine may not settle a keychain in, before any rename or
/// `security` call: it must be absolute and its own canonical path, so no link sits on it,
/// and never the owner's own home, whose login keychain a rename would take from the login
/// window. Input: the home. Output: done, or a failure.
fn profile_home_guard(home: &Path) -> Result<(), StoreError> {
    if !home.is_absolute()
        || std::fs::canonicalize(home).map_err(|_| failure())? != home
        || is_owner_home(home)?
    {
        return Err(failure());
    }
    Ok(())
}

/// Refuses a home the engine may not give an agent keychain, before any `security` call: it
/// must be its own canonical path, so no link sits anywhere on it, lie strictly below the
/// canonical engine-owned `root`, not be the owner's own home, and hold no keychain name a
/// profile home or a real login home has. Inputs: the root the engine keeps agent homes
/// under and the home. Output: done, or a failure.
fn agent_home_guard(root: &Path, home: &Path) -> Result<(), StoreError> {
    if !root.is_absolute() || !home.is_absolute() {
        return Err(failure());
    }
    let root = std::fs::canonicalize(root).map_err(|_| failure())?;
    if std::fs::canonicalize(home).map_err(|_| failure())? != home
        || !home.starts_with(&root)
        || home == root
        || is_owner_home(home)?
    {
        return Err(failure());
    }
    for name in [LOGIN_KEYCHAIN, PROFILE_KEYCHAIN, ASIDE_KEYCHAIN] {
        match std::fs::symlink_metadata(home.join("Library/Keychains").join(name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(failure()),
        }
    }
    assert!(home.starts_with(&root) && home != root);
    Ok(())
}

/// The agent keychains an earlier launch left in a private home's keychain folder, read
/// without following a link; a keychain under any other name stays where it is. Input: the
/// folder, already checked as a real folder or absent. Output: their paths, or a failure
/// for an agent keychain name on a link, a hard-linked file or any other entry, or for a
/// folder of more than 64 entries.
fn earlier_keychains(directory: &Path) -> Result<Vec<PathBuf>, StoreError> {
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
    assert!(earlier.len() <= FOLDER_ENTRIES_MAX);
    Ok(earlier)
}

/// Makes the `Library` and `Library/Preferences` folders a home's keychain settings live in,
/// refusing a link at either, at `Library/Keychains` or at the settings file. Input: the
/// home. Output: done, or a failure.
fn settings_folders(home: &Path) -> Result<(), StoreError> {
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
fn point_at(home: &Path, keychain: &Path) -> Result<(), StoreError> {
    for setting in ["default-keychain", "list-keychains"] {
        set_setting(home, setting, keychain)?;
    }
    Ok(())
}

/// Sets one of a home's own keychain settings, `default-keychain` or `list-keychains`, to
/// `keychain` alone, in that home's user domain. Inputs: the home, the setting and the
/// keychain. Output: done, or a failure.
fn set_setting(home: &Path, setting: &str, keychain: &Path) -> Result<(), StoreError> {
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
fn discard(home: &Path, keychain: &Path) -> Result<(), StoreError> {
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
/// keychain may be locked, as every keychain is after a restart; nothing in it outlives a
/// turn, so it is deleted unread. The new keychain is unlocked with its empty password and
/// locks neither on a timeout nor on sleep. Only `agent-<16 hex>.keychain-db`
/// files are ever deleted. Inputs: the canonical root the engine keeps agent homes under,
/// the home and the homes that share its keychain. Output: the keychain path, or a failure,
/// with nothing written when a home fails [`agent_home_guard`] or a folder or keychain there
/// is a link.
pub fn fresh_profile_keychain(
    root: &Path,
    home: &Path,
    sharers: &[&Path],
) -> Result<PathBuf, StoreError> {
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
    let settled =
        unlock(home, &keychain).and_then(|()| set_setting(home, "default-keychain", &keychain));
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
/// of a stale copy. The keychain is named in each call, unlocked first and no link is
/// followed to it; a keychain or an item already gone is fine. Inputs: the absolute home,
/// the agent keychain file in its keychain folder and the service. Output: done, or a
/// failure, also for a keychain outside that folder or under a name it never gets.
pub fn forget_record(home: &Path, keychain: &Path, service: &str) -> Result<(), StoreError> {
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
    unlock(home, keychain)?;
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
mod tests;
