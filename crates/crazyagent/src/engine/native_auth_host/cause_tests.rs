use super::{CAUSE_BYTES_MAX, CustodianCause};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BOUND: Duration = Duration::from_secs(15);

/// Starts a real shell that runs the script with the text as its one argument and its error
/// stream piped, and takes that stream as dispatch does. Inputs: the script and the text.
/// Output: the shell and its cause, nothing read yet.
fn start(script: &str, text: &str) -> (Child, CustodianCause) {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .arg("sh")
        .arg(text)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = crate::spawn_gate::spawn(&mut command).unwrap();
    let cause = CustodianCause::new(child.stderr.take().unwrap()).unwrap();
    (child, cause)
}

/// Reads the shell's error stream as dispatch does, on every poll and once after it exited.
/// Inputs: the shell and its cause. Output: the cause once the shell exited.
fn finish(mut child: Child, mut cause: CustodianCause) -> CustodianCause {
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        cause.read().unwrap();
        assert!(
            started.elapsed() < BOUND,
            "the shell never finished writing"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    cause.read().unwrap();
    cause
}

/// Writes the text and its line end on the error stream and exits 1, as the custodian does.
/// Input: the text. Output: the code read from it, if any.
fn cause_of(text: &str) -> Option<String> {
    let (child, cause) = start("printf '%s\\n' \"$1\" >&2; exit 1", text);
    finish(child, cause).code().map(str::to_owned)
}

#[test]
fn a_custodian_cause_is_one_line_of_1_to_128_lowercase_letters_and_underscores() {
    let longest = "a".repeat(CAUSE_BYTES_MAX);
    assert_eq!(cause_of(&longest), Some(longest.clone()));
    assert_eq!(
        cause_of("native_profile_binding_conflict").as_deref(),
        Some("native_profile_binding_conflict")
    );
    assert_eq!(cause_of(&"a".repeat(CAUSE_BYTES_MAX + 1)), None);
    assert_eq!(cause_of(""), None);
    assert_eq!(cause_of("Native_auth"), None);
    assert_eq!(cause_of("native_auth_2"), None);
    assert_eq!(cause_of("native_auth\nnative_auth"), None);
}

#[test]
fn a_custodian_that_writes_more_than_its_pipe_holds_is_drained_and_kept_to_the_bound() {
    let (child, cause) = start(
        "/usr/bin/head -c 1048576 /dev/zero | /usr/bin/tr '\\000' a >&2; exit 1",
        "",
    );
    let cause = finish(child, cause);
    assert_eq!(cause.text.len(), CAUSE_BYTES_MAX + 2);
    assert_eq!(cause.code(), None);
}

#[test]
fn a_cause_read_returns_while_the_custodian_keeps_its_error_stream_open() {
    let (mut child, mut cause) = start(
        "printf '%s\\n' \"$1\" >&2; exec /bin/sleep 30",
        "native_auth_busy",
    );
    let started = Instant::now();
    while cause.code().is_none() {
        cause.read().unwrap();
        assert!(
            started.elapsed() < BOUND,
            "the cause read waited on an open stream"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        started.elapsed() < BOUND,
        "the cause read waited on an open stream"
    );
    assert_eq!(cause.code(), Some("native_auth_busy"));
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
    child.wait().unwrap();
}
