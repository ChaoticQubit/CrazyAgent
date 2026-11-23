#![cfg(unix)]

mod poll_support;

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// An engine start refused because a restore did not finish is a deliberate stop: it exits 0,
/// so neither launchd (KeepAlive SuccessfulExit false) nor systemd (Restart on-failure) starts
/// it again in a loop, and it still says why on stderr.
#[test]
fn an_engine_start_refused_by_a_broken_restore_exits_0_and_names_the_refusal() {
    for text in [
        "failed storage_restore_rollback_failed replaced-20261117T001500000Z",
        "restoring",
    ] {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut record = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.path().join("restore-record"))
            .unwrap();
        std::io::Write::write_all(&mut record, text.as_bytes()).unwrap();
        drop(record);

        let mut child = kernel_processes::spawn_gate::spawn(
            std::process::Command::new(env!("CARGO_BIN_EXE_crazyagent"))
                .arg("engine")
                .arg(root.path())
                .arg(root.path().join("global.toml"))
                .arg(root.path().join("project.toml"))
                .arg("broken.sock")
                .stdout(Stdio::null())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        let mut status = None;
        let stopped = poll_support::poll_until(Instant::now() + Duration::from_secs(30), || {
            status = child.try_wait().unwrap();
            status.is_some()
        });
        if !stopped {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("{text}: the refused engine did not stop");
        }
        let status = status.unwrap();
        let mut said = String::new();
        std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut said).unwrap();

        assert!(said.contains("engine_restore_broken"), "{text}: {said}");
        assert_eq!(status.code(), Some(0), "{text}: {said}");
    }
}
