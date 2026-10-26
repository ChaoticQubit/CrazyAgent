use crate::engine::EngineError;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SECURITY: &str = "/usr/bin/security";
const STEP_TIME: Duration = Duration::from_secs(10);

fn failure() -> EngineError {
    EngineError("native_auth_claude_keychain_unavailable".into())
}

fn security(home: &Path, args: &[&std::ffi::OsStr]) -> Result<(), EngineError> {
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
            return if status.success() {
                Ok(())
            } else {
                Err(failure())
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(failure());
        }
        std::thread::sleep(Duration::from_millis(20));
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
/// stores and reads its token there instead of failing on the absent login keychain.
/// Input: the absolute private profile home. Output: the keychain path, set as that home's
/// default and only search-list keychain; the owner's real keychain settings are untouched.
pub(super) fn prepare_profile_keychain(home: &Path) -> Result<PathBuf, EngineError> {
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn private_home_gets_its_own_keychain_and_the_owner_keychain_is_untouched() {
        let real_home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let real_default = read(&real_home, &["default-keychain", "-d", "user"]);
        let root = tempfile::Builder::new()
            .prefix("ca-keychain-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let keychain = prepare_profile_keychain(&home).unwrap();
        assert_eq!(keychain, home.join("Library/Keychains/login.keychain-db"));
        assert!(
            read(&home, &["default-keychain", "-d", "user"]).contains(keychain.to_str().unwrap())
        );
        let stored = crate::spawn_gate::status(
            Command::new(SECURITY)
                .args([
                    "add-generic-password",
                    "-U",
                    "-a",
                    "probe",
                    "-s",
                    "crazyagent-keychain-test",
                    "-w",
                    "value",
                ])
                .env_clear()
                .env("HOME", &home)
                .env("PATH", "/usr/bin:/bin"),
        )
        .unwrap();
        assert!(stored.success());
        assert_eq!(
            read(
                &home,
                &[
                    "find-generic-password",
                    "-a",
                    "probe",
                    "-s",
                    "crazyagent-keychain-test",
                    "-w"
                ]
            )
            .trim(),
            "value"
        );
        assert_eq!(prepare_profile_keychain(&home).unwrap(), keychain);
        assert_eq!(
            read(
                &home,
                &[
                    "find-generic-password",
                    "-a",
                    "probe",
                    "-s",
                    "crazyagent-keychain-test",
                    "-w"
                ]
            )
            .trim(),
            "value"
        );
        assert_eq!(
            read(&real_home, &["default-keychain", "-d", "user"]),
            real_default
        );
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
    fn keychain_folders_a_sandboxed_agent_links_elsewhere_are_refused_untouched() {
        use std::os::unix::fs::PermissionsExt;
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
        agent.shell(&format!(
            "mkdir '{home}/Library' && ln -s '{preferences}' '{home}/Library/Preferences' \
             && ln -s '{keychains}' '{home}/Library/Keychains'",
            home = home.display(),
            preferences = preferences.display(),
            keychains = keychains.display()
        ));

        assert!(prepare_profile_keychain(&home).is_err());
        assert_eq!(entries(&preferences), Vec::<String>::new());
        assert_eq!(entries(&keychains), Vec::<String>::new());

        let planted = agent.peer.join("keychain");
        std::fs::write(&planted, "peer").unwrap();
        std::fs::set_permissions(
            &planted,
            std::os::unix::fs::PermissionsExt::from_mode(0o644),
        )
        .unwrap();
        agent.shell(&format!(
            "cd '{home}/Library' && rm Preferences Keychains && mkdir Keychains \
             && ln -s '{planted}' Keychains/login.keychain-db",
            home = home.display(),
            planted = planted.display()
        ));
        assert!(prepare_profile_keychain(&home).is_err());
        assert_eq!(std::fs::read_to_string(&planted).unwrap(), "peer");
        assert_eq!(
            std::fs::metadata(&planted).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(entries(&home.join("Library")), ["Keychains"]);
    }
}
