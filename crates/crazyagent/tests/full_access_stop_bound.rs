#![cfg(target_os = "macos")]

use crazyagent::harnesses::launch::{AccountBinding, LaunchLimits, LaunchRequest, Session};
use crazyagent::harnesses::leftovers::Unstopped;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const STAND_IN: &str = r#"#!/bin/sh
[ "$1" = first-exec ] && exit 3
echo $$ > leader.pid
exec /bin/sleep 30
"#;

fn private_folder(parent: &Path, name: &str) -> PathBuf {
    let folder = parent.join(name);
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::canonicalize(folder).unwrap()
}

/// A stop under a held-up process listing ends within one listing wait plus one settle wait,
/// far inside the 6 s the listing's start is held. When the run's first listing finished
/// before the hold, the stop knows the run's processes, waits out its own held-up listing
/// (1.1 s) and the settle wait (2.1 s), and names them with the unread list; when it did
/// not, nothing is known and the stop ends after the one listing, unchecked. Both are told
/// as a process list that could not be read.
#[test]
fn a_full_access_stop_whose_process_listing_cannot_start_ends_within_its_bound() {
    let root = tempfile::Builder::new()
        .prefix("crazyagent-listing-bound-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let project = private_folder(root.path(), "project");
    let home = private_folder(root.path(), "home");
    let bin = private_folder(root.path(), "bin");
    let harness = bin.join("claude");
    std::fs::write(&harness, STAND_IN).unwrap();
    std::fs::set_permissions(&harness, std::fs::Permissions::from_mode(0o755)).unwrap();
    let first = crazyagent::spawn_gate::status(
        std::process::Command::new(&harness)
            .arg("first-exec")
            .current_dir(&bin)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert_eq!(first.code(), Some(3));
    let session = Session::start(LaunchRequest {
        harness: "claude-code".into(),
        executable: harness,
        working_dir: project.clone(),
        account: AccountBinding::Subscription { home },
        model: None,
        tools: false,
        sandbox_profile: None,
        unconfined: true,
        git_identity: None,
        limits: LaunchLimits::default(),
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !project.join("leader.pid").exists() {
        assert!(Instant::now() < deadline, "stand-in agent never started");
        std::thread::sleep(Duration::from_millis(10));
    }

    let (entered, gate_held) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        crazyagent::spawn_gate::guarded(|| {
            entered.send(()).unwrap();
            std::thread::sleep(Duration::from_secs(6));
        });
    });
    gate_held.recv().unwrap();
    let started = Instant::now();
    let stopped = session.stop();
    let took = started.elapsed();
    holder.join().unwrap();

    assert!(
        took < Duration::from_millis(3800),
        "a process listing whose start was held up for 6 s held the stop for {took:?}; a stop waits out one held-up listing and one settle wait at most"
    );
    match stopped {
        Err(Unstopped::Left(left)) => assert!(
            left.names().ends_with("the process list could not be read"),
            "{} ({})",
            left.plain(),
            left.code()
        ),
        other => panic!(
            "a stop whose process listing never ran was reported {other:?} instead of unchecked"
        ),
    }
}
