#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::owner::{OwnerResult, TerminalOutput, TerminalSession};
use crazyagent::harnesses::interactive::{InteractiveCommand, Transcript};
use crazyagent::protocol::ProjectId;
use crazyagent::team::NodeKind;
use crazyagent::terminal::tmux::{SOCKET, Tmux, installed};
use crazyagent::terminal::{AgentId, FOLDER, Host, TerminalFrame};
use e2e_support::{Engine, Owner, TMUX, hex, scratch};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

const FRAME_TIME: Duration = Duration::from_secs(10);

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
    tokio::time::timeout(FRAME_TIME, async {
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
    tokio::time::timeout(FRAME_TIME, async {
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
async fn the_owner_types_into_an_engine_terminal_and_the_team_graph_shows_the_master() {
    let mut engine = Engine::new("ca-e2e-team-");
    let tmux = installed(engine.path()).expect(TMUX);
    let folder = engine.path().join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let agent = AgentId::team(1).unwrap();
    let command = InteractiveCommand {
        argv: vec!["/bin/sh".into()],
        cwd: engine.path().to_path_buf(),
        env: vec![("PS1".into(), "e2e$ ".into())],
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
    engine.launch();
    let mut owner = Owner::attach(&engine).await;

    let TerminalSession {
        agent: attached,
        cols,
        rows,
        mut output,
        mut control,
    } = owner
        .controller
        .terminal_attach(agent.clone())
        .await
        .unwrap();
    assert_eq!((attached, cols, rows), (agent.clone(), 80, 24));
    let mut seen = Vec::new();
    assert!(
        read_until(&mut output, &mut seen, b"e2e$ ").await,
        "the screen bytes show the prompt: {}",
        String::from_utf8_lossy(&seen)
    );
    let marker = hex(4);
    let typed = format!("echo e2e-{marker}-$((6*7))");
    control
        .input(format!("{typed}\r").into_bytes())
        .await
        .unwrap();
    assert!(
        read_until(&mut output, &mut seen, typed.as_bytes()).await,
        "the typed input echoes back: {}",
        String::from_utf8_lossy(&seen)
    );
    assert!(
        read_until(
            &mut output,
            &mut seen,
            format!("\r\ne2e-{marker}-42\r\n").as_bytes()
        )
        .await,
        "the shell ran the typed line: {}",
        String::from_utf8_lossy(&seen)
    );
    control.resize(100, 30).await.unwrap();
    control.input(b"stty size\r".to_vec()).await.unwrap();
    assert!(
        read_until(&mut output, &mut seen, b"\r\n30 100\r\n").await,
        "the resize reached the pane: {}",
        String::from_utf8_lossy(&seen)
    );
    control.detach().await.unwrap();
    assert_eq!(closed(&mut output).await, "terminal_detached");

    let TerminalSession {
        cols,
        rows,
        mut output,
        control: _attached,
        ..
    } = owner
        .controller
        .terminal_attach(agent.clone())
        .await
        .unwrap();
    assert_eq!((cols, rows), (100, 30));
    let mut screen = Vec::new();
    assert!(
        read_until(
            &mut output,
            &mut screen,
            format!("e2e-{marker}-42").as_bytes()
        )
        .await,
        "the screen on attach shows what ran before: {}",
        String::from_utf8_lossy(&screen)
    );
    assert_eq!(
        owner
            .controller
            .terminal_attach(AgentId::team(9).unwrap())
            .await
            .err()
            .map(|error| error.0),
        Some("terminal_agent_unknown".into())
    );

    let disk = scratch("ca-e2e-team-home-");
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
    assert_eq!(
        owner.create_project("e2e-team", "Tidepool team").await,
        OwnerResult::Applied
    );
    let project = ProjectId::new("e2e-team").unwrap();
    let graph = owner.controller.team_graph(project.clone()).await.unwrap();
    assert_eq!(graph.project_id, project);
    let master = graph
        .nodes
        .first()
        .expect("the graph always has the master");
    assert_eq!(master.kind, NodeKind::Master);
    assert_eq!(master.label, "Master");
    assert_eq!(master.parent, None);
    assert_eq!(
        master.terminal, None,
        "no harness runs the master yet, so it has no terminal to open"
    );
    assert!(
        graph.nodes.iter().all(|node| node.kind != NodeKind::Agent),
        "{graph:?}"
    );
    assert!(graph.edges.is_empty(), "{graph:?}");
    assert!(
        owner
            .controller
            .team_status(project)
            .await
            .unwrap()
            .agents
            .is_empty()
    );

    tokio::task::spawn_blocking(move || launcher.kill(&agent))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(closed(&mut output).await, "terminal_ended");
    owner.controller.detach();
    engine.stop();
}
