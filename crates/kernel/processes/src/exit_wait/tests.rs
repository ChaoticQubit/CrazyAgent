use super::*;
use std::process::{Command, Stdio};
use std::time::Duration;

/// The test hooks of a watch, read on the arming thread and carried into the waiter thread,
/// where a thread-local hook would not reach.
pub(crate) mod hooks {
    use std::cell::Cell;
    #[cfg(unix)]
    use std::cell::RefCell;
    #[cfg(unix)]
    use std::sync::Arc;
    #[cfg(unix)]
    use std::sync::mpsc::{Receiver, Sender, channel};

    thread_local! {
        static ARMS: Cell<usize> = const { Cell::new(0) };
        static REAPS: Cell<usize> = const { Cell::new(0) };
        static HEARD: Cell<Option<super::super::HeardAnswer>> = const { Cell::new(None) };
        #[cfg(unix)]
        static HOLD: Cell<bool> = const { Cell::new(false) };
        #[cfg(unix)]
        static ALIVE: RefCell<Option<Arc<()>>> = const { RefCell::new(None) };
        #[cfg(target_os = "linux")]
        static PIDFD_ERROR: Cell<Option<rustix::io::Errno>> = const { Cell::new(None) };
    }

    /// Counts one arm on this thread. Output: none.
    pub(crate) fn armed() {
        ARMS.with(|arms| arms.set(arms.get() + 1));
    }

    /// How many watches this thread armed. Output: the count.
    pub(crate) fn arms() -> usize {
        ARMS.with(Cell::get)
    }

    /// Counts one reap through a watch on this thread and keeps what its waiter answered.
    /// Input: the answer, none when no thread waited. Output: none.
    pub(crate) fn reaped(heard: Option<super::super::HeardAnswer>) {
        REAPS.with(|reaps| reaps.set(reaps.get() + 1));
        HEARD.with(|last| last.set(heard));
    }

    /// How many reaps this thread made through a watch, and what the last one's waiter
    /// answered. Output: the count and the answer.
    pub(crate) fn reaps() -> (usize, Option<super::super::HeardAnswer>) {
        (REAPS.with(Cell::get), HEARD.with(Cell::get))
    }

    /// Makes this thread's next waiters hold before their `waitid` until the reap releases
    /// them. Input: whether to hold. Output: none.
    #[cfg(unix)]
    pub(crate) fn hold(on: bool) {
        HOLD.with(|hold| hold.set(on));
    }

    /// Hands this thread's next waiters a clone of `alive`, dropped when the waiter ends.
    /// Input: the token. Output: none.
    #[cfg(unix)]
    pub(crate) fn alive(token: Option<Arc<()>>) {
        ALIVE.with(|alive| *alive.borrow_mut() = token);
    }

    /// Makes this thread's process descriptors fail with `error`. Input: the error, or none
    /// to open them. Output: none.
    #[cfg(target_os = "linux")]
    pub(crate) fn refuse_pidfd(error: Option<rustix::io::Errno>) {
        PIDFD_ERROR.with(|refused| refused.set(error));
    }

    /// The error this thread's process descriptors are set to fail with. Output: it, if any.
    #[cfg(target_os = "linux")]
    pub(crate) fn forced_pidfd_error() -> Option<rustix::io::Errno> {
        PIDFD_ERROR.with(Cell::get)
    }

    /// What one waiter thread carries: the hold it waits on before its `waitid`, and the
    /// token it drops when it ends.
    #[cfg(unix)]
    pub(crate) struct WaiterHooks {
        hold: Option<Receiver<()>>,
        _alive: Option<Arc<()>>,
    }

    #[cfg(unix)]
    impl WaiterHooks {
        /// Waits for the reap's release when this waiter holds. Output: none.
        pub(crate) fn before_waitid(&self) {
            if let Some(hold) = &self.hold {
                let _ = hold.recv();
            }
        }
    }

    /// The hooks for a waiter armed now on this thread. Output: the release the reap sends,
    /// if the waiter holds, and the waiter's own hooks.
    #[cfg(unix)]
    pub(crate) fn for_waiter() -> (Option<Sender<()>>, WaiterHooks) {
        let (release, hold) = if HOLD.with(Cell::get) {
            let (release, hold) = channel();
            (Some(release), Some(hold))
        } else {
            (None, None)
        };
        let alive = ALIVE.with(|alive| alive.borrow().clone());
        (
            release,
            WaiterHooks {
                hold,
                _alive: alive,
            },
        )
    }
}

/// A child of `program` with no standard streams. Inputs: the program and its arguments.
/// Output: the running child.
fn started(program: &str, args: &[&str]) -> Child {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::spawn_gate::spawn(&mut command).unwrap()
}

/// A child that exits at once with success. Output: the child.
fn quick() -> Child {
    #[cfg(unix)]
    {
        started("/usr/bin/true", &[])
    }
    #[cfg(windows)]
    {
        started("cmd.exe", &["/C", "exit 0"])
    }
}

/// A child that runs for about `seconds` seconds. Input: the seconds. Output: the child.
fn lasting(seconds: &str) -> Child {
    #[cfg(unix)]
    {
        started("/bin/sleep", &[seconds])
    }
    #[cfg(windows)]
    {
        let count = (seconds.parse::<f64>().unwrap().ceil() as u32 + 1).to_string();
        started("ping.exe", &["-n", &count, "127.0.0.1"])
    }
}

/// Whether no unreaped child of this process has `child`'s number left, so it was reaped
/// exactly once. Input: the child. Output: true when `waitid` finds none.
#[cfg(unix)]
fn reaped(child: &Child) -> bool {
    matches!(
        rustix::process::waitid(
            WaitId::Pid(pid_of(child)),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG,
        ),
        Err(Errno::CHILD)
    )
}

/// Sets the hooks a waiter row needs, so Linux tests the waiter thread too. Inputs: whether
/// the waiter holds, and its token. Output: none.
#[cfg(unix)]
fn waiter_hooks(hold: bool, alive: Option<std::sync::Arc<()>>) {
    #[cfg(target_os = "linux")]
    hooks::refuse_pidfd(Some(Errno::PERM));
    hooks::hold(hold);
    hooks::alive(alive);
}

/// A child that exited before the watch armed is heard at once, reaped once, and a second
/// wait reads std's kept status.
#[cfg(unix)]
#[test]
fn an_exit_before_the_watch_is_heard_at_once() {
    let mut child = quick();
    rustix::process::waitid(
        WaitId::Pid(pid_of(&child)),
        WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
    )
    .unwrap();
    let mut watch = watch(&mut child).unwrap();
    let start = Instant::now();
    assert!(
        watch
            .exited_by(&child, start + Duration::from_secs(5))
            .unwrap()
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(watch.reap(&mut child).unwrap().success());
    assert!(reaped(&child));
    assert!(child.wait().unwrap().success());
}

/// A child that exits at once is heard and reaped, on every system.
#[test]
fn a_quick_exit_is_heard_and_reaped() {
    let mut child = quick();
    let mut watch = watch(&mut child).unwrap();
    assert!(
        watch
            .exited_by(&child, Instant::now() + Duration::from_secs(30))
            .unwrap()
    );
    assert!(watch.reap(&mut child).unwrap().success());
    assert!(child.wait().unwrap().success());
}

/// Another child's exit during the wait neither ends it early nor reaps the wrong child.
#[cfg(unix)]
#[test]
fn another_child_s_exit_wakes_no_wrong_wait() {
    let mut first = lasting("0.3");
    let mut second = quick();
    let mut watch = watch(&mut first).unwrap();
    let start = Instant::now();
    assert!(
        watch
            .exited_by(&first, start + Duration::from_secs(5))
            .unwrap()
    );
    assert!(start.elapsed() >= Duration::from_millis(250));
    assert!(watch.reap(&mut first).unwrap().success());
    assert!(second.wait().unwrap().success());
    assert!(reaped(&first));
    assert!(reaped(&second));
}

/// A deadline that falls at the exit reaps the child once either way, the exit heard or the
/// site's kill taken, fifty times over.
#[test]
fn a_deadline_at_the_exit_reaps_once_either_way() {
    for _ in 0..50 {
        let mut child = lasting("0.05");
        let mut watch = watch(&mut child).unwrap();
        let exited = watch
            .exited_by(&child, Instant::now() + Duration::from_millis(50))
            .unwrap();
        if !exited {
            let _ = child.kill();
        }
        let status = watch.reap(&mut child).unwrap();
        assert!(!exited || status.success());
        #[cfg(unix)]
        assert!(reaped(&child));
        assert_eq!(child.wait().unwrap(), status);
    }
}

/// A child std already reaped arms nothing, so no call names its number again, and the
/// watch reads the kept status at once.
#[test]
fn a_child_already_reaped_arms_nothing() {
    let mut child = quick();
    let kept = child.wait().unwrap();
    let before = hooks::arms();
    let mut watch = watch(&mut child).unwrap();
    assert_eq!(hooks::arms(), before);
    assert!(watch.exited_by(&child, Instant::now()).unwrap());
    assert_eq!(watch.reap(&mut child).unwrap(), kept);
}

/// The deadline passes with the child running and nothing reaped; the kill that follows is
/// reaped through the watch.
#[test]
fn a_running_child_meets_the_deadline_unreaped() {
    let mut child = lasting("30");
    let mut watch = watch(&mut child).unwrap();
    let start = Instant::now();
    assert!(
        !watch
            .exited_by(&child, start + Duration::from_millis(100))
            .unwrap()
    );
    assert!(start.elapsed() >= Duration::from_millis(100));
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
    assert!(!watch.reap(&mut child).unwrap().success());
}

/// The reap waits for the waiter's answer: a waiter held before its `waitid` until after
/// the kill still hears the exit, not a child already gone, so it never waits on a number
/// that was reaped and given to another process.
#[cfg(unix)]
#[test]
fn the_reap_follows_the_waiter_s_answer() {
    waiter_hooks(true, None);
    let mut child = lasting("30");
    let watch = watch(&mut child).unwrap();
    waiter_hooks(false, None);
    child.kill().unwrap();
    let (heard, status) = watch.reap_heard(&mut child);
    assert_eq!(heard, Some(Heard::Exited));
    assert!(!status.unwrap().success());
    assert!(reaped(&child));
}

/// One watch waited on again and again keeps one waiter thread, gone once the child exits.
#[cfg(unix)]
#[test]
fn a_kept_watch_never_starts_a_second_waiter() {
    let token = std::sync::Arc::new(());
    waiter_hooks(false, Some(token.clone()));
    let mut child = lasting("30");
    let mut watch = watch(&mut child).unwrap();
    waiter_hooks(false, None);
    for _ in 0..5 {
        assert!(
            !watch
                .exited_by(&child, Instant::now() + Duration::from_millis(10))
                .unwrap()
        );
    }
    assert_eq!(std::sync::Arc::strong_count(&token), 2);
    child.kill().unwrap();
    assert!(!watch.reap(&mut child).unwrap().success());
    let gone = crate::poll_support::poll_until(Instant::now() + Duration::from_secs(10), || {
        std::sync::Arc::strong_count(&token) == 1
    });
    assert!(gone);
}

/// A watch dropped without a reap returns at once, reaps nothing, and its waiter thread
/// ends when the child exits, before anyone reaps it.
#[cfg(unix)]
#[test]
fn a_dropped_watch_blocks_nobody_and_its_waiter_ends_at_the_exit() {
    let token = std::sync::Arc::new(());
    waiter_hooks(false, Some(token.clone()));
    let mut child = lasting("0.2");
    let watch = watch(&mut child).unwrap();
    waiter_hooks(false, None);
    let start = Instant::now();
    drop(watch);
    assert!(start.elapsed() < Duration::from_millis(50));
    assert!(child.try_wait().unwrap().is_none());
    let gone = crate::poll_support::poll_until(Instant::now() + Duration::from_secs(10), || {
        std::sync::Arc::strong_count(&token) == 1
    });
    assert!(gone);
    assert!(child.wait().unwrap().success());
}

/// A wait that also watches a descriptor wakes when it is readable, the child still
/// running, then hears the exit the kill makes.
#[cfg(unix)]
#[test]
fn a_readable_descriptor_wakes_the_wait_first() {
    let (reader, mut writer) = crate::spawn_gate::guarded(std::io::pipe).unwrap();
    let mut child = lasting("30");
    let mut watch = watch(&mut child).unwrap();
    writer.write_all(b"out").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let woke = watch
        .woke_by(&child, Some(reader.as_fd()), deadline)
        .unwrap();
    assert_eq!(woke, Woke::Readable);
    child.kill().unwrap();
    let woke = watch.woke_by(&child, None, deadline).unwrap();
    assert_eq!(woke, Woke::Exited);
    assert!(!watch.reap(&mut child).unwrap().success());
}

/// A refused process descriptor falls back to a waiter thread that hears the exit, and the
/// fallback is said once per process however often it happens.
#[cfg(target_os = "linux")]
#[test]
fn a_refused_process_descriptor_falls_back_and_says_so_once() {
    use std::sync::atomic::Ordering;
    hooks::refuse_pidfd(Some(Errno::PERM));
    for _ in 0..2 {
        let mut child = quick();
        let mut watch = watch(&mut child).unwrap();
        assert!(matches!(watch.arm, Arm::Waiter(_)));
        assert!(
            watch
                .exited_by(&child, Instant::now() + Duration::from_secs(30))
                .unwrap()
        );
        assert!(watch.reap(&mut child).unwrap().success());
    }
    hooks::refuse_pidfd(None);
    assert_eq!(FALLBACK_SAID.load(Ordering::Acquire), 1);
}

/// With process descriptors open, Linux waits on one and starts no thread.
#[cfg(target_os = "linux")]
#[test]
fn linux_waits_on_a_process_descriptor() {
    let mut child = lasting("0.1");
    let mut watch = watch(&mut child).unwrap();
    assert!(matches!(watch.arm, Arm::Pidfd(_)));
    assert!(
        watch
            .exited_by(&child, Instant::now() + Duration::from_secs(30))
            .unwrap()
    );
    assert!(watch.reap(&mut child).unwrap().success());
}
