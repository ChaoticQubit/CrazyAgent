use super::*;
use crate::engine::Client;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::OwnerClientKind;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::team::master::{OwnerTyping, owner_key_ms};
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use crate::terminal::{FOLDER, Host};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);

struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Drop for Server {
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

async fn closed(output: &mut TerminalOutput) -> String {
    tokio::time::timeout(WAIT, async {
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

/// A sized attach is only an opening request: sent inside a live session it ends that
/// session as an invalid request and never resizes the agent's window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_sized_mid_session_is_refused() {
    let tmp = tempfile::Builder::new()
        .prefix("ca-term-sized-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root = tmp.path();
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.join("g.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 2000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let tmux = installed(root).expect("tmux must be installed on this machine");
    let folder = root.join(FOLDER);
    let _server = Server {
        tmux: tmux.clone(),
        socket: folder.join(SOCKET),
    };
    let agent = AgentId::team(1).unwrap();
    let command = InteractiveCommand {
        argv: vec!["/bin/sh".into()],
        cwd: root.to_path_buf(),
        env: vec![("PS1".into(), "ready$ ".into())],
        transcript: Transcript::Unavailable,
    };
    let _launcher = {
        let (agent, folder) = (agent.clone(), folder.clone());
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
        root,
        "e.sock",
        &global,
        &root.join("p.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let (_, owner, _) = Client::new(root.join("e.sock"), WAIT)
        .unwrap()
        .with_owner(OwnerClientKind::Tui)
        .await
        .unwrap();

    let TerminalSession {
        cols,
        rows,
        mut output,
        mut control,
        ..
    } = owner.terminal_attach(agent.clone()).await.unwrap();
    assert_eq!((cols, rows), (80, 24));
    control
        .send(&TerminalRequest::AttachSized {
            agent_id: agent.clone(),
            pane: None,
            cols: 120,
            rows: 40,
        })
        .await
        .unwrap();
    assert_eq!(closed(&mut output).await, "terminal_request_invalid");

    let TerminalSession { cols, rows, .. } = owner.terminal_attach(agent).await.unwrap();
    assert_eq!(
        (cols, rows),
        (80, 24),
        "a refused sized attach left the window as it was"
    );

    drop(owner);
    tokio::time::timeout(WAIT, service.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn an_owner_prompt_counts_as_an_owner_key_as_typed_keys_do() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("engine.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    crate::team::initialize(&connection).unwrap();
    let typing = OwnerTyping::new(database.clone());
    let requests = [
        (7, TerminalRequest::Prompt { text: "2".into() }),
        (
            8,
            TerminalRequest::Input {
                bytes: b"2\r".to_vec(),
            },
        ),
        (
            9,
            TerminalRequest::Resize {
                cols: 100,
                rows: 30,
            },
        ),
        (10, TerminalRequest::Detach),
    ];
    let at = crate::team::now_ms();
    for (agent, request) in &requests {
        if let Some(keys) = owner_keys(request) {
            typing.record(*agent, keys, at);
        }
    }
    let keyed = |agent: i64| owner_key_ms(&connection, Some(&typing), agent).unwrap();
    assert_eq!(
        keyed(7),
        Some(at),
        "a prompt the owner sends from the terminal panel answers a box on that screen as the owner"
    );
    assert_eq!(keyed(8), Some(at));
    assert_eq!((keyed(9), keyed(10)), (None, None));
}
