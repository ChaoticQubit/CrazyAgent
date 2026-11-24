/// Starts `program` with `args` as the leader of a process group of its own. Inputs: the
/// program and its arguments. Output: the child and its group as a pid.
#[cfg(unix)]
fn group_leader(program: &str, args: &[&str]) -> (std::process::Child, rustix::process::Pid) {
    use std::os::unix::process::CommandExt;
    let child = kernel_processes::spawn_gate::spawn(
        std::process::Command::new(program)
            .args(args)
            .process_group(0)
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    let group = rustix::process::Pid::from_child(&child);
    (child, group)
}

/// CB-2 one signal: a pane group that already answered ESRCH gets no KILL from the stop
/// that ends it, since its number may lead a new group by then. macOS answers EPERM for a
/// group whose last member it just reaped, so the test first waits for ESRCH.
#[cfg(unix)]
#[test]
fn a_pane_group_that_answered_esrch_is_never_signalled() {
    let (mut ended, group) = group_leader("/usr/bin/true", &[]);
    assert!(ended.wait().unwrap().success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while rustix::process::test_kill_process_group(group) != Err(rustix::io::Errno::SRCH) {
        assert!(
            std::time::Instant::now() < deadline,
            "the reaped group answers ESRCH"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let calls = std::cell::RefCell::new(Vec::new());

    let gone = super::end_pane_group_with(
        kernel_processes::group::Ending::claim(ended.id()),
        std::time::Duration::from_secs(1),
        || {
            let answer = rustix::process::test_kill_process_group(group);
            calls.borrow_mut().push(("test", answer));
            answer != Err(rustix::io::Errno::SRCH)
        },
        || calls.borrow_mut().push(("kill", Ok(()))),
    );
    assert!(gone, "a group that answered ESRCH is gone");
    assert_eq!(
        calls.into_inner(),
        [("test", Err(rustix::io::Errno::SRCH))],
        "the gone group is tested once and never signalled"
    );
}

/// CB-2 one signal: a stop that finds its pane's group held by another path, as the
/// terminal host's close of that pane, only watches the group and never signals it, so the
/// group is signalled from one path only. Once the hold is gone a stop ends the group.
#[cfg(unix)]
#[test]
fn a_pane_group_another_path_holds_is_only_watched() {
    use std::os::unix::process::ExitStatusExt;
    let (mut survivor, group) = group_leader("/bin/sleep", &["120"]);
    let id = survivor.id();
    let other = kernel_processes::group::Ending::claim(id).expect("no path holds a new group");
    assert!(
        kernel_processes::group::Ending::claim(id).is_none(),
        "a held group has one holder"
    );
    assert!(
        kernel_processes::group::Ending::claim(id).is_none(),
        "a refused claim leaves the holder's hold in place"
    );
    let reaped = std::thread::spawn(move || survivor.wait());
    let kills = std::cell::Cell::new(0);

    let gone = super::end_pane_group_with(
        None,
        std::time::Duration::from_millis(300),
        || rustix::process::test_kill_process_group(group) != Err(rustix::io::Errno::SRCH),
        || kills.set(kills.get() + 1),
    );
    drop(other);
    let ended = super::end_pane_group(id, kernel_processes::group::Ending::claim(id));
    if !ended {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
    let status = reaped.join().unwrap().unwrap();
    assert!(!gone, "the held group still runs");
    assert_eq!(
        kills.get(),
        0,
        "a stop never signals a group another path holds"
    );
    assert!(
        ended,
        "a hold freed before its group ended leaves the group claimable, and a stop ends it"
    );
    assert_eq!(status.signal(), Some(9), "the group ended by KILL");
}

/// CB-2 r1 F1: once a holder saw its group answer ESRCH, the number stays refused for a
/// while, so no later path, such as a session that notices its pane gone, claims and
/// signals a number a new group may lead by then.
#[cfg(unix)]
#[test]
fn a_group_its_holder_saw_end_stays_refused_to_later_paths() {
    use std::os::unix::process::ExitStatusExt;
    let (mut survivor, _) = group_leader("/bin/sleep", &["120"]);
    let id = survivor.id();
    let held = kernel_processes::group::Ending::claim(id).expect("no path holds a new group");
    let reaped = std::thread::spawn(move || survivor.wait());

    let ended = super::end_pane_group(id, Some(held));
    let later = kernel_processes::group::Ending::claim(id);
    let status = reaped.join().unwrap().unwrap();
    assert!(ended, "the holder ends its group");
    assert_eq!(status.signal(), Some(9), "the group ended by KILL");
    assert!(
        later.is_none(),
        "a number whose group just ended is refused to a later path"
    );
}

/// CB-2 r1 F1: a path that has no hold, because another path held the group or just ended
/// it, only watches the group for the wait and never claims or signals it itself.
#[cfg(unix)]
#[test]
fn a_path_without_a_hold_only_watches_and_never_claims() {
    use std::os::unix::process::ExitStatusExt;
    let (mut survivor, group) = group_leader("/bin/sleep", &["120"]);
    let id = survivor.id();
    let reaped = std::thread::spawn(move || survivor.wait());
    let started = std::time::Instant::now();

    let ended = super::end_pane_group(id, None);
    let watched = started.elapsed();
    if rustix::process::test_kill_process_group(group) != Err(rustix::io::Errno::SRCH) {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
    let status = reaped.join().unwrap().unwrap();
    assert!(!ended, "a path without a hold never ends the group itself");
    assert!(
        watched >= super::GROUP_EXIT_WAIT,
        "it watched the whole wait: {watched:?}"
    );
    assert_eq!(
        status.signal(),
        Some(9),
        "the group ran until the test ended it"
    );
}
