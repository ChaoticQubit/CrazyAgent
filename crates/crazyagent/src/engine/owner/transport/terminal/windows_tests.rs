use super::*;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::terminal::Host;
use tokio::io::AsyncReadExt;
use tokio::sync::oneshot;

const WAIT: Duration = Duration::from_secs(20);
const SHELL: &str = r"C:\Windows\System32\cmd.exe";
const GRANT_CHECK_ROW: Duration = Duration::from_secs(5);

/// A running owner server on the owner pipe in a fresh temp folder, the engine's ConPTY host
/// with one owner shell started in it, and a desktop client admitted to it, as an owner
/// terminal opens on Windows. Fields keep the temp folder and the server task alive.
struct Shell {
    _temp: tempfile::TempDir,
    engine: Arc<Engine>,
    owners: PathBuf,
    host: Arc<dyn Host>,
    agent: AgentId,
    client: OwnerClient,
    stop: Option<oneshot::Sender<()>>,
    running: Option<tokio::task::JoinHandle<Result<(), EngineError>>>,
}

impl Shell {
    /// Starts the server, opens the engine's terminal host, starts `cmd.exe` as team 1 in
    /// it and admits a desktop client. Output: the running shell.
    async fn start() -> Shell {
        let temp = tempfile::tempdir().unwrap();
        let full = std::fs::canonicalize(temp.path()).unwrap();
        let plain = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
        let global = plain.join("g");
        std::fs::write(&global, "").unwrap();
        let config = crate::config::resolve_config(&global, &plain.join("p")).unwrap();
        let engine = Arc::new(
            Engine::open(&plain.join("db"), config.clone())
                .await
                .unwrap(),
        );
        let owners = plain.join("gen");
        drop(crazyagent_win_sandbox::private::dir(&owners).unwrap());
        let (server, _host) = OwnerServer::bind(&owners, engine.clone(), &config)
            .await
            .unwrap();
        let (stop, stopped) = oneshot::channel();
        let running = tokio::spawn(server.run(stopped));
        let host = engine
            .terminal(&plain)
            .expect("the engine opens its ConPTY host on Windows");
        let agent = AgentId::team(1).unwrap();
        let command = InteractiveCommand {
            argv: vec![SHELL.into(), "/D".into(), "/Q".into()],
            cwd: plain.clone(),
            env: Vec::new(),
            transcript: Transcript::Unavailable,
        };
        {
            let (host, agent) = (host.clone(), agent.clone());
            tokio::task::spawn_blocking(move || host.spawn(&agent, &command))
                .await
                .unwrap()
                .expect("the owner shell starts under ConPTY");
        }
        let client =
            OwnerClient::attach(&owners, OwnerClientKind::Desktop, Duration::from_secs(10))
                .unwrap();
        Shell {
            _temp: temp,
            engine,
            owners,
            host,
            agent,
            client,
            stop: Some(stop),
            running: Some(running),
        }
    }

    /// Stops the server and waits for it. Takes the shell. Output: none; panics on a server
    /// that ended with an error.
    async fn stop_server(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.send(()).unwrap();
        }
        if let Some(running) = self.running.take() {
            running.await.unwrap().unwrap();
        }
    }

    /// Ends the shell, the server and the engine. Takes the shell.
    async fn end(mut self) {
        let _ = self.host.kill(&self.agent);
        self.stop_server().await;
        self.engine.close().await.unwrap();
    }
}

/// Reads output frames until one holds `text` or the view closes. Takes the output and the
/// text. Output: whether it was seen within `WAIT`.
async fn seen(output: &mut TerminalOutput, text: &str) -> bool {
    let mut all = Vec::new();
    tokio::time::timeout(WAIT, async {
        loop {
            match output.next().await {
                Ok(TerminalFrame::Output { bytes }) => {
                    all.extend_from_slice(&bytes);
                    if String::from_utf8_lossy(&all).contains(text) {
                        return true;
                    }
                }
                _ => return false,
            }
        }
    })
    .await
    .unwrap_or(false)
}

/// Reads frames until the view closes. Takes the output and how long to wait. Output: the
/// close reason, or what came in its place.
async fn closed_within(output: &mut TerminalOutput, wait: Duration) -> String {
    tokio::time::timeout(wait, async {
        loop {
            match output.next().await {
                Ok(TerminalFrame::Output { .. }) => {}
                Ok(TerminalFrame::Closed { reason }) => return reason,
                other => return format!("{other:?}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| "no close in time".into())
}

/// T1: an owner terminal attach of an owner shell over the owner pipe gives a session with
/// the ConPTY size, where the client used to refuse with `terminal_owner_unavailable`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_shell_attaches_over_the_owner_pipe_with_its_size() {
    let shell = Shell::start().await;
    let session = shell.client.terminal_attach(shell.agent.clone()).await;
    let session = session
        .map_err(|error| error.0)
        .expect("the attach answers");
    assert_eq!(session.agent, shell.agent);
    assert_eq!((session.cols, session.rows), (80, 24));
    println!("w4 row: attach {}x{}", session.cols, session.rows);
    drop(session);
    shell.end().await;
}

/// T1: a pane attach of a pane the ConPTY host does not have gets the engine's own refusal,
/// not the client's `terminal_owner_unavailable`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pane_attach_of_an_unknown_pane_gets_the_engines_refusal() {
    let shell = Shell::start().await;
    let refused = shell
        .client
        .terminal_attach_pane(shell.agent.clone(), "%9".into())
        .await
        .err()
        .map(|error| error.0);
    println!("w4 row: unknown pane {refused:?}");
    assert!(
        matches!(
            refused.as_deref(),
            Some("terminal_pane_unknown" | "terminal_unavailable")
        ),
        "the engine refused the pane: {refused:?}"
    );
    shell.end().await;
}

/// T-R1: once the engine stops and a rogue publishes its own pipe for the owner endpoint, an
/// admitted client's terminal attach refuses before any byte, so the rogue never reads the
/// 37-byte packet with the capability.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rogue_owner_instance_gets_no_terminal_packet() {
    let mut shell = Shell::start().await;
    shell.stop_server().await;
    let rogue = crate::ipc::serve_unpinned(&shell.owners.join(SOCKET)).unwrap();
    let reading = tokio::spawn(async move {
        let mut rogue = rogue;
        let mut bytes = Vec::new();
        if tokio::time::timeout(Duration::from_secs(5), rogue.connect())
            .await
            .is_ok()
        {
            let _ =
                tokio::time::timeout(Duration::from_secs(5), rogue.read_to_end(&mut bytes)).await;
        }
        bytes
    });
    let refused = shell
        .client
        .terminal_attach(shell.agent.clone())
        .await
        .err()
        .map(|error| error.0);
    assert_eq!(refused.as_deref(), Some("owner_socket_replaced"));
    assert!(reading.await.unwrap().is_empty(), "the rogue read no byte");
    println!("w4 row: rogue refused");
    shell.end().await;
}

/// T2: a view of an owner shell types into it and sees the echo, resizes it, takes wheel
/// keys, and ends as `terminal_detached` on a detach; a later attach reports the new size.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_shell_view_types_resizes_scrolls_and_detaches() {
    let shell = Shell::start().await;
    let TerminalSession {
        mut output,
        mut control,
        ..
    } = shell
        .client
        .terminal_attach(shell.agent.clone())
        .await
        .map_err(|error| error.0)
        .unwrap();
    control
        .input(b"echo w4-typed-%COMSPEC:~0,1%\r".to_vec())
        .await
        .unwrap();
    assert!(seen(&mut output, "w4-typed-C").await, "the echo came back");
    control.resize(100, 30).await.unwrap();
    control.scroll(b"\x1b[A".to_vec()).await.unwrap();
    control.send(&TerminalRequest::Detach).await.unwrap();
    assert_eq!(closed_within(&mut output, WAIT).await, "terminal_detached");
    let again = shell
        .client
        .terminal_attach(shell.agent.clone())
        .await
        .map_err(|error| error.0)
        .unwrap();
    assert_eq!((again.cols, again.rows), (100, 30), "the resize took");
    println!("w4 row: typed, resized, scrolled, detached");
    drop(again);
    shell.end().await;
}

/// T-R2: a view whose desktop capability changes on disk mid-view ends as
/// `owner_grant_stale_or_revoked` at its next grant check, with no further output.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_grant_ends_an_owner_shell_view() {
    let shell = Shell::start().await;
    let TerminalSession { mut output, .. } = shell
        .client
        .terminal_attach(shell.agent.clone())
        .await
        .map_err(|error| error.0)
        .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(shell.owners.join("owner-desktop.cap"))
        .and_then(|mut file| std::io::Write::write_all(&mut file, &[0x5a; 32]))
        .expect("the desktop capability is overwritten");
    assert_eq!(
        closed_within(&mut output, GRANT_CHECK_ROW + WAIT).await,
        "owner_grant_stale_or_revoked"
    );
    println!("w4 row: changed grant ended the view");
    shell.end().await;
}
