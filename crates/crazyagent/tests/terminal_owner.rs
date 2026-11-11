#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    OwnerClientKind, OwnerResponse, OwnerResult, TerminalOutput, TerminalSession,
};
use crazyagent::harnesses::interactive::{InteractiveCommand, Transcript};
use crazyagent::project_setup::{Platform, ProjectSetup, ResearchKind};
use crazyagent::protocol::{ProjectId, ProjectSpec};
use crazyagent::team::{AgentStatus, NodeKind};
use crazyagent::terminal::tmux::{SOCKET, Tmux, installed};
use crazyagent::terminal::{AgentId, FOLDER, Host, TerminalFrame};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

struct Server {
    tmux: PathBuf,
    socket: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = crazyagent::spawn_gate::status(
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
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
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

/// Counts the tmux processes whose command line names `socket`: the server on it and any
/// client still connected. Input: the server's socket path. Output: the count.
fn servers(socket: &Path) -> usize {
    let listed = crazyagent::spawn_gate::output(
        Command::new("/bin/ps")
            .args(["-A", "-o", "args="])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert!(listed.status.success(), "{listed:?}");
    let socket = socket.to_str().unwrap();
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter(|line| line.contains("tmux") && line.contains(socket))
        .count()
}

/// Waits up to ten seconds for no tmux process to name `socket`. Input: the socket path.
/// Output: whether none is left.
async fn gone(socket: &Path) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while servers(socket) > 0 {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    true
}

/// A team sub-agent's terminal on an in-process engine in a temporary folder. Fields drop
/// in declaration order, so the server guard ends the tmux server while its socket still
/// exists and only then is the folder holding that socket deleted.
struct SubAgent {
    _server: Server,
    root: tempfile::TempDir,
    service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
    controller: OwnerController,
    launcher: Tmux,
    agent: AgentId,
}

impl SubAgent {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-term-sub-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("g.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 2000\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let tmux = installed(root.path()).expect("tmux must be installed on this machine");
        let folder = root.path().join(FOLDER);
        let server = Server {
            tmux: tmux.clone(),
            socket: folder.join(SOCKET),
        };
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("p.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let member: i64 = {
            let connection = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
            connection.busy_timeout(Duration::from_secs(5)).unwrap();
            connection
                .execute(
                    "INSERT INTO team_agents(project,role,harness,tools,status,task,created_ms,heartbeat_ms) VALUES ('alpha','sub','codex',1,'stopped',4,1,1)",
                    [],
                )
                .unwrap();
            connection.last_insert_rowid()
        };
        let agent = AgentId::team(member).unwrap();
        let command = InteractiveCommand {
            argv: vec!["/bin/sh".into()],
            cwd: root.path().to_path_buf(),
            env: vec![("PS1".into(), "ready$ ".into())],
            transcript: Transcript::Unavailable,
        };
        let launcher = {
            let agent = agent.clone();
            tokio::task::spawn_blocking(move || {
                let launcher = Tmux::open(Some(tmux), &folder);
                launcher.spawn(&agent, &command).unwrap();
                launcher
            })
            .await
            .unwrap()
        };
        assert!(
            servers(&server.socket) > 0,
            "the sub-agent runs on the engine's tmux server"
        );
        let controller = OwnerController::attach(
            root.path(),
            OwnerClientKind::Desktop,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        Self {
            _server: server,
            root,
            service,
            _stop: stop,
            controller,
            launcher,
            agent,
        }
    }

    fn database_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for entry in std::fs::read_dir(self.root.path()).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().starts_with("engine.db") {
                bytes.extend(std::fs::read(entry.path()).unwrap());
            }
        }
        assert!(!bytes.is_empty(), "the engine database exists");
        bytes
    }

    async fn stop(self) {
        let socket = self._server.socket.clone();
        let Self {
            mut service,
            controller,
            launcher,
            agent,
            ..
        } = self;
        tokio::task::spawn_blocking(move || launcher.kill(&agent))
            .await
            .unwrap()
            .unwrap();
        drop(controller);
        let terminal = service
            .owner_runtime()
            .and_then(|runtime| runtime.terminal().ok())
            .expect("the engine runs a terminal host");
        tokio::time::timeout(Duration::from_secs(10), service.shutdown())
            .await
            .unwrap()
            .unwrap();
        tokio::task::spawn_blocking(move || crazyagent::terminal::quit(terminal.as_ref(), true))
            .await
            .unwrap()
            .unwrap();
        assert!(
            gone(&socket).await,
            "a stopped engine with no team agent left ends its tmux server while its folder remains"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_test_engine_dropped_without_a_stop_leaves_no_tmux_server() {
    let sub = SubAgent::start().await;
    let socket = sub._server.socket.clone();
    assert!(servers(&socket) > 0);
    drop(sub);
    assert!(
        gone(&socket).await,
        "an engine that never stops cleanly leaves no tmux server once its test ends"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn keys_typed_into_a_sub_agent_never_reach_the_engine_database() {
    let mut sub = SubAgent::start().await;
    let TerminalSession {
        mut output,
        mut control,
        ..
    } = sub
        .controller
        .terminal_attach(sub.agent.clone())
        .await
        .unwrap();
    let mut seen = Vec::new();
    assert!(read_until(&mut output, &mut seen, b"ready$ ").await);
    control
        .input(b"stty -echo; read secret; stty echo\r".to_vec())
        .await
        .unwrap();
    control.input(b"hunter2-hidden\r".to_vec()).await.unwrap();
    control
        .input(b"echo typed-line-shown\r".to_vec())
        .await
        .unwrap();
    assert!(read_until(&mut output, &mut seen, b"\r\ntyped-line-shown\r\n").await);
    control.input(b"echo after\r".to_vec()).await.unwrap();
    assert!(read_until(&mut output, &mut seen, b"\r\nafter\r\n").await);
    let stored = sub.database_bytes();
    assert!(
        !contains(&stored, b"hunter2-hidden"),
        "a secret typed at a hidden prompt is not in the engine database"
    );
    assert!(
        !contains(&stored, b"echo typed-line-shown"),
        "a typed command line is not in the engine database"
    );
    control.detach().await.unwrap();
    assert_eq!(closed(&mut output).await, "terminal_detached");
    sub.stop().await;
}

/// finding_855af84870d65746: a revoked viewer's next key, prompt or resize never reaches the
/// terminal. A view checks its grant at once and then every 5 s, so the viewer first types
/// one key and idles, and the revoke lands between those checks, where only the per-key
/// check stands between the revoked viewer and the terminal.
#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_viewer_cannot_type_one_more_key_into_the_terminal() {
    use crazyagent::engine::owner::{OwnerControl, OwnerDecision};
    use crazyagent::protocol::RequestId;
    let mut sub = SubAgent::start().await;
    let TerminalSession {
        mut output,
        mut control,
        ..
    } = sub
        .controller
        .terminal_attach(sub.agent.clone())
        .await
        .unwrap();
    let mut seen = Vec::new();
    assert!(read_until(&mut output, &mut seen, b"ready$ ").await);
    control
        .input(b"echo before-revoke\r".to_vec())
        .await
        .unwrap();
    assert!(read_until(&mut output, &mut seen, b"\r\nbefore-revoke\r\n").await);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let host = sub.service.owner_runtime().unwrap();
    host.control(OwnerControl {
        operation_id: RequestId::new("revoke-terminal-viewer").unwrap(),
        target: OwnerClientKind::Desktop,
        expected_epoch: host.epoch(OwnerClientKind::Desktop).unwrap(),
        decision: OwnerDecision::Revoke,
    })
    .await
    .unwrap();
    let _ = control.input(b"echo leaked-after-revoke\r".to_vec()).await;
    let _ = control.resize(100, 30).await;
    assert_eq!(closed(&mut output).await, "owner_grant_stale_or_revoked");

    let mut tui = OwnerController::attach(
        sub.root.path(),
        OwnerClientKind::Tui,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    let TerminalSession {
        cols,
        rows,
        mut output,
        mut control,
        ..
    } = tui.terminal_attach(sub.agent.clone()).await.unwrap();
    assert_eq!(
        (cols, rows),
        (80, 24),
        "the revoked viewer's resize never reached the terminal"
    );
    let mut screen = Vec::new();
    control
        .input(b"echo still-served\r".to_vec())
        .await
        .unwrap();
    assert!(
        read_until(&mut output, &mut screen, b"\r\nstill-served\r\n").await,
        "{}",
        String::from_utf8_lossy(&screen)
    );
    assert!(
        !contains(&screen, b"leaked-after-revoke"),
        "a key the revoked viewer typed never reached the terminal: {}",
        String::from_utf8_lossy(&screen)
    );
    control.detach().await.unwrap();
    assert_eq!(closed(&mut output).await, "terminal_detached");
    drop(tui);
    sub.stop().await;
}

/// A full scrollback replayed to a viewer that starts reading only after the attach deadline
/// arrives whole and in order, then live output follows and the view ends with its detach
/// reason, instead of a rejection written into a session that already opened.
#[tokio::test(flavor = "multi_thread")]
async fn a_long_scrollback_replay_outlives_the_attach_deadline_and_stays_whole() {
    let mut sub = SubAgent::start().await;
    let TerminalSession {
        mut output,
        mut control,
        ..
    } = sub
        .controller
        .terminal_attach(sub.agent.clone())
        .await
        .unwrap();
    let mut seen = Vec::new();
    assert!(read_until(&mut output, &mut seen, b"ready$ ").await);
    control
        .input(
            b"seq -f 'row-%04g-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx' 1 4000; echo filled-up\r"
                .to_vec(),
        )
        .await
        .unwrap();
    assert!(read_until(&mut output, &mut seen, b"\r\nfilled-up\r\n").await);
    control.detach().await.unwrap();
    assert_eq!(closed(&mut output).await, "terminal_detached");

    let TerminalSession {
        mut output,
        mut control,
        ..
    } = sub
        .controller
        .terminal_attach(sub.agent.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    let mut replayed = Vec::new();
    for wanted in [&b"row-0001-"[..], b"row-3950-", b"\r\nfilled-up\r\n"] {
        assert!(
            read_until(&mut output, &mut replayed, wanted).await,
            "the replay after the attach deadline holds {:?}",
            String::from_utf8_lossy(wanted)
        );
    }
    control
        .input(b"echo live-after-replay\r".to_vec())
        .await
        .unwrap();
    assert!(
        read_until(&mut output, &mut replayed, b"\r\nlive-after-replay\r\n").await,
        "live output follows the whole replay"
    );
    control.detach().await.unwrap();
    assert_eq!(closed(&mut output).await, "terminal_detached");
    sub.stop().await;
}

fn prompts(database: &std::path::Path) -> Vec<String> {
    let connection = rusqlite::Connection::open(database).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    let mut statement = connection
        .prepare("SELECT text FROM team_owner_prompts ORDER BY id")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sent_prompt_is_one_submission_and_the_only_text_kept_for_the_master() {
    let mut sub = SubAgent::start().await;
    let database = sub.root.path().join("engine.db");
    let TerminalSession {
        mut output,
        mut control,
        ..
    } = sub
        .controller
        .terminal_attach(sub.agent.clone())
        .await
        .unwrap();
    let mut seen = Vec::new();
    assert!(read_until(&mut output, &mut seen, b"ready$ ").await);
    control.input(b"echo typed-first\r".to_vec()).await.unwrap();
    control
        .prompt("echo sent-prompt\nsecond line".into())
        .await
        .unwrap();
    assert!(
        read_until(&mut output, &mut seen, b"sent-prompt second line\r\n").await,
        "a multi-line prompt to an app without bracketed paste is one submission: {}",
        String::from_utf8_lossy(&seen)
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let kept = loop {
        let kept = prompts(&database);
        if !kept.is_empty() || std::time::Instant::now() > deadline {
            break kept;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(kept, vec!["echo sent-prompt\nsecond line".to_owned()]);

    control
        .input(b"printf '\\033[?2004h'; cat -v\r".to_vec())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    control.prompt("pasted".into()).await.unwrap();
    assert!(
        read_until(&mut output, &mut seen, b"^[[200~pasted^[[201~").await,
        "a prompt to an app with bracketed paste on arrives as one paste: {}",
        String::from_utf8_lossy(&seen)
    );
    assert_eq!(
        control
            .prompt(" \n ".into())
            .await
            .err()
            .map(|error| error.0),
        Some("terminal_prompt_bounds".into())
    );
    control.input(vec![3]).await.unwrap();
    control.detach().await.unwrap();
    assert_eq!(closed(&mut output).await, "terminal_detached");
    sub.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn owner_keys_keep_echoing_while_another_writer_holds_the_engine_database() {
    let mut sub = SubAgent::start().await;
    let database = sub.root.path().join("engine.db");
    let member = sub.agent.team_agent().unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    crazyagent::team::initialize(&connection).unwrap();
    connection
        .execute(
            "INSERT INTO team_live_masters(project,live,agent) VALUES ('alpha',1,?1)",
            [member],
        )
        .unwrap();
    let agents = sub
        .service
        .owner_runtime()
        .unwrap()
        .agents()
        .expect("the engine opens its agent register as a running team does");
    let TerminalSession {
        mut output,
        mut control,
        ..
    } = sub
        .controller
        .terminal_attach(sub.agent.clone())
        .await
        .unwrap();
    let mut seen = Vec::new();
    assert!(read_until(&mut output, &mut seen, b"ready$ ").await);

    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    for word in ["first", "second", "third"] {
        control
            .input(format!("echo {word}-key\r").into_bytes())
            .await
            .unwrap();
        let wanted = format!("\r\n{word}-key\r\n");
        assert!(read_until(&mut output, &mut seen, wanted.as_bytes()).await);
    }
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "keys echo at once while another writer holds the engine database: {:?}",
        started.elapsed()
    );
    connection.execute_batch("ROLLBACK").unwrap();

    let host = sub.service.owner_runtime().unwrap().terminal().unwrap();
    let project = crazyagent::protocol::ProjectId::new("alpha").unwrap();
    assert_eq!(
        crazyagent::team::master::master_input(
            &connection,
            host.as_ref(),
            Some(agents.typing()),
            &project,
            "engine news",
        ),
        Err(crazyagent::team::master::InputRefused::OwnerTyping),
        "engine input waits while the owner typed within ten seconds"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let stored = loop {
        let stored: Option<i64> = connection
            .query_row(
                "SELECT typed_ms FROM team_live_masters WHERE project='alpha'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if stored.is_some() || std::time::Instant::now() > deadline {
            break stored;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        stored.is_some(),
        "the signal reaches the engine database once the lock is free"
    );

    drop(agents);
    control.detach().await.unwrap();
    assert_eq!(closed(&mut output).await, "terminal_detached");
    sub.stop().await;
}

/// Checks that the engine recorded one owner change. Input: its reply. Output: none.
fn applied(response: OwnerResponse) {
    match response {
        OwnerResponse::Applied { operation } => {
            assert_eq!(operation.result, OwnerResult::Applied)
        }
        other => panic!("the owner change must be recorded: {other:?}"),
    }
}

/// Waits up to ten seconds for an agent terminal's screen to show some text. Inputs: the
/// host, the agent and the text. Output: whether it showed.
fn shows(host: &dyn Host, agent: &AgentId, wanted: &[u8]) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !host
        .screen(agent)
        .is_ok_and(|screen| contains(&screen, wanted))
    {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// A finished sub-agent whose last screen the engine kept names its terminal in the team
/// graph, and its card opens that screen, labeled with the time it finished, and closes as
/// ended, so no key reaches the ended agent; one that never ran here names none (N-D21).
#[tokio::test(flavor = "multi_thread")]
async fn a_finished_sub_agent_opens_its_kept_last_screen_read_only() {
    let mut sub = SubAgent::start().await;
    let project = ProjectId::new("alpha").unwrap();
    let disk = tempfile::Builder::new()
        .prefix("ca-term-home-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(disk.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = disk.path().join("Projects");
    std::fs::create_dir(&home).unwrap();
    applied(
        sub.controller
            .set_projects_home(home.to_str().unwrap().into(), 1)
            .await
            .unwrap(),
    );
    applied(
        sub.controller
            .setup_project(
                project.clone(),
                ProjectSpec {
                    name: "Alpha".into(),
                    repositories: vec![],
                },
                ProjectSetup {
                    about: "Watches sensor buoys and raises alerts.".into(),
                    tech_stack: vec!["rust".into()],
                    platforms: vec![Platform::Website],
                    research: vec![ResearchKind::MarketResearch],
                    pool: None,
                    team: None,
                    documents: vec![],
                },
                2,
            )
            .await
            .unwrap(),
    );
    let lead = sub.agent.team_agent().unwrap();
    let (finished, never) = {
        let connection = rusqlite::Connection::open(sub.root.path().join("engine.db")).unwrap();
        connection.busy_timeout(Duration::from_secs(5)).unwrap();
        connection
            .execute(
                "INSERT OR REPLACE INTO team_finished_view(project,show) VALUES ('alpha',1)",
                [],
            )
            .unwrap();
        let worker = || {
            connection
                .execute(
                    "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,parent) VALUES ('alpha','sub','codex',1,'stopped',1,1,?1)",
                    [lead],
                )
                .unwrap();
            AgentId::team(connection.last_insert_rowid()).unwrap()
        };
        (worker(), worker())
    };
    let host = sub.service.owner_runtime().unwrap().terminal().unwrap();
    let ran = {
        let (host, finished, cwd) = (
            host.clone(),
            finished.clone(),
            sub.root.path().to_path_buf(),
        );
        tokio::task::spawn_blocking(move || {
            let command = InteractiveCommand {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf 'kept-screen-%s\\n' $((6*7)); exec /bin/sleep 30".into(),
                ],
                cwd,
                env: Vec::new(),
                transcript: Transcript::Unavailable,
            };
            host.spawn(&finished, &command).unwrap();
            let shown = shows(host.as_ref(), &finished, b"kept-screen-42");
            host.kill(&finished).unwrap();
            shown
        })
        .await
        .unwrap()
    };
    assert!(
        ran,
        "the sub-agent's run drew its last screen before it ended"
    );

    let graph = sub.controller.team_graph(project).await.unwrap();
    let node = |agent: &AgentId| {
        graph
            .nodes
            .iter()
            .find(|node| node.id == agent.to_string())
            .unwrap_or_else(|| panic!("{agent} is drawn: {graph:?}"))
    };
    assert_eq!(
        (node(&finished).kind, node(&finished).status),
        (NodeKind::Sub, Some(AgentStatus::Stopped))
    );
    assert_eq!(
        node(&finished).terminal,
        Some(finished.clone()),
        "the card of a finished sub-agent whose last screen is kept opens it"
    );
    assert_eq!(
        node(&never).terminal,
        None,
        "a finished sub-agent that never ran under this engine has no screen to open"
    );

    let TerminalSession { mut output, .. } = sub
        .controller
        .terminal_attach(finished.clone())
        .await
        .unwrap();
    let mut seen = Vec::new();
    assert!(
        read_until(&mut output, &mut seen, b"kept-screen-42").await
            && read_until(&mut output, &mut seen, b" Finished at ").await,
        "the card shows the kept screen under its finished label: {}",
        String::from_utf8_lossy(&seen)
    );
    assert_eq!(closed(&mut output).await, "terminal_ended");
    let typed = tokio::task::spawn_blocking(move || host.input(&finished, b"x").err())
        .await
        .unwrap()
        .map(|error| error.0);
    assert_eq!(
        typed.as_deref(),
        Some("terminal_agent_unknown"),
        "no key reaches the ended agent"
    );
    sub.stop().await;
}
