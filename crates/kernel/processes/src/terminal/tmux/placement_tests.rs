use super::*;
use crate::terminal::tmux::test_seams::{test_confinement, test_hooks};

/// Ends a row's tmux server when the row ends, passed or failed.
struct Ended {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Drop for Ended {
    fn drop(&mut self) {
        let _ = crate::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

/// A fresh host whose control made an agent's window running `cat`. Input: the row's home
/// folder. Output: the host, the guard that ends its server, the control and the agent.
fn window_made(home: &Path) -> (Tmux, Ended, Arc<Control>, AgentId) {
    let folder = home.join("terminal");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&folder)
        .unwrap();
    let tmux = installed(home).expect("tmux is required for this test");
    let host = Tmux::open(
        Some(tmux.clone()),
        &folder,
        test_confinement(),
        test_hooks(),
    );
    let ended = Ended {
        tmux,
        socket: host.socket(),
    };
    let control = host.control().unwrap();
    let agent = AgentId::team(1).unwrap();
    control
        .run(&format!("new-window -d -t {SESSION}: -n {agent} cat"))
        .unwrap();
    assert_eq!(windows(&control).unwrap(), std::slice::from_ref(&agent));
    (host, ended, control, agent)
}

/// Makes an agent's window on a fresh host, then hands `settle` a reply whose placement
/// line cannot be read. Input: the reply line. Output: the code and the agents whose
/// windows are left.
fn settled_with(reply: &str) -> (HostError, Vec<AgentId>) {
    let home = tempfile::tempdir().unwrap();
    let (host, _ended, control, agent) = window_made(home.path());
    let reply = vec![format!("{reply}{FORMAT_END}").into_bytes()];
    let refused = host.settle(&control, &agent, Ok(reply)).unwrap_err();
    (refused, windows(&control).unwrap())
}

/// A placement reply that timed out, which closed its control as `Control::wait` does,
/// still leaves no agent window, so a second spawn of that agent is not refused.
#[test]
fn a_placement_reply_that_timed_out_leaves_no_agent_window() {
    let home = tempfile::tempdir().unwrap();
    let (host, _ended, control, agent) = window_made(home.path());
    control.stop();

    let refused = host
        .settle(&control, &agent, Err(failure("terminal_not_answering")))
        .unwrap_err();

    assert_eq!(refused, failure("terminal_not_answering"));
    let open = host.control_with(false).unwrap();
    assert!(windows(&open).unwrap().is_empty());
    let command = PaneCommand {
        argv: vec!["/bin/sleep".into(), "600".into()],
        cwd: home.path().to_path_buf(),
        env: Vec::new(),
    };
    assert!(host.spawn(&agent, &command).is_ok());
}

/// A placement line with no pane and process id pair leaves no agent window behind.
#[test]
fn a_placement_line_of_another_shape_leaves_no_agent_window() {
    let (refused, left) = settled_with("no-pair");
    assert_eq!(refused, failure("terminal_command_failed"));
    assert!(left.is_empty(), "{left:?}");
}

/// A placement line whose process id does not parse leaves no agent window behind.
#[test]
fn a_placement_line_with_a_bad_process_id_leaves_no_agent_window() {
    let (refused, left) = settled_with("%1 pid");
    assert_eq!(refused, failure("terminal_command_failed"));
    assert!(left.is_empty(), "{left:?}");
}
