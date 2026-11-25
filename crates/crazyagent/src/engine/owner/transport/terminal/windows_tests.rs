use super::*;
use kernel_processes::terminal::PaneCommand;
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
    folder: PathBuf,
    client: OwnerClient,
    stop: Option<oneshot::Sender<()>>,
    running: Option<tokio::task::JoinHandle<Result<(), EngineError>>>,
}

impl Shell {
    /// Starts the server, opens the engine's terminal host, starts `cmd.exe` as team 1 in
    /// it and admits a desktop client. Output: the running shell.
    async fn start() -> Shell {
        Shell::start_with(&[]).await
    }

    /// Starts a shell as `start` does, with these files written first into the folder the
    /// shell starts in, as files a project folder may hold. Takes the names and contents.
    /// Output: the running shell.
    async fn start_with(files: &[(&str, &[u8])]) -> Shell {
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
        let folder = plain.join("project");
        std::fs::create_dir(&folder).unwrap();
        for (name, bytes) in files {
            std::fs::write(folder.join(name), bytes).unwrap();
        }
        let (server, _host) = OwnerServer::bind(&owners, engine.clone(), &config)
            .await
            .unwrap();
        let (stop, stopped) = oneshot::channel();
        let running = tokio::spawn(server.run(stopped));
        let host = engine
            .terminal(&plain)
            .expect("the engine opens its ConPTY host on Windows");
        let agent = AgentId::team(1).unwrap();
        let command = PaneCommand {
            argv: vec![SHELL.into(), "/D".into(), "/Q".into()],
            cwd: folder.clone(),
            env: Vec::new(),
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
            folder,
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
    shown(output, text).await.is_some()
}

/// Reads output frames until they hold `text` or the view closes. Takes the output and the
/// text. Output: all the output read, once `text` was in it within `WAIT`, or none.
async fn shown(output: &mut TerminalOutput, text: &str) -> Option<String> {
    let mut all = Vec::new();
    tokio::time::timeout(WAIT, async {
        loop {
            match output.next().await {
                Ok(TerminalFrame::Output { bytes }) => {
                    all.extend_from_slice(&bytes);
                    let read = String::from_utf8_lossy(&all);
                    if read.contains(text) {
                        return Some(read.into_owned());
                    }
                }
                _ => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// Asks the host whether the shell's app turned bracketed paste on, until the answer is
/// `want` or `WAIT` ends. Takes the shell and the wanted answer. Output: the last answer.
async fn bracketed(shell: &Shell, want: bool) -> Result<bool, String> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let (host, agent) = (shell.host.clone(), shell.agent.clone());
        let answer = tokio::task::spawn_blocking(move || host.input_live(&agent))
            .await
            .unwrap()
            .map_err(|error| error.0);
        if answer == Ok(want) || tokio::time::Instant::now() >= deadline {
            return answer;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
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

/// T-R3: an input-only view of an owner shell that exited ends as `terminal_ended` at its
/// next grant check, with no key typed, since the host tells the pane is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_input_only_view_of_an_exited_shell_ends_as_terminal_ended() {
    let shell = Shell::start().await;
    let TerminalSession {
        mut output,
        mut control,
        ..
    } = shell
        .client
        .terminal_attach_input(shell.agent.clone(), None, (80, 24), false)
        .await
        .map_err(|error| error.0)
        .unwrap();
    control.input(b"exit\r".to_vec()).await.unwrap();
    assert_eq!(
        closed_within(&mut output, GRANT_CHECK_ROW + WAIT).await,
        "terminal_ended"
    );
    println!("w4 row: an input-only view of an exited shell ended");
    shell.end().await;
}

/// T-R4 (C4): a `git.bat` planted in the folder the owner shell starts in does not run when
/// the owner types `git`, since the shell's environment carries
/// `NoDefaultCurrentDirectoryInExePath=1`; the shell still runs what the owner types next.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_planted_git_bat_in_the_project_folder_does_not_run() {
    let shell =
        Shell::start_with(&[("git.bat", &b"@echo w4-planted-%COMSPEC:~0,1%\r\n"[..])]).await;
    assert!(shell.folder.join("git.bat").is_file());
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
    control.input(b"git\r".to_vec()).await.unwrap();
    control
        .input(b"echo w4-after-%COMSPEC:~0,1%\r".to_vec())
        .await
        .unwrap();
    let read = shown(&mut output, "w4-after-C")
        .await
        .expect("the shell ran the next command");
    assert!(
        !read.contains("w4-planted-C"),
        "the planted git.bat ran: {read:?}"
    );
    println!("w4 row: the planted git.bat did not run");
    drop(control);
    shell.end().await;
}

/// T-R5: an owner paste into a shell that has not turned bracketed paste on reaches it as
/// typed keys, with no bracket, so the shell runs the pasted line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paste_without_bracketed_paste_reaches_the_shell_as_typed_keys() {
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
    assert_eq!(bracketed(&shell, false).await, Ok(false));
    control
        .paste(b"echo w4-pasted-%COMSPEC:~0,1%\r\n".to_vec(), true)
        .await
        .unwrap();
    let read = shown(&mut output, "w4-pasted-C")
        .await
        .expect("the pasted line ran");
    assert!(
        !read.contains("200~"),
        "no bracket reached the shell: {read:?}"
    );
    println!("w4 row: an unbracketed paste ran as typed keys");
    drop(control);
    shell.end().await;
}

/// T-R5: the host reads bracketed paste as off until the shell's app turns it on with
/// DECSET 2004 through ConPTY, and as on after, which decides whether a paste is bracketed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_host_reads_bracketed_paste_on_only_after_the_app_turns_it_on() {
    let shell = Shell::start_with(&[("on.bat", &b"@echo \x1b[?2004h\r\n"[..])]).await;
    let TerminalSession { mut control, .. } = shell
        .client
        .terminal_attach(shell.agent.clone())
        .await
        .map_err(|error| error.0)
        .unwrap();
    assert_eq!(bracketed(&shell, false).await, Ok(false), "off at start");
    control.input(b".\\on.bat\r".to_vec()).await.unwrap();
    let on = bracketed(&shell, true).await;
    println!("w4 row: bracketed paste after DECSET 2004: {on:?}");
    assert_eq!(on, Ok(true), "on after the app's DECSET 2004");
    drop(control);
    shell.end().await;
}
