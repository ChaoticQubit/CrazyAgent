use super::super::RegisteredNativeAuthHost;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const FIRST_EXEC_TIME: Duration = Duration::from_secs(60);

/// Waits until a freshly written cat fixture echoes one line, so it is running cat and the
/// caller times only its exit after input ends. macOS assesses each new script once after
/// spawn returns, one at a time across the machine, so that wait has its own bound.
/// Inputs: the child's input and output pipes. Output: none; panics past FIRST_EXEC_TIME.
pub(super) fn await_fresh_cat(input: &mut ChildStdin, output: ChildStdout) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut output = output;
        let mut echoed = [0; 6];
        let _ = sender.send(output.read_exact(&mut echoed).map(|()| echoed));
    });
    input.write_all(b"ready\n").unwrap();
    let echoed = receiver
        .recv_timeout(FIRST_EXEC_TIME)
        .expect("fresh cat fixture did not start within its first-exec bound")
        .unwrap();
    reader.join().unwrap();
    assert_eq!(&echoed, b"ready\n");
}

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
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0),
    )
    .unwrap();
    let initial = child.try_wait().unwrap();
    let mut input = child.stdin.take().unwrap();
    await_fresh_cat(&mut input, child.stdout.take().unwrap());
    drop(input);
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
