use super::custody::{cleanup_direct_child, retain_direct_child};
use rustix::process::{Pid, WaitOptions};
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const CHILD_WAIT: Duration = Duration::from_secs(2);
const CASE_WALL_MAX: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

struct InertChild {
    child: Option<Child>,
}

fn pid(value: u32) -> Pid {
    assert!(value > 1);
    Pid::from_raw(i32::try_from(value).unwrap()).unwrap()
}

fn wait_for_exit(child: &mut Child, deadline: Instant) -> io::Result<Option<ExitStatus>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn fallback_cleanup(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    child.kill()?;
    if wait_for_exit(child, Instant::now() + CHILD_WAIT)?.is_some() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "fixture fallback did not reap the retained child",
    ))
}

impl InertChild {
    fn spawn(root: &Path, executable: &str, arguments: &[&str]) -> Self {
        let child = Command::new(executable)
            .args(arguments)
            .env_clear()
            .env("PATH", "/ca799-selected-path")
            .env("HOME", root)
            .env("TMPDIR", root)
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self { child: Some(child) }
    }

    fn child(&mut self) -> &mut Child {
        self.child.as_mut().unwrap()
    }

    fn take(&mut self) -> Child {
        self.child.take().unwrap()
    }

    fn assert_live(&mut self) {
        assert!(self.child().try_wait().unwrap().is_none());
        assert!(rustix::process::test_kill_process(pid(self.child().id())).is_ok());
    }

    fn assert_inherited_group(&mut self, group: Pid) {
        let direct = pid(self.child().id());
        assert_ne!(direct, group);
        assert_eq!(rustix::process::getpgid(Some(direct)).unwrap(), group);
        self.assert_live();
    }

    fn cleanup(&mut self) -> ExitStatus {
        let direct = self.child().id();
        let status = cleanup_direct_child(self.child(), Instant::now() + CHILD_WAIT)
            .unwrap()
            .expect("direct cleanup must return an actual reaped status");
        assert_eq!(self.child().id(), direct);
        assert_reaped(self.child(), status);
        status
    }
}

impl Drop for InertChild {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if let Err(error) = fallback_cleanup(child) {
                if thread::panicking() {
                    eprintln!(
                        "fixture fallback failed for direct child {}: {error}",
                        child.id()
                    );
                } else {
                    panic!(
                        "fixture fallback failed for direct child {}: {error}",
                        child.id()
                    );
                }
            }
        }
    }
}

fn assert_reaped(child: &mut Child, status: ExitStatus) {
    assert!(matches!(
        rustix::process::waitpid(Some(pid(child.id())), WaitOptions::NOHANG),
        Err(rustix::io::Errno::CHILD)
    ));
    assert_eq!(child.try_wait().unwrap(), Some(status));
}

#[test]
fn native_auth_custody_session_rejected_live_child_keeps_ownership_and_sentinel() {
    let started = Instant::now();
    let root = tempfile::tempdir().unwrap();
    let group = rustix::process::getpgid(None).unwrap();
    let mut sentinel = InertChild::spawn(root.path(), "/bin/sleep", &["30"]);
    let mut first = InertChild::spawn(root.path(), "/bin/sleep", &["30"]);
    let mut rejected = InertChild::spawn(root.path(), "/bin/sleep", &["30"]);
    sentinel.assert_inherited_group(group);
    first.assert_inherited_group(group);
    rejected.assert_inherited_group(group);
    let first_id = first.child().id();
    let rejected_id = rejected.child().id();
    let mut retained = InertChild { child: None };
    if let Err(child) = retain_direct_child(&mut retained.child, first.take()) {
        first.child = Some(child);
        panic!("an empty slot rejected its actual child");
    }
    assert_eq!(retained.child().id(), first_id);
    retained.assert_live();
    match retain_direct_child(&mut retained.child, rejected.take()) {
        Err(child) => rejected.child = Some(child),
        Ok(()) => panic!("an occupied slot accepted a second child"),
    }
    assert_eq!(retained.child().id(), first_id);
    assert_eq!(rejected.child().id(), rejected_id);
    retained.assert_live();
    rejected.assert_live();
    sentinel.assert_live();
    let expired = Instant::now().checked_sub(POLL_INTERVAL).unwrap();
    assert!(
        cleanup_direct_child(rejected.child(), expired)
            .unwrap()
            .is_none()
    );
    assert_eq!(rejected.child().id(), rejected_id);
    rejected.assert_live();
    retained.assert_live();
    sentinel.assert_live();
    let rejected_status = rejected.cleanup();
    assert_eq!(rejected_status.signal(), Some(9));
    retained.assert_live();
    sentinel.assert_live();
    let retained_status = retained.cleanup();
    assert_eq!(retained_status.signal(), Some(9));
    sentinel.assert_inherited_group(group);
    assert_eq!(rustix::process::getpgid(None).unwrap(), group);
    assert_eq!(sentinel.cleanup().signal(), Some(9));
    assert!(started.elapsed() < CASE_WALL_MAX);
}

#[test]
fn native_auth_custody_session_already_reaped_child_keeps_status_and_sentinel() {
    let started = Instant::now();
    let root = tempfile::tempdir().unwrap();
    let group = rustix::process::getpgid(None).unwrap();
    let mut sentinel = InertChild::spawn(root.path(), "/bin/sleep", &["30"]);
    sentinel.assert_inherited_group(group);
    let mut exited = InertChild::spawn(root.path(), "/usr/bin/true", &[]);
    let direct = exited.child().id();
    assert_ne!(pid(direct), group);
    let original = wait_for_exit(exited.child(), Instant::now() + CHILD_WAIT)
        .unwrap()
        .expect("the actual true child must exit before cleanup");
    assert!(original.success());
    assert_reaped(exited.child(), original);
    assert_eq!(exited.cleanup(), original);
    assert_eq!(exited.child().id(), direct);
    assert!(original.success());
    sentinel.assert_inherited_group(group);
    assert_eq!(rustix::process::getpgid(None).unwrap(), group);
    assert_eq!(sentinel.cleanup().signal(), Some(9));
    assert!(started.elapsed() < CASE_WALL_MAX);
}
