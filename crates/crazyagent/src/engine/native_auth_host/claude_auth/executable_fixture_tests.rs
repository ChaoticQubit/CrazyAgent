use super::super::RegisteredNativeAuthHost;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn registered_cat_fixture_reports_actual_exit_and_stderr() {
    let root = tempfile::Builder::new()
        .prefix("ca-claude-copied-executable-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let executable = root.path().join("registered-cat");
    fs::write(&executable, b"#!/bin/sh\nexec /bin/cat\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let registered = RegisteredNativeAuthHost::capture(&executable).unwrap();
    let digest = registered.verified_sha256().unwrap();
    let mut child = crate::spawn_gate::spawn(
        Command::new(registered.verified_path().unwrap())
            .env_clear()
            .current_dir(&home)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0),
    )
    .unwrap();
    let initial = child.try_wait().unwrap();
    drop(child.stdin.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let status = child.wait().unwrap();
            panic!("copied executable exceeded two seconds: {status:?}");
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .take(4097)
        .read_to_end(&mut stderr)
        .unwrap();
    assert!(stderr.len() <= 4096);
    eprintln!(
        "copied executable initial={initial:?} code={:?} signal={:?} stderr={:?}",
        status.code(),
        status.signal(),
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(registered.verified_sha256().unwrap(), digest);
    assert!(
        status.success(),
        "copied executable fixture failed: {status:?}"
    );
}
