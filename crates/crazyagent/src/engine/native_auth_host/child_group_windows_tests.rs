use super::child_group::{ChildGroup, DROP_TIME, cleanup_owned_group, reaped_group_gone};
use super::custodian_windows_tests::{all_end_within, ping_child_of, runs};
use super::login_env::login_child;
use super::login_windows_tests::{cmd, profile_home};
use crazyagent_win_sandbox::ProcessTree;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const WITH_GRANDCHILD: &str = "start /b ping -n 60 127.0.0.1 >NUL & ping -n 60 127.0.0.1 >NUL";
const LEAVES_GRANDCHILD: &str = "start /b ping -n 60 127.0.0.1 >NUL";
const TREE_END_BOUND: Duration = Duration::from_secs(2);
const CLEANUP_SLACK: Duration = Duration::from_millis(500);

/// A fake login child, `cmd` running the script, built by the one builder and held in its
/// tree from the start. Input: the script. Output: its profile home folder, the child and
/// its tree.
fn held(script: &str) -> (tempfile::TempDir, Child, ChildGroup) {
    let (folder, home) = profile_home();
    let mut command = Command::new(cmd());
    command.args(["/c", script]);
    login_child(&mut command, &home, "CODEX_HOME");
    command.stdin(Stdio::null());
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

/// N4 row 1: cleanup of a held login child that started a grandchild ends both within the
/// cleanup bound, and leaves no process in the tree.
#[test]
fn cleanup_ends_a_held_login_child_and_its_grandchild_within_the_bound() {
    let (_folder, mut child, tree) = held(WITH_GRANDCHILD);
    let grandchild = ping_child_of(child.id());
    let started = Instant::now();
    let cleaned = cleanup_owned_group(&mut child, &tree, started + DROP_TIME).unwrap();
    assert!(started.elapsed() <= DROP_TIME + CLEANUP_SLACK);
    assert!(
        all_end_within(&[(grandchild, "PING.EXE")], Duration::ZERO),
        "the grandchild outlived cleanup"
    );
    assert!(cleaned);
    assert_eq!(tree.active_processes().unwrap(), 0);
}

/// N4 row 2: dropping a login child's held tree ends the child and its grandchild within
/// two seconds, so no path that loses the tree leaves either running.
#[test]
fn dropping_the_held_tree_ends_the_login_child_and_its_grandchild() {
    let (_folder, mut child, tree) = held(WITH_GRANDCHILD);
    let parent = child.id();
    let grandchild = ping_child_of(parent);
    assert!(runs(parent, "cmd.exe"));
    drop(tree);
    assert!(all_end_within(
        &[(parent, "cmd.exe"), (grandchild, "PING.EXE")],
        TREE_END_BOUND
    ));
    assert!(!child.wait().unwrap().success());
}

/// N4 row 6: after the login child is reaped, its group reads as present while a
/// grandchild still runs, and as gone once the tree has ended.
#[test]
fn a_reaped_child_group_reads_present_while_a_grandchild_lives_and_gone_once_ended() {
    let (_folder, mut child, tree) = held(LEAVES_GRANDCHILD);
    let grandchild = ping_child_of(child.id());
    assert!(child.wait().unwrap().success());
    assert!(runs(grandchild, "PING.EXE"));
    assert!(
        !reaped_group_gone(&tree),
        "a living grandchild read as a gone group"
    );
    let started = Instant::now();
    while !tree.ended().unwrap() {
        assert!(started.elapsed() < TREE_END_BOUND);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!runs(grandchild, "PING.EXE"));
    assert!(reaped_group_gone(&tree));
}
