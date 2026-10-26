use super::{gone, start_identity};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

/// Starts a real `sleep` that outlives the test unless killed, in its own process group
/// when asked, the way the engine starts a master harness. Input: whether it leads its own
/// group. Output: the child and its process id.
fn sleeper(own_group: bool) -> (Child, i32) {
    let mut command = Command::new("/bin/sleep");
    command
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if own_group {
        command.process_group(0);
    }
    let child = crate::spawn_gate::spawn(&mut command).unwrap();
    let pid = i32::try_from(child.id()).unwrap();
    assert!(pid > 1);
    (child, pid)
}

/// Kills and reaps a test child so no process outlives its test. Input: the child.
/// Output: none; the child is reaped.
fn end(mut child: Child) {
    let _ = child.kill();
    child.wait().unwrap();
}

#[test]
fn a_live_recorded_group_is_not_gone() {
    let (child, group) = sleeper(true);
    let started = start_identity(group).unwrap();
    assert!(started.is_some(), "a live process has a start identity");
    let result = gone(group, started.as_deref());
    end(child);
    assert!(!result.unwrap());
}

#[test]
fn a_killed_and_reaped_group_is_gone() {
    let (child, group) = sleeper(true);
    let started = start_identity(group).unwrap();
    end(child);
    assert_eq!(start_identity(group).unwrap(), None);
    assert!(gone(group, started.as_deref()).unwrap());
}

#[test]
fn a_live_group_under_the_recorded_number_is_not_gone_whatever_its_start() {
    let (child, group) = sleeper(true);
    let result = gone(group, Some("Thu Jan  1 00:00:00 1970"));
    end(child);
    assert!(!result.unwrap());
}

#[test]
fn a_leader_alive_outside_its_group_is_not_gone() {
    let (child, pid) = sleeper(false);
    let started = start_identity(pid).unwrap();
    assert!(started.is_some());
    let result = gone(pid, started.as_deref());
    end(child);
    assert!(!result.unwrap());
}

#[test]
fn a_reused_id_that_leads_no_group_and_started_later_leaves_the_record_gone() {
    let (child, pid) = sleeper(false);
    let result = gone(pid, Some("Thu Jan  1 00:00:00 1970"));
    let unknown = gone(pid, None);
    end(child);
    assert!(result.unwrap());
    assert!(
        !unknown.unwrap(),
        "an unknown recorded start cannot prove a live process is a reuse"
    );
}
