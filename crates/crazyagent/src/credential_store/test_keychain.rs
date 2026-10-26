use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, PoisonError};

const SECURITY: &str = "/usr/bin/security";

static SERVED: Mutex<BTreeMap<String, PathBuf>> = Mutex::new(BTreeMap::new());

/// A lib test's own keychain file inside a temporary home: the home's default and only
/// search-list keychain, named in every call the test makes, and deleted on drop. It is
/// never the owner's login keychain, and no other home ever sees it.
pub(crate) struct TestKeychain {
    home: PathBuf,
    path: PathBuf,
    served: Vec<String>,
    _root: Option<tempfile::TempDir>,
}

impl TestKeychain {
    /// Creates a keychain in a temporary home of its own. Output: the keychain, whose home
    /// is removed after the keychain is deleted.
    pub(crate) fn temporary() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-keychain-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut keychain = Self::create(&home);
        keychain._root = Some(root);
        keychain
    }

    /// Creates an unlocked keychain at `<home>/Library/Keychains/login.keychain-db` and
    /// makes it the home's default and only search-list keychain, which `security` keeps
    /// per home. Input: an absolute temporary home. Output: the keychain.
    pub(crate) fn create(home: &Path) -> Self {
        let folder = home.join("Library/Keychains");
        std::fs::create_dir_all(&folder).unwrap();
        for private in [home.join("Library"), folder.clone()] {
            std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let keychain = Self::adopt(home, folder.join("login.keychain-db"));
        let path = keychain.text().to_owned();
        keychain.security(&["create-keychain", "-p", "", &path], None);
        keychain.security(&["list-keychains", "-d", "user", "-s", &path], None);
        keychain.security(&["default-keychain", "-d", "user", "-s", &path], None);
        keychain
    }

    /// Takes charge of a keychain file that already sits in a temporary home, such as one
    /// the sign-in custodian created, so the test names it in every call and deletes it on
    /// drop. Inputs: the home and the keychain file inside it. Output: the keychain.
    pub(crate) fn adopt(home: &Path, path: PathBuf) -> Self {
        assert!(home.is_absolute());
        assert!(path.starts_with(home));
        assert!(path.to_str().is_some_and(|text| !text.contains('"')));
        let owner = std::env::var_os("HOME").map(PathBuf::from);
        assert!(
            owner.is_none_or(|owner| !path.starts_with(owner.join("Library"))),
            "a test keychain must never be in the owner's Library"
        );
        Self {
            home: home.to_path_buf(),
            path,
            served: Vec::new(),
            _root: None,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn text(&self) -> &str {
        self.path
            .to_str()
            .expect("the keychain path was checked as text")
    }

    fn run(&self, args: &[&str], input: Option<&str>) -> Output {
        let mut child = crate::spawn_gate::spawn(
            Command::new(SECURITY)
                .args(args)
                .env_clear()
                .env("HOME", &self.home)
                .env("PATH", "/usr/bin:/bin")
                .current_dir(&self.home)
                .stdin(if input.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        if let Some(input) = input {
            use std::io::Write as _;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        child.wait_with_output().unwrap()
    }

    /// Runs `security` against this keychain only, with `HOME` set to its temporary home.
    /// Inputs: the arguments and optional stdin; one of them must name the keychain file.
    /// Output: stdout, or a panic when the call names no keychain or fails.
    pub(crate) fn security(&self, args: &[&str], input: Option<&str>) -> String {
        let quoted = format!("\"{}\"", self.text());
        assert!(
            args.contains(&self.text()) || input.is_some_and(|input| input.contains(&quoted)),
            "security {:?} names no keychain; a test may only use its own",
            args.first()
        );
        let output = self.run(args, input);
        assert!(
            output.status.success(),
            "security {:?} failed",
            args.first()
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// Stores a record in this keychain through stdin, as hex, so no value appears in a
    /// process argument. Inputs: the account, the service and the record text.
    pub(crate) fn add(&self, account: &str, service: &str, record: &str) {
        let hex: String = record.bytes().map(|byte| format!("{byte:02x}")).collect();
        let line = format!(
            "add-generic-password -U -a \"{account}\" -s \"{service}\" -X {hex} \"{}\"\n",
            self.text()
        );
        self.security(&["-i"], Some(&line));
    }

    /// Reads one record from this keychain. Input: the service. Output: the record text.
    pub(crate) fn find(&self, service: &str) -> String {
        let path = self.text().to_owned();
        let found = self.security(&["find-generic-password", "-s", service, "-w", &path], None);
        found.trim_end().to_owned()
    }

    /// Lets the credential store open `namespace` in lib tests through this keychain's
    /// home, the way the engine opens the owner's default keychain. Input: the namespace.
    pub(crate) fn serve(&mut self, namespace: &str) {
        let mut served = SERVED.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(
            served
                .insert(namespace.to_owned(), self.home.clone())
                .is_none()
        );
        self.served.push(namespace.to_owned());
    }
}

impl Drop for TestKeychain {
    fn drop(&mut self) {
        let mut served = SERVED.lock().unwrap_or_else(PoisonError::into_inner);
        for namespace in &self.served {
            served.remove(namespace);
        }
        drop(served);
        let path = self.text().to_owned();
        let deleted = self.run(&["delete-keychain", &path], None);
        if !std::thread::panicking() {
            assert!(
                deleted.status.success(),
                "the test keychain was not deleted"
            );
            assert!(!self.path.exists());
        }
    }
}

/// The temporary home a lib test registered for a credential-store namespace. Input: the
/// namespace. Output: the home, or none when no test keychain serves it.
pub(crate) fn home_for(namespace: &str) -> Option<PathBuf> {
    let served = SERVED.lock().unwrap_or_else(PoisonError::into_inner);
    served.get(namespace).cloned()
}

mod tests {
    use super::*;

    fn preference(home: &Path, args: &[&str]) -> String {
        let output = crate::spawn_gate::output(
            Command::new(SECURITY)
                .args(args)
                .env_clear()
                .env("HOME", home)
                .env("PATH", "/usr/bin:/bin")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn a_call_that_names_no_keychain_is_refused() {
        let keychain = TestKeychain::temporary();
        keychain.add("unit", "crazyagent-test-keychain", "value");
        for args in [
            vec![
                "find-generic-password",
                "-s",
                "crazyagent-test-keychain",
                "-w",
            ],
            vec!["dump-keychain"],
            vec!["list-keychains", "-d", "user"],
        ] {
            let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                keychain.security(&args, None)
            }));
            assert!(refused.is_err(), "{args:?} ran without naming the keychain");
        }
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            keychain.security(
                &["-i"],
                Some("find-generic-password -s crazyagent-test-keychain -w\n"),
            )
        }));
        assert!(refused.is_err());
        assert_eq!(keychain.find("crazyagent-test-keychain"), "value");
    }

    #[test]
    fn a_keychain_stays_inside_its_own_home_and_is_deleted_on_drop() {
        let keychain = TestKeychain::temporary();
        let other = keychain.home.parent().unwrap().join("other");
        std::fs::create_dir(&other).unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = keychain.path().to_path_buf();
        let text = path.to_str().unwrap().to_owned();
        keychain.add("unit", "crazyagent-test-keychain", "value");
        assert_eq!(keychain.find("crazyagent-test-keychain"), "value");
        assert!(preference(&keychain.home, &["default-keychain", "-d", "user"]).contains(&text));
        assert!(preference(&keychain.home, &["list-keychains", "-d", "user"]).contains(&text));
        for args in [
            ["default-keychain", "-d", "user"],
            ["list-keychains", "-d", "user"],
        ] {
            assert!(
                !preference(&other, &args).contains(&text),
                "another home sees the test keychain through {args:?}"
            );
        }
        let home = keychain.home.clone();
        drop(keychain);
        assert!(!path.exists());
        assert!(!preference(&home, &["list-keychains", "-d", "user"]).contains(&text));
    }
}
