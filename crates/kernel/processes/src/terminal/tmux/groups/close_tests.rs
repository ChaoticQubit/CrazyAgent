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

/// A setup step's error, which fails the row with its message.
type Failed = Box<dyn std::error::Error>;

/// A host on a fresh private folder whose one agent runs `sleep`. Input: the row's home
/// folder. Output: the host, the guard that ends its server and the agent, or the first
/// setup step that failed.
fn sleeping_pane(home: &Path) -> Result<(Tmux, Ended, AgentId), Failed> {
    let folder = home.join("terminal");
    std::fs::DirBuilder::new().mode(0o700).create(&folder)?;
    let tmux = installed(home).ok_or("tmux is required for this test")?;
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
    let agent = AgentId::team(1)?;
    let command = PaneCommand {
        argv: vec!["/bin/sleep".into(), "600".into()],
        cwd: home.to_path_buf(),
        env: Vec::new(),
    };
    host.spawn(&agent, &command)?;
    Ok((host, ended, agent))
}

/// Whether a process still runs. Input: its pid. Output: true while it answers signal 0.
fn running(pid: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    rustix::process::test_kill_process(pid).is_ok()
}

/// A close whose command never reached tmux, since its control client stopped, leaves the
/// pane open and its processes running.
#[test]
fn a_close_never_sent_leaves_the_panes_processes_running() -> Result<(), Failed> {
    let home = tempfile::tempdir()?;
    let (host, _ended, agent) = sleeping_pane(home.path())?;
    let panes = host.panes(&agent)?;
    assert_eq!(panes.len(), 1);
    let control = host.live_control()?;
    control.stop();

    let closed = host.close_panes(
        &control,
        &panes,
        &format!("kill-window -t {}", target(&agent)),
    );

    assert!(closed.is_err());
    host.join_reapers();
    assert!(running(panes[0].pid), "a pane tmux kept lost its processes");
    Ok(())
}

/// A close tmux refused, so its pane stays open, leaves that pane's processes running.
#[test]
fn a_close_tmux_refused_leaves_the_open_panes_processes_running() -> Result<(), Failed> {
    let home = tempfile::tempdir()?;
    let (host, _ended, agent) = sleeping_pane(home.path())?;
    let panes = host.panes(&agent)?;
    assert_eq!(panes.len(), 1);
    let control = host.live_control()?;

    let closed = host.close_panes(
        &control,
        &panes,
        "kill-window -t crazyagent:=no-such-window",
    );

    assert!(closed.is_err());
    host.join_reapers();
    assert!(running(panes[0].pid), "a pane tmux kept lost its processes");
    Ok(())
}
