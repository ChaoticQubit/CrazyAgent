use crate::keychain::{agent_keychain_name, forget_record};
use crate::perf_kit::{Row, SAMPLES, time};
use crate::secret::SecretString;
use crate::store::macos::{Keychain, owned_namespace};
use crate::store::{Store, reference};
use std::hint::black_box;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FAST: usize = 1000;
const FILES: usize = 10;
const ONE: usize = 1;
const ONE_SHOT: usize = SAMPLES / 10 + SAMPLES;
const SECURITY: &str = "/usr/bin/security";
const NAMESPACE: &str = "dev.crazyagent.e0123456789ab";
const SERVICE: &str = "dev.crazyagent.e0123456789ab.perf";
const AGENT_KEYCHAIN: &str = "agent-0123456789abcdef.keychain-db";
const PROFILE_KEYCHAIN: &str = "profile.keychain-db";
const SECRET: &[u8] = b"ghp_owner-paste-0123456789abcdef0123";

fn row(name: &'static str, batch: usize) -> Row {
    Row {
        layer: 1,
        module: "security",
        name,
        batch,
    }
}

/// A keychain file the rows make in a private temporary home and delete on drop: the
/// home's default and only search-list keychain, set with only `HOME` pointing at that
/// home, so the user's own keychains are never named or changed.
struct Scratch {
    home: PathBuf,
    path: PathBuf,
}

impl Scratch {
    /// Makes the keychain `name` with an empty password in `home`. Inputs: the home and the
    /// file name. Output: the scratch keychain.
    fn create(home: &Path, name: &str) -> Self {
        let folder = home.join("Library/Keychains");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(home.join("Library/Preferences")).unwrap();
        for private in [
            home.to_path_buf(),
            home.join("Library"),
            folder.clone(),
            home.join("Library/Preferences"),
        ] {
            std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let scratch = Self {
            home: home.to_path_buf(),
            path: folder.join(name),
        };
        let path = scratch.text().to_owned();
        scratch.security(&["create-keychain", "-p", "", &path]);
        scratch.security(&["list-keychains", "-d", "user", "-s", &path]);
        scratch.security(&["default-keychain", "-d", "user", "-s", &path]);
        scratch
    }

    fn text(&self) -> &str {
        self.path.to_str().unwrap()
    }

    /// Runs `security` on this keychain with only `HOME` and `PATH` set and asserts it
    /// succeeds. Input: the arguments, which name this keychain. Output: none.
    fn security(&self, args: &[&str]) {
        assert!(args.contains(&self.text()));
        let ran = kernel_processes::spawn_gate::output(
            Command::new(SECURITY)
                .args(args)
                .env_clear()
                .env("HOME", &self.home)
                .env("PATH", "/usr/bin:/bin")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert!(ran.status.success(), "security {:?} failed", args.first());
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let path = self.text().to_owned();
        self.security(&["delete-keychain", &path]);
    }
}

/// Times the macOS keychain calls that touch only a scratch keychain in a temporary home:
/// the name checks, the store opened at that keychain and in that home, its writes, reads
/// and deletes, and one agent keychain record removal. `open_profile_keychain`,
/// `prepare_profile_keychain` and `fresh_profile_keychain` get no row here (layer 3).
/// Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn keychain_calls() {
    let root = tempfile::tempdir().unwrap();
    let home = std::fs::canonicalize(root.path()).unwrap().join("home");
    let scratch = Scratch::create(&home, PROFILE_KEYCHAIN);
    let agent = Scratch::create(&home.join("agent"), AGENT_KEYCHAIN);
    let store = Keychain::at(NAMESPACE, &scratch.path).unwrap();
    let secret = SecretString::new(SECRET.to_vec()).unwrap();
    let found = store.put(SERVICE, "token", &secret).unwrap();
    assert_eq!(store.get(&found).unwrap().expose().as_bytes(), SECRET);
    assert_eq!(
        Keychain::in_home(NAMESPACE, &home).unwrap().path(),
        scratch.path
    );
    assert!(agent_keychain_name(AGENT_KEYCHAIN) && owned_namespace(NAMESPACE).is_ok());
    forget_record(&agent.home, &agent.path, SERVICE).unwrap();
    time(&row("keychain.agent_keychain_name", FAST), SAMPLES, || {
        black_box(agent_keychain_name(black_box(AGENT_KEYCHAIN)));
    });
    time(&row("store.macos.owned_namespace", FAST), SAMPLES, || {
        black_box(owned_namespace(black_box(NAMESPACE)).unwrap());
    });
    time(&row("store.macos.Keychain.at", FILES), SAMPLES, || {
        black_box(Keychain::at(NAMESPACE, black_box(&scratch.path)).unwrap());
    });
    time(&row("store.macos.Keychain.path", FAST), SAMPLES, || {
        black_box(black_box(&store).path());
    });
    time(&row("store.macos.Keychain.in_home", ONE), SAMPLES, || {
        black_box(Keychain::in_home(NAMESPACE, black_box(&home)).unwrap());
    });
    time(&row("store.macos.Keychain.put", ONE), SAMPLES, || {
        black_box(store.put(SERVICE, "token", &secret).unwrap());
    });
    time(&row("store.macos.Keychain.get", ONE), SAMPLES, || {
        black_box(store.get(&found).unwrap());
    });
    let accounts: Vec<_> = (0..ONE_SHOT)
        .map(|at| store.put(SERVICE, &format!("gone-{at}"), &secret).unwrap())
        .collect();
    let mut next = accounts.iter();
    time(&row("store.macos.Keychain.delete", ONE), SAMPLES, || {
        store.delete(next.next().unwrap()).unwrap();
    });
    assert!(next.next().is_none());
    time(
        &row("store.macos.Keychain.git_credential_helper", FAST),
        SAMPLES,
        || {
            black_box(store.git_credential_helper(&found, "perf").unwrap());
        },
    );
    time(&row("keychain.forget_record", ONE), SAMPLES, || {
        forget_record(&agent.home, &agent.path, SERVICE).unwrap();
    });
    assert_eq!(reference(SERVICE, "token").unwrap(), found);
}
