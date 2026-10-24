#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, TerminalOutput, TerminalSession};
use crazyagent::harnesses::interactive::{InteractiveCommand, Transcript};
use crazyagent::terminal::tmux::{SOCKET, Tmux, installed};
use crazyagent::terminal::{AgentId, FOLDER, Host, TerminalFrame};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new(&self.tmux)
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn contains(seen: &[u8], wanted: &[u8]) -> bool {
    seen.windows(wanted.len()).any(|window| window == wanted)
}

async fn read_until(output: &mut TerminalOutput, seen: &mut Vec<u8>, wanted: &[u8]) -> bool {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !contains(seen, wanted) {
            match output.next().await {
                Ok(TerminalFrame::Output { bytes }) => seen.extend_from_slice(&bytes),
                _ => return false,
            }
        }
        true
    })
    .await
    .unwrap_or(false)
}

async fn closed(output: &mut TerminalOutput) -> String {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match output.next().await {
                Ok(TerminalFrame::Output { .. }) => {}
                Ok(TerminalFrame::Closed { reason }) => return reason,
                other => return format!("{other:?}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| "no close within ten seconds".into())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_attaches_to_an_agent_terminal_over_the_engine_socket() {
    let root = tempfile::Builder::new()
        .prefix("ca-term-owner-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.path().join("g.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 2000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let tmux = installed(root.path()).expect("tmux must be installed on this machine");
    let folder = root.path().join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let agent = AgentId::team(1).unwrap();
    let command = InteractiveCommand {
        argv: vec!["/bin/sh".into()],
        cwd: root.path().to_path_buf(),
        env: vec![("PS1".into(), "ready$ ".into())],
        transcript: Transcript::Unavailable,
    };
    let launcher = {
        let agent = agent.clone();
        let folder = folder.clone();
        tokio::task::spawn_blocking(move || {
            let launcher = Tmux::open(Some(tmux), &folder);
            launcher.spawn(&agent, &command).unwrap();
            launcher
        })
        .await
        .unwrap()
    };

    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("p.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let mut controller =
        OwnerController::attach(root.path(), OwnerClientKind::Tui, Duration::from_secs(5))
            .await
            .unwrap();

    let TerminalSession {
        agent: attached,
        cols,
        rows,
        mut output,
        mut control,
    } = controller.terminal_attach(agent.clone()).await.unwrap();
    assert_eq!((attached, cols, rows), (agent.clone(), 80, 24));
    let mut seen = Vec::new();
    assert!(read_until(&mut output, &mut seen, b"ready$ ").await);
    control.input(b"echo hi\r".to_vec()).await.unwrap();
    assert!(
        read_until(&mut output, &mut seen, b"\r\nhi\r\n").await,
        "{}",
        String::from_utf8_lossy(&seen)
    );
    control.resize(100, 30).await.unwrap();
    control.detach().await.unwrap();
    assert_eq!(closed(&mut output).await, "terminal_detached");

    let TerminalSession {
        cols,
        rows,
        mut output,
        control: _attached,
        ..
    } = controller.terminal_attach(agent.clone()).await.unwrap();
    assert_eq!(
        (cols, rows),
        (100, 30),
        "the resize reached the agent's terminal"
    );
    let mut screen = Vec::new();
    assert!(
        read_until(&mut output, &mut screen, b"echo hi\r\nhi").await,
        "the screen on attach shows what ran before: {}",
        String::from_utf8_lossy(&screen)
    );

    assert_eq!(
        controller
            .terminal_attach(AgentId::team(9).unwrap())
            .await
            .err()
            .map(|error| error.0),
        Some("terminal_agent_unknown".into())
    );

    tokio::task::spawn_blocking(move || launcher.kill(&agent))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(closed(&mut output).await, "terminal_ended");

    drop(controller);
    tokio::time::timeout(Duration::from_secs(10), service.shutdown())
        .await
        .unwrap()
        .unwrap();
}
