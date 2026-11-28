use crate::keychain::{agent_keychain_name, forget_record};
use crate::perf_kit::{Row, SAMPLES, time};
use crate::secret::SecretString;
use crate::store::macos::{Keychain, owned_namespace};
use crate::store::{Store, reference};
use std::hint::black_box;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
    /// file name. Output: the scratch keychain, or the folder or `security` step that failed.
    fn create(home: &Path, name: &str) -> Result<Self, String> {
        let folder = home.join("Library/Keychains");
        for private in [
            home.to_path_buf(),
            home.join("Library"),
            folder.clone(),
            home.join("Library/Preferences"),
        ] {
            std::fs::create_dir_all(&private)
                .and_then(|()| {
                    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700))
                })
                .map_err(|error| format!("{}: {error}", private.display()))?;
        }
        let scratch = Self {
            home: home.to_path_buf(),
            path: folder.join(name),
        };
        let path = scratch.text().to_owned();
        scratch.security(&["create-keychain", "-p", "", &path])?;
        scratch.security(&["list-keychains", "-d", "user", "-s", &path])?;
        scratch.security(&["default-keychain", "-d", "user", "-s", &path])?;
        Ok(scratch)
    }

    fn text(&self) -> &str {
        self.path.to_str().unwrap()
    }

    /// Runs `security` on this keychain with only `HOME` and `PATH` set. Input: the
    /// arguments, which name this keychain. Output: Ok once it succeeds, else the step and
    /// why it failed.
    fn security(&self, args: &[&str]) -> Result<(), String> {
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
        .map_err(|error| format!("security {:?}: {error}", args.first()))?;
        if ran.status.success() {
            Ok(())
        } else {
            Err(format!(
                "security {:?} failed: {}",
                args.first(),
                ran.status
            ))
        }
    }
}

impl Drop for Scratch {
    /// Deletes the keychain if `create` made its file, and reports a failed delete instead of
    /// panicking, so a row already failing is never aborted by a second panic.
    fn drop(&mut self) {
        if !self.path.exists() {
            return;
        }
        let path = self.text().to_owned();
        if let Err(error) = self.security(&["delete-keychain", &path]) {
            eprintln!("scratch keychain {path} was not deleted: {error}");
        }
    }
}

/// Runs one bare `security -h`, which reads no keychain, the way the store runs `security`:
/// the spawn gate, a reader thread on its output of at most 4096 bytes and a `try_wait` poll
/// every 10 ms, so its row times the spawn, reader and poll alone. Panics when the child
/// fails or outlives its 10 s deadline.
fn security_spawn() {
    let mut command = Command::new(SECURITY);
    command
        .arg("-h")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = kernel_processes::spawn_gate::spawn(&mut command).unwrap();
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || crate::read_limited::read_limited(stdout, 4096));
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "security -h outlived its deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let (read, bytes) = reader.join().unwrap();
    assert!(status.success() && read.is_ok(), "security -h failed");
    black_box(bytes);
}

/// Times the macOS keychain calls that touch only a scratch keychain in a temporary home
/// under `/tmp/ca-keychain-*`, so its path is as long as the twin build's: the name checks,
/// the store opened at that keychain and in that home, its writes, reads and deletes, and
/// one agent keychain record removal. The profile keychain calls get no row (layer 3).
/// Prints one PERF line per call and asserts no time.
#[test]
#[ignore = "timing row: run through scripts/perf/run.sh"]
fn keychain_calls() {
    let root = tempfile::Builder::new()
        .prefix("ca-keychain-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let home = root.path().join("home");
    let scratch =
        Scratch::create(&home, PROFILE_KEYCHAIN).unwrap_or_else(|error| panic!("{error}"));
    let agent = Scratch::create(&home.join("agent"), AGENT_KEYCHAIN)
        .unwrap_or_else(|error| panic!("{error}"));
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
    security_spawn();
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
    time(
        &row("store.macos.security_spawn", ONE),
        SAMPLES,
        security_spawn,
    );
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
