#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::owner::{OwnerClient, OwnerClientKind};
use crazyagent::harnesses::HarnessStatus;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_crazyagent");
const READY_WAIT: Duration = Duration::from_secs(60);
const MASTER_ONLY: &str = "runs only as a CrazyAgent master";

/// The verified RCLI host the release bundles beside the engine. Takes nothing. Returns
/// its absolute path; an unset or missing host stops the test as unavailable.
fn rcli_bundle() -> PathBuf {
    let bundle = std::env::var_os("CRAZYAGENT_RCLI")
        .map(PathBuf::from)
        .expect("UNAVAILABLE: set CRAZYAGENT_RCLI to the verified crazyagent-rcli bundle");
    assert!(
        bundle.is_absolute() && bundle.is_file(),
        "UNAVAILABLE: {bundle:?}"
    );
    bundle
}

/// An engine process the test started, killed and reaped when the test ends.
struct Engine(Child);

impl Drop for Engine {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}

/// Starts the engine at `executable` with its own home and a PATH that holds no RCLI host,
/// and waits for its ready line. Takes the executable, the engine home and the owner home.
/// Returns the running engine.
fn start(executable: &Path, home: &Path, owner: &Path) -> Engine {
    let mut child = crazyagent::spawn_gate::spawn(
        Command::new(executable)
            .arg("engine")
            .arg("--home")
            .arg(home)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", owner)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let stdout = child.stdout.take().unwrap();
    let engine = Engine(child);
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = send.send(read);
    });
    let line = receive.recv_timeout(READY_WAIT).unwrap().unwrap();
    assert_eq!(line.trim(), "ready");
    engine
}

/// The installed engine's folder is not on its PATH, yet the RCLI host bundled beside its
/// executable is listed as found at that path, with the master-only limit every found RCLI
/// has, to the TUI and desktop owners and to the TUI controller the setup screen reads
/// (fix/rcli-catalog-bundled 8da15004).
#[tokio::test(flavor = "multi_thread")]
async fn a_bundled_rcli_beside_the_engine_is_listed_without_path() {
    let bundle = rcli_bundle();
    let temp = tempfile::Builder::new()
        .prefix("ca-rcli-beside-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = temp.path().canonicalize().unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (bin, home, owner) = (base.join("bin"), base.join("home"), base.join("owner"));
    for folder in [&bin, &owner] {
        std::fs::create_dir(folder).unwrap();
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let (engine, host) = (bin.join("crazyagent"), bin.join("crazyagent-rcli"));
    std::fs::hard_link(BIN, &engine)
        .expect("the built engine is linked into the bundle folder on the same volume");
    std::fs::hard_link(&bundle, &host)
        .expect("the RCLI host is linked into the bundle folder on the same volume");
    let _running = start(&engine, &home, &owner);
    for kind in [OwnerClientKind::Tui, OwnerClientKind::Desktop] {
        let (client, _) = OwnerClient::discover(&home, kind, Duration::from_secs(5))
            .await
            .unwrap();
        let entries = client
            .harness_catalog(kind == OwnerClientKind::Desktop)
            .await
            .unwrap();
        let rcli = entries.iter().find(|entry| entry.id == "rcli").unwrap();
        assert_eq!(
            rcli.status,
            HarnessStatus::Limited {
                reason: MASTER_ONLY.into()
            },
            "{kind:?} {rcli:?}"
        );
        assert_eq!(
            rcli.status_label,
            format!("Limited control: {MASTER_ONLY}"),
            "{kind:?}"
        );
        assert_eq!(rcli.executable.as_deref(), Some(host.as_path()), "{kind:?}");
    }
    let controller = OwnerController::attach(&home, OwnerClientKind::Tui, Duration::from_secs(5))
        .await
        .unwrap();
    let entries = controller.harness_catalog(false).await.unwrap();
    let rcli = entries.iter().find(|entry| entry.id == "rcli").unwrap();
    assert_eq!(
        (rcli.status_label.clone(), rcli.executable.as_deref()),
        (
            format!("Limited control: {MASTER_ONLY}"),
            Some(host.as_path())
        )
    );
}
