use super::custody::{
    DirectIdentity, GroupCleanup, OwnedProcess, ProcessBounds, ProcessObservation, ProcessStop,
};
use rustix::process::{Pid, Signal};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::{Duration, Instant};

const STREAM_BYTES: usize = 65_536;
const OBSERVATION_WAIT: Duration = Duration::from_secs(2);
const CASE_WALL_MAX: Duration = Duration::from_secs(10);

struct Case {
    root: tempfile::TempDir,
}

struct InertChild {
    process: OwnedProcess,
    identity: DirectIdentity,
}

struct Sentinel {
    child: Child,
}

fn pid(value: u32) -> Pid {
    assert!(value > 1);
    Pid::from_raw(i32::try_from(value).unwrap()).unwrap()
}

fn bounds() -> ProcessBounds {
    ProcessBounds {
        run_time: Duration::from_secs(3),
        cleanup_time: Duration::from_secs(5),
        stream_bytes: STREAM_BYTES,
    }
}

fn environment(root: &Path) -> Vec<(OsString, OsString)> {
    vec![
        ("PATH".into(), "/ca772-selected-path".into()),
        ("HOME".into(), root.as_os_str().to_owned()),
        ("TMPDIR".into(), root.as_os_str().to_owned()),
        ("LANG".into(), "C".into()),
        ("LC_ALL".into(), "C".into()),
    ]
}

fn shell(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script, "ca772"]);
    command
}

fn wait_file(path: &Path) -> Vec<u8> {
    let deadline = Instant::now() + OBSERVATION_WAIT;
    loop {
        match fs::read(path) {
            Ok(bytes) if !bytes.is_empty() => return bytes,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("fixture observation failed: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "fixture did not produce its marker"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn assert_stream_bounds(observation: &ProcessObservation) {
    assert!(observation.stdout.bytes.len() <= STREAM_BYTES + 1);
    assert!(observation.stderr.bytes.len() <= STREAM_BYTES + 1);
    assert!(observation.stdout.observed_bytes >= observation.stdout.bytes.len() as u64);
    assert!(observation.stderr.observed_bytes >= observation.stderr.bytes.len() as u64);
}

fn assert_spaces(bytes: &[u8], count: usize) {
    assert_eq!(bytes.len(), count);
    assert!(bytes.iter().all(|byte| *byte == b' '));
}

fn assert_observed_group_cleanup(observation: &ProcessObservation) {
    let GroupCleanup {
        direct_reaped,
        group_absent,
        uncertain,
    } = &observation.cleanup;
    assert!(*direct_reaped);
    assert!(*group_absent);
    assert!(!*uncertain);
    assert!(observation.status.is_some());
    assert!(observation.stdout.eof);
    assert!(observation.stderr.eof);
    assert!(matches!(
        rustix::process::waitpid(
            Some(pid(observation.identity.pid)),
            rustix::process::WaitOptions::NOHANG
        ),
        Err(rustix::io::Errno::CHILD)
    ));
    assert_eq!(
        rustix::process::test_kill_process_group(pid(observation.identity.process_group)),
        Err(rustix::io::Errno::SRCH)
    );
    assert_stream_bounds(observation);
}

impl Case {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn spawn(&self, command: Command, limits: ProcessBounds) -> InertChild {
        let process = OwnedProcess::spawn(
            command,
            self.root.path(),
            &environment(self.root.path()),
            limits,
        )
        .unwrap();
        let identity = process.identity();
        let child = InertChild { process, identity };
        assert!(child.identity.pid > 1);
        assert_eq!(child.identity.process_group, child.identity.pid);
        child
    }
}

impl InertChild {
    fn finish(&mut self, cancelled: bool) -> ProcessObservation {
        let started = Instant::now();
        let observation = self.process.finish(&AtomicBool::new(cancelled)).unwrap();
        assert!(started.elapsed() < CASE_WALL_MAX);
        assert_eq!(observation.identity, self.identity);
        observation
    }
}

impl Drop for InertChild {
    fn drop(&mut self) {
        if self.identity.process_group <= 1 || self.identity.process_group != self.identity.pid {
            return;
        }
        if rustix::process::test_kill_process_group(pid(self.identity.process_group)).is_ok() {
            let _ =
                rustix::process::kill_process_group(pid(self.identity.process_group), Signal::KILL);
        }
    }
}

impl Sentinel {
    fn start(root: &Path) -> Self {
        let child = Command::new("/bin/sleep")
            .arg("30")
            .env_clear()
            .envs(environment(root))
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        Self { child }
    }

    fn assert_live(&mut self) {
        assert!(self.child.try_wait().unwrap().is_none());
        assert!(rustix::process::test_kill_process(pid(self.child.id())).is_ok());
    }

    fn stop(&mut self) {
        rustix::process::kill_process_group(pid(self.child.id()), Signal::KILL).unwrap();
        let deadline = Instant::now() + OBSERVATION_WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(status.signal(), Some(9));
                break;
            }
            assert!(Instant::now() < deadline, "sentinel was not reaped");
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Sentinel {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        let _ = rustix::process::kill_process_group(pid(self.child.id()), Signal::KILL);
        let _ = self.child.kill();
        let deadline = Instant::now() + OBSERVATION_WAIT;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => thread::sleep(Duration::from_millis(5)),
            }
        }
    }
}

#[test]
fn native_auth_custody_retains_the_actual_direct_pid_and_group() {
    let case = Case::new();
    let marker = case.path("direct.pid");
    let mut command = shell("printf '%s' \"$$\" > \"$1\"; exec /bin/sleep 30");
    command.arg(&marker);
    let mut child = case.spawn(command, bounds());
    let direct = String::from_utf8(wait_file(&marker)).unwrap();
    assert_eq!(direct.parse::<u32>().unwrap(), child.identity.pid);
    assert_eq!(child.process.identity(), child.identity);
    assert_eq!(
        rustix::process::getpgid(Some(pid(child.identity.pid))).unwrap(),
        pid(child.identity.process_group)
    );
    let observation = child.finish(true);
    assert_eq!(observation.stop, ProcessStop::Cancelled);
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_observes_both_streams_and_real_eof_on_success() {
    let case = Case::new();
    let mut child = case.spawn(shell("printf out; printf err >&2"), bounds());
    let observation = child.finish(false);
    assert_eq!(observation.stop, ProcessStop::Exited);
    assert!(observation.status.unwrap().success());
    assert_eq!(observation.stdout.bytes, b"out");
    assert_eq!(observation.stderr.bytes, b"err");
    assert_eq!(observation.stdout.observed_bytes, 3);
    assert_eq!(observation.stderr.observed_bytes, 3);
    assert!(!observation.stdout.truncated);
    assert!(!observation.stderr.truncated);
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_gives_native_cat_closed_stdin() {
    let case = Case::new();
    let input = case.path("configured-stdin.txt");
    fs::write(&input, b"ca772-input-must-not-reach-native-cat").unwrap();
    let mut command = Command::new("/bin/cat");
    command.stdin(File::open(input).unwrap());
    let mut child = case.spawn(command, bounds());
    let observation = child.finish(false);
    assert_eq!(observation.stop, ProcessStop::Exited);
    assert!(observation.status.unwrap().success());
    assert!(observation.stdout.bytes.is_empty());
    assert!(observation.stderr.bytes.is_empty());
    assert_eq!(observation.stdout.observed_bytes, 0);
    assert_eq!(observation.stderr.observed_bytes, 0);
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_retains_a_real_nonzero_exit_code() {
    let case = Case::new();
    let mut child = case.spawn(shell("exit 7"), bounds());
    let observation = child.finish(false);
    assert_eq!(observation.stop, ProcessStop::Exited);
    assert_eq!(observation.status.unwrap().code(), Some(7));
    assert!(!observation.status.unwrap().success());
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_deadline_reaps_a_real_quiet_child() {
    let case = Case::new();
    let mut limits = bounds();
    limits.run_time = Duration::from_millis(300);
    let mut command = Command::new("/bin/sleep");
    command.arg("30");
    let mut child = case.spawn(command, limits);
    let observation = child.finish(false);
    assert_eq!(observation.stop, ProcessStop::Deadline);
    assert!(!observation.status.unwrap().success());
    assert!(observation.stdout.bytes.is_empty());
    assert!(observation.stderr.bytes.is_empty());
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_deadline_escalates_for_actual_term_ignoring_exec() {
    let case = Case::new();
    let marker = case.path("term-ignored");
    let mut command = shell("trap '' TERM; printf started > \"$1\"; exec /bin/sleep 30");
    command.arg(&marker);
    let mut limits = bounds();
    limits.run_time = Duration::from_millis(300);
    let mut child = case.spawn(command, limits);
    assert_eq!(wait_file(&marker), b"started");
    let observation = child.finish(false);
    assert_eq!(observation.stop, ProcessStop::Deadline);
    assert_eq!(observation.status.unwrap().signal(), Some(9));
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_cancel_acts_on_an_observed_live_child() {
    let case = Case::new();
    let marker = case.path("cancel-live");
    let mut command = shell("printf started > \"$1\"; exec /bin/sleep 30");
    command.arg(&marker);
    let mut child = case.spawn(command, bounds());
    assert_eq!(wait_file(&marker), b"started");
    assert!(rustix::process::test_kill_process(pid(child.identity.pid)).is_ok());
    let observation = child.finish(true);
    assert_eq!(observation.stop, ProcessStop::Cancelled);
    assert!(!observation.status.unwrap().success());
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_accepts_exact_stdout_bound() {
    let case = Case::new();
    let mut child = case.spawn(shell("printf '%65536s' ''"), bounds());
    let observation = child.finish(false);
    assert_eq!(observation.stop, ProcessStop::Exited);
    assert!(observation.status.unwrap().success());
    assert_spaces(&observation.stdout.bytes, STREAM_BYTES);
    assert_eq!(observation.stdout.observed_bytes, STREAM_BYTES as u64);
    assert!(!observation.stdout.truncated);
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_accepts_exact_stderr_bound() {
    let case = Case::new();
    let mut child = case.spawn(shell("printf '%65536s' '' >&2"), bounds());
    let observation = child.finish(false);
    assert_eq!(observation.stop, ProcessStop::Exited);
    assert!(observation.status.unwrap().success());
    assert_spaces(&observation.stderr.bytes, STREAM_BYTES);
    assert_eq!(observation.stderr.observed_bytes, STREAM_BYTES as u64);
    assert!(!observation.stderr.truncated);
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_checks_fast_final_stdout_overflow() {
    let case = Case::new();
    let mut child = case.spawn(shell("printf '%65537s' ''"), bounds());
    let observation = child.finish(false);
    assert_eq!(observation.stop, ProcessStop::StdoutLimit);
    assert_spaces(&observation.stdout.bytes, STREAM_BYTES + 1);
    assert_eq!(observation.stdout.observed_bytes, STREAM_BYTES as u64 + 1);
    assert!(!observation.stdout.truncated);
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_checks_fast_final_stderr_overflow() {
    let case = Case::new();
    let mut child = case.spawn(shell("printf '%65537s' '' >&2"), bounds());
    let observation = child.finish(false);
    assert_eq!(observation.stop, ProcessStop::StderrLimit);
    assert_spaces(&observation.stderr.bytes, STREAM_BYTES + 1);
    assert_eq!(observation.stderr.observed_bytes, STREAM_BYTES as u64 + 1);
    assert!(!observation.stderr.truncated);
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_observes_stdout_overflow_before_the_run_deadline() {
    let case = Case::new();
    let mut limits = bounds();
    limits.run_time = Duration::from_secs(8);
    let command = shell("printf '%262144s' ''; exec /bin/sleep 30");
    let mut child = case.spawn(command, limits);
    let started = Instant::now();
    let observation = child.finish(false);
    assert!(started.elapsed() < Duration::from_secs(7));
    assert_eq!(observation.stop, ProcessStop::StdoutLimit);
    assert!((STREAM_BYTES as u64 + 1..=262_144).contains(&observation.stdout.observed_bytes));
    assert_spaces(&observation.stdout.bytes, STREAM_BYTES + 1);
    assert_eq!(
        observation.stdout.truncated,
        observation.stdout.observed_bytes > observation.stdout.bytes.len() as u64
    );
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_observes_stderr_overflow_before_the_run_deadline() {
    let case = Case::new();
    let mut limits = bounds();
    limits.run_time = Duration::from_secs(8);
    let command = shell("printf '%262144s' '' >&2; exec /bin/sleep 30");
    let mut child = case.spawn(command, limits);
    let started = Instant::now();
    let observation = child.finish(false);
    assert!(started.elapsed() < Duration::from_secs(7));
    assert_eq!(observation.stop, ProcessStop::StderrLimit);
    assert!((STREAM_BYTES as u64 + 1..=262_144).contains(&observation.stderr.observed_bytes));
    assert_spaces(&observation.stderr.bytes, STREAM_BYTES + 1);
    assert_eq!(
        observation.stderr.truncated,
        observation.stderr.observed_bytes > observation.stderr.bytes.len() as u64
    );
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_bounds_eof_wait_after_direct_parent_exit() {
    let case = Case::new();
    let marker = case.path("in-group-child.pid");
    let script = "/bin/sleep 30 & printf '%s' \"$!\" > \"$1\"; printf parent-exit; exit 0";
    let mut command = shell(script);
    command.arg(&marker);
    let mut child = case.spawn(command, bounds());
    let descendant = String::from_utf8(wait_file(&marker))
        .unwrap()
        .parse::<u32>()
        .unwrap();
    assert_ne!(descendant, child.identity.pid);
    assert_eq!(
        rustix::process::getpgid(Some(pid(descendant))).unwrap(),
        pid(child.identity.process_group)
    );
    let observation = child.finish(false);
    assert_eq!(observation.stop, ProcessStop::Exited);
    assert!(observation.status.unwrap().success());
    assert_eq!(observation.stdout.bytes, b"parent-exit");
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_replaces_preexec_environment_without_runtime_equality_claim() {
    let case = Case::new();
    let mut command = Command::new("/usr/bin/printenv");
    command.env("CA772_SYNTHETIC_PARENT_ONLY", "must-not-reach-child");
    command.env("PATH", "/ca772-command-overlay");
    command.env("LANG", "ca772-overlay");
    let mut child = case.spawn(command, bounds());
    let observation = child.finish(false);
    assert_eq!(observation.stop, ProcessStop::Exited);
    assert!(observation.status.unwrap().success());
    let text = std::str::from_utf8(&observation.stdout.bytes).unwrap();
    let observed: BTreeMap<_, _> = text
        .lines()
        .map(|line| line.split_once('=').unwrap())
        .collect();
    for (key, value) in environment(case.root.path()) {
        assert_eq!(
            observed.get(key.to_str().unwrap()),
            Some(&value.to_str().unwrap())
        );
    }
    assert!(!observed.contains_key("CA772_SYNTHETIC_PARENT_ONLY"));
    assert!(!text.contains("must-not-reach-child"));
    assert!(!text.contains("/ca772-command-overlay"));
    assert!(!text.contains("ca772-overlay"));
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_keeps_a_retained_cloexec_file_out_of_the_exec_child() {
    let case = Case::new();
    let protected = case.path("retained-nonsecret.txt");
    fs::write(&protected, b"ca772-retained-file").unwrap();
    let retained = File::open(&protected).unwrap();
    let mut command = Command::new("/bin/cat");
    command.arg(format!("/dev/fd/{}", retained.as_raw_fd()));
    let mut child = case.spawn(command, bounds());
    let observation = child.finish(false);
    assert_eq!(observation.stop, ProcessStop::Exited);
    assert!(!observation.status.unwrap().success());
    assert!(observation.stdout.bytes.is_empty());
    assert_eq!(fs::read(&protected).unwrap(), b"ca772-retained-file");
    assert_eq!(retained.metadata().unwrap().len(), 19);
    assert_observed_group_cleanup(&observation);
}

#[test]
fn native_auth_custody_cancellation_does_not_kill_an_unrelated_sentinel() {
    let case = Case::new();
    let mut sentinel = Sentinel::start(case.root.path());
    sentinel.assert_live();
    let marker = case.path("target-live");
    let mut command = shell("printf started > \"$1\"; exec /bin/sleep 30");
    command.arg(&marker);
    let mut child = case.spawn(command, bounds());
    assert_ne!(sentinel.child.id(), child.identity.pid);
    assert_eq!(wait_file(&marker), b"started");
    let observation = child.finish(true);
    assert_eq!(observation.stop, ProcessStop::Cancelled);
    assert_observed_group_cleanup(&observation);
    sentinel.assert_live();
    sentinel.stop();
}

#[test]
fn native_auth_custody_expired_cleanup_budget_remains_explicitly_uncertain() {
    let case = Case::new();
    let marker = case.path("cleanup-live");
    let mut command = shell("trap '' TERM; printf started > \"$1\"; exec /bin/sleep 30");
    command.arg(&marker);
    let mut limits = bounds();
    limits.cleanup_time = Duration::from_nanos(1);
    let mut child = case.spawn(command, limits);
    assert_eq!(wait_file(&marker), b"started");
    let observation = child.finish(true);
    assert_eq!(observation.stop, ProcessStop::Cancelled);
    assert!(observation.cleanup.uncertain);
    assert!(!observation.cleanup.direct_reaped || !observation.cleanup.group_absent);
    assert_stream_bounds(&observation);
    let direct = pid(child.identity.pid);
    let started = Instant::now();
    drop(child);
    assert!(started.elapsed() < CASE_WALL_MAX);
    assert!(matches!(
        rustix::process::waitpid(Some(direct), rustix::process::WaitOptions::NOHANG),
        Err(rustix::io::Errno::CHILD)
    ));
}

#[test]
fn native_auth_custody_invalid_bounds_do_not_spawn_the_marker_program() {
    let case = Case::new();
    for invalid in 0..3 {
        let marker = case.path(&format!("invalid-{invalid}"));
        let mut command = shell("printf ran > \"$1\"");
        command.arg(&marker);
        let mut limits = bounds();
        match invalid {
            0 => limits.run_time = Duration::ZERO,
            1 => limits.cleanup_time = Duration::ZERO,
            2 => limits.stream_bytes = 0,
            _ => unreachable!(),
        }
        let result = OwnedProcess::spawn(
            command,
            case.root.path(),
            &environment(case.root.path()),
            limits,
        );
        let error = match result {
            Ok(process) => {
                let identity = process.identity();
                let _cleanup = InertChild { process, identity };
                panic!("invalid bounds admitted a child");
            }
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!marker.exists());
    }
}
