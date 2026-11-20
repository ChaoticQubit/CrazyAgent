use super::child_group::{ChildGroup, DROP_TIME, cleanup_owned_group};
use super::login_env::login_child;
use super::login_pipe::{LoginPipe, OUTPUT_BYTES_MAX};
use super::login_windows_tests::{cmd, profile_home};
use crate::engine::codex_host::pump::{self, Signal};
use crazyagent_win_sandbox::ProcessTree;
use std::io::{ErrorKind, Write};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const LINE: &str = "0123456789012345678901234567890123456789012345678";
const QUIET: &str = "ping -n 60 127.0.0.1 >NUL";
const BOUND: &str = "native_auth_test_output_bound";
const FLOOD_BOUND: Duration = Duration::from_secs(30);
const CALL_BOUND: Duration = Duration::from_millis(250);
const PENDING_TIME: Duration = Duration::from_secs(1);
const BROKEN_BOUND: Duration = Duration::from_secs(5);
const WRITE_BYTES: usize = 1 << 20;

/// A `cmd` line that writes about 1 MB to stdout, and as much to stderr when asked.
/// Input: whether stderr floods too. Output: the line.
fn flood(both: bool) -> String {
    let lines = WRITE_BYTES / LINE.len();
    let errors = if both {
        format!("& echo {LINE} 1>&2")
    } else {
        String::new()
    };
    format!("for /L %i in (1,1,{lines}) do @(echo {LINE}{errors})")
}

/// A fake login child, `cmd` running the line as given, built by the one builder with its
/// stdin piped and held in its tree from the start. Input: the line. Output: its profile
/// home folder, the child and its tree.
fn held(line: &str) -> (tempfile::TempDir, Child, ChildGroup) {
    let (folder, home) = profile_home();
    let mut command = Command::new(cmd());
    command.arg("/c").raw_arg(line);
    login_child(&mut command, &home, "CODEX_HOME");
    command.stdin(Stdio::piped());
    let mut child = crate::spawn_gate::spawn(&mut command).unwrap();
    match ProcessTree::hold(&child) {
        Ok(tree) => (folder, child, tree),
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("hold: {error}");
        }
    }
}

/// Reads pumped pipes together until each refuses at its bound, every read returning
/// within [`CALL_BOUND`], so a full pipe never stalls the child on another. Inputs: the
/// pipes, the custody signal and the end of the wait. Output: per pipe, the bytes taken
/// before its refusal, or none when the wait ran out first.
fn read_to_bound(
    pipes: &mut [&mut LoginPipe<pump::Output>],
    signal: &Signal,
    until: Instant,
) -> Vec<Option<usize>> {
    let mut taken = vec![0; pipes.len()];
    let mut bounded = vec![false; pipes.len()];
    while Instant::now() < until && bounded.contains(&false) {
        for (at, pipe) in pipes.iter_mut().enumerate() {
            if bounded[at] {
                continue;
            }
            let started = Instant::now();
            let read = pipe.read();
            assert!(started.elapsed() < CALL_BOUND, "a read blocked");
            match read {
                Ok(bytes) => taken[at] += bytes.len(),
                Err(error) if error.0 == BOUND => bounded[at] = true,
                Err(error) => panic!("read: {}", error.0),
            }
            assert!(!pipe.ended(), "the flood ended before its bound");
        }
        signal.wait_until(Instant::now() + Duration::from_millis(20));
    }
    taken
        .into_iter()
        .zip(bounded)
        .map(|(bytes, bounded)| bounded.then_some(bytes))
        .collect()
}

/// N4 row 3: a login child that floods stdout and stderr never blocks custody's reads, and
/// each pipe refuses at its bound with no more than the bound taken, as on Unix.
#[test]
fn a_login_child_flooding_both_outputs_meets_each_bound_without_a_blocked_read() {
    let (_folder, mut child, tree) = held(&flood(true));
    let signal = Arc::new(Signal::new());
    let mut output = LoginPipe::new(
        pump::output(child.stdout.take().unwrap(), Arc::clone(&signal), "out").unwrap(),
        BOUND,
    );
    let mut errors = LoginPipe::new(
        pump::output(child.stderr.take().unwrap(), Arc::clone(&signal), "err").unwrap(),
        BOUND,
    );
    let read = read_to_bound(
        &mut [&mut output, &mut errors],
        &signal,
        Instant::now() + FLOOD_BOUND,
    );
    let (out, err) = (read[0], read[1]);
    assert!(
        cleanup_owned_group(&mut child, &tree, Instant::now() + DROP_TIME).unwrap(),
        "cleanup"
    );
    let (Some(out), Some(err)) = (out, err) else {
        panic!("a flood never met the bound: {out:?} {err:?}");
    };
    assert!(out <= OUTPUT_BYTES_MAX && err <= OUTPUT_BYTES_MAX);
}

/// N4 row 4: a write to a login child that never reads its stdin stays pending, every call
/// returning at once, and fails once the tree has ended, so custody gives up at its
/// deadline with no hang. S1's pump carries this; the row holds it for the login child.
#[test]
fn a_write_to_a_login_child_that_never_reads_stays_pending_then_fails_once_ended() {
    let (_folder, mut child, tree) = held(QUIET);
    let signal = Arc::new(Signal::new());
    let mut input = pump::input(child.stdin.take().unwrap(), Arc::clone(&signal), "in").unwrap();
    let bytes = vec![b'x'; WRITE_BYTES];
    let until = Instant::now() + PENDING_TIME;
    while Instant::now() < until {
        let started = Instant::now();
        let wrote = input.write(&bytes);
        assert!(started.elapsed() < CALL_BOUND, "a write blocked");
        assert_eq!(wrote.unwrap_err().kind(), ErrorKind::WouldBlock);
        signal.wait_until(Instant::now() + Duration::from_millis(20));
    }
    assert!(cleanup_owned_group(&mut child, &tree, Instant::now() + DROP_TIME).unwrap());
    let started = Instant::now();
    loop {
        match input.write(&bytes) {
            Err(error) if error.kind() == ErrorKind::BrokenPipe => break,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            other => panic!("write after the tree ended: {other:?}"),
        }
        assert!(
            started.elapsed() < BROKEN_BOUND,
            "the pending write never failed"
        );
        signal.wait_until(Instant::now() + Duration::from_millis(20));
    }
}

/// N4 flood row (S1 r3 CR-1354): while a write to a login child that never reads its stdin
/// is pending, custody still reads its flooding output to the bound, so neither side waits
/// on the other.
#[test]
fn a_login_child_flooding_output_while_a_write_is_pending_still_meets_the_bound() {
    let (_folder, mut child, tree) = held(&flood(false));
    let signal = Arc::new(Signal::new());
    let mut input = pump::input(child.stdin.take().unwrap(), Arc::clone(&signal), "in").unwrap();
    let mut output = LoginPipe::new(
        pump::output(child.stdout.take().unwrap(), Arc::clone(&signal), "out").unwrap(),
        BOUND,
    );
    let bytes = vec![b'x'; WRITE_BYTES];
    assert_eq!(
        input.write(&bytes).unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
    let out = read_to_bound(&mut [&mut output], &signal, Instant::now() + FLOOD_BOUND)[0];
    let pending = input.write(&bytes).map_err(|error| error.kind());
    assert!(cleanup_owned_group(&mut child, &tree, Instant::now() + DROP_TIME).unwrap());
    assert!(
        out.is_some(),
        "the output never met the bound while a write was pending"
    );
    assert_eq!(pending, Err(ErrorKind::WouldBlock));
}
