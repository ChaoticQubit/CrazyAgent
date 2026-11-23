use super::*;
use crate::terminal::tmux::test_seams::test_confinement;

#[test]
fn an_unconfined_editor_command_is_refused() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exit 0"]);
    let started = spawn(&mut command, &test_confinement(), true);
    let refused = match started {
        Ok(mut child) => {
            let _ = child.wait();
            None
        }
        Err(code) => Some(code),
    };
    assert_eq!(refused, Some(NOT_CONFINED));
}

#[test]
fn a_command_through_the_confinement_runner_is_not_refused_as_unconfined() {
    let confinement = test_confinement();
    let mut command = Command::new(confinement.sandbox_exec);
    command.args(["-n", "no-network", "/usr/bin/true"]);
    match spawn(&mut command, &confinement, true) {
        Ok(mut child) => {
            let _ = child.wait();
        }
        Err(code) => assert_eq!(code, SPAWN_FAILED),
    }
}

#[test]
fn no_refusal_where_confinement_is_not_required() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exit 7"]);
    let mut child = spawn(&mut command, &test_confinement(), false).expect("the command started");
    let status = child.wait().expect("the command ended");
    assert_eq!(status.code(), Some(7));
}
