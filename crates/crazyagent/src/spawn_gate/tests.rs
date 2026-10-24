use super::{guarded, output, spawn, status};
use std::process::{Command, Stdio};

#[test]
#[should_panic(expected = "spawn_gate entered again on a thread that already holds it")]
fn entering_the_gate_inside_the_gate_panics_instead_of_deadlocking() {
    let _ = guarded(|| spawn(&mut Command::new("/usr/bin/true")));
}

#[test]
fn the_gate_is_usable_again_after_a_reentry_panic() {
    let reentry = std::thread::spawn(|| guarded(|| status(&mut Command::new("/usr/bin/true"))));
    assert!(reentry.join().is_err());
    assert!(
        status(&mut Command::new("/usr/bin/true"))
            .unwrap()
            .success()
    );
}

#[test]
fn output_keeps_the_stdio_the_caller_set() {
    let captured = output(
        Command::new("/bin/sh")
            .args(["-c", "printf out; printf err >&2"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(captured.status.success());
    assert_eq!(captured.stdout, b"out");
    assert!(captured.stderr.is_empty());
}
