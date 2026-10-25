use super::custodian_command;
use crate::research::runner::Lifeline;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

const TRIALS: usize = 200;
const WATCHERS: usize = 12;
const HELD: Duration = Duration::from_secs(3);
const PROMPT: Duration = Duration::from_secs(2);
const CUSTODIAN_BOUND: Duration = Duration::from_secs(15);

fn executable() -> PathBuf {
    let path = std::env::var_os("CRAZYAGENT_TEST_NATIVE_AUTH_HOST")
        .map(PathBuf::from)
        .expect("root must supply the source-matched product custodian executable path");
    assert!(path.is_absolute());
    path
}

struct Group(Child);

impl Drop for Group {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn reap(held: &Mutex<Vec<(Instant, Lifeline)>>, older_than: Duration) {
    let mut held = held.lock().unwrap();
    let now = Instant::now();
    held.retain(|(started, _)| now.duration_since(*started) < older_than);
}

/// Starts the owner-less custodian the way dispatch does while twelve research
/// lifelines start at the same instant, many times. A lifeline that inherits the
/// custodian's bootstrap pipe holds it open for three seconds, so the custodian
/// sees no end of input and cannot answer within two.
#[test]
fn custodian_bootstrap_pipe_never_reaches_a_child_started_at_the_same_time() {
    let executable = executable();
    let runtime = tempfile::Builder::new()
        .prefix("ca-bootstrap-pipe-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let group = Group(
        crate::spawn_gate::spawn(
            Command::new("/bin/sleep")
                .arg("600")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0),
        )
        .unwrap(),
    );
    let leader = group.0.id();
    let held = Arc::new(Mutex::new(Vec::new()));
    for trial in 0..TRIALS {
        reap(&held, HELD);
        let barrier = Arc::new(Barrier::new(WATCHERS + 1));
        let watchers: Vec<_> = (0..WATCHERS)
            .map(|_| {
                let barrier = barrier.clone();
                let held = held.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let lifeline = Lifeline::watch(leader).unwrap();
                    held.lock().unwrap().push((Instant::now(), lifeline));
                })
            })
            .collect();
        let mut command = custodian_command(&executable, runtime.path(), None).unwrap();
        barrier.wait();
        let started = Instant::now();
        let mut custodian = crate::spawn_gate::spawn(&mut command).unwrap();
        let mut input = custodian.stdin.take().unwrap();
        input.write_all(&[0xff; 32]).unwrap();
        drop(input);
        let status = loop {
            if let Some(status) = custodian.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() > CUSTODIAN_BOUND {
                let _ = custodian.kill();
                let _ = custodian.wait();
                panic!("trial {trial}: the custodian outlived its own bootstrap bound");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let took = started.elapsed();
        for watcher in watchers {
            watcher.join().unwrap();
        }
        assert_eq!(status.code(), Some(1), "trial {trial}");
        assert!(
            took < PROMPT,
            "trial {trial}: the custodian answered after {took:?}; a lifeline started with it held its bootstrap pipe"
        );
    }
    reap(&held, Duration::ZERO);
    assert!(held.lock().unwrap().is_empty());
    assert_eq!(
        rustix::process::test_kill_process(
            rustix::process::Pid::from_raw(i32::try_from(leader).unwrap()).unwrap()
        ),
        Ok(()),
        "a released lifeline must never stop the group it watched"
    );
    assert_eq!(std::fs::read_dir(runtime.path()).unwrap().count(), 0);
}
