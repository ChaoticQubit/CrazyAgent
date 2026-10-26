#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const SECURITY: &str = "/usr/bin/security";

/// An integration test's own keychain file inside a temporary home: the home's default and
/// only search-list keychain, named in every call the test makes, and deleted on drop. It is
/// never the owner's login keychain, and no other home ever sees it.
pub struct TemporaryKeychain {
    home: PathBuf,
    path: PathBuf,
    _root: tempfile::TempDir,
}

impl TemporaryKeychain {
    /// Creates an unlocked keychain at `<home>/Library/Keychains/login.keychain-db` in a new
    /// temporary home and makes it that home's default and only search-list keychain, which
    /// `security` keeps per home. Output: the keychain.
    pub fn create() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-keychain-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let home = root.path().join("home");
        let folder = home.join("Library/Keychains");
        std::fs::create_dir_all(&folder).unwrap();
        for private in [home.clone(), home.join("Library"), folder.clone()] {
            std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let keychain = Self {
            home,
            path: folder.join("login.keychain-db"),
            _root: root,
        };
        let text = keychain.text().to_owned();
        for args in [
            vec!["create-keychain", "-p", "", &text],
            vec!["list-keychains", "-d", "user", "-s", &text],
            vec!["default-keychain", "-d", "user", "-s", &text],
        ] {
            assert!(
                keychain.run(&args).status.success(),
                "security {:?} failed",
                args.first()
            );
        }
        keychain
    }

    /// The temporary home whose default keychain this is, for a process that names none.
    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn text(&self) -> &str {
        self.path.to_str().expect("the keychain path is text")
    }

    fn run(&self, args: &[&str]) -> Output {
        assert!(
            args.contains(&self.text()),
            "security {:?} names no keychain; a test may only use its own",
            args.first()
        );
        crazyagent::spawn_gate::output(
            Command::new(SECURITY)
                .args(args)
                .env_clear()
                .env("HOME", &self.home)
                .env("PATH", "/usr/bin:/bin")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap()
    }
}

impl Drop for TemporaryKeychain {
    fn drop(&mut self) {
        let path = self.text().to_owned();
        let deleted = self.run(&["delete-keychain", &path]);
        if !std::thread::panicking() {
            assert!(
                deleted.status.success(),
                "the test keychain was not deleted"
            );
            assert!(!self.path.exists());
        }
    }
}
