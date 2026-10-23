use crate::engine::EngineError;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SECURITY: &str = "/usr/bin/security";
const STEP_TIME: Duration = Duration::from_secs(10);

fn failure() -> EngineError {
    EngineError("native_auth_claude_keychain_unavailable".into())
}

fn security(home: &Path, args: &[&std::ffi::OsStr]) -> Result<(), EngineError> {
    let mut child = Command::new(SECURITY)
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
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

/// Gives a private Claude profile home its own unlocked macOS keychain, so the official CLI
/// stores and reads its token there instead of failing on the absent login keychain.
/// Input: the absolute private profile home. Output: the keychain path, set as that home's
/// default and only search-list keychain; the owner's real keychain settings are untouched.
pub(super) fn prepare_profile_keychain(home: &Path) -> Result<PathBuf, EngineError> {
    if !home.is_absolute() || !home.is_dir() {
        return Err(failure());
    }
    let directory = home.join("Library/Keychains");
    std::fs::create_dir_all(&directory).map_err(|_| failure())?;
    let keychain = directory.join("login.keychain-db");
    let path = keychain.as_os_str();
    let empty = std::ffi::OsStr::new("");
    if !keychain.exists() {
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
        let output = Command::new(SECURITY)
            .args(args)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .output()
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
            .tempdir_in("/private/tmp")
            .unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let keychain = prepare_profile_keychain(&home).unwrap();
        assert_eq!(keychain, home.join("Library/Keychains/login.keychain-db"));
        assert!(
            read(&home, &["default-keychain", "-d", "user"]).contains(keychain.to_str().unwrap())
        );
        let stored = Command::new(SECURITY)
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
            .env("PATH", "/usr/bin:/bin")
            .status()
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
}
