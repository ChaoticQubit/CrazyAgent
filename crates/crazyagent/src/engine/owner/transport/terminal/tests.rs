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

/// v242 MRR2-2 (rv-code2 r2): a pane attach on a native lead whose own server is gone and
/// whose teammates' server ended, the state the 2255 lead's servers end in, is refused at the
/// handshake with `terminal_teammates_server_ended`, the code the desktop and the TUI read as
/// the terminal's end. Here an earlier engine started the lead, so this engine holds no
/// keeper and the refused socket that server left counts as its end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pane_attach_on_a_lead_whose_servers_ended_is_refused_as_its_teammates_servers_end() {
    use std::os::unix::fs::DirBuilderExt;
    let tmp = tempfile::Builder::new()
        .prefix("ca-term-mates-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let root = tmp.path();
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = root.join("g.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 2000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
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
    let lead = AgentId::team(1).unwrap();
    let private = |folder: &std::path::Path| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(folder)
            .unwrap();
    };
    let record = crate::team::runtime::agent_teammates_socket_name(&root.join("team"), 1);
    private(record.parent().unwrap());
    let socket = crate::terminal::renew_teammates_socket(root, &lead).unwrap();
    private(socket.parent().unwrap());
    drop(crate::spawn_gate::bind_unix_std(&socket).unwrap());

    let refused = owner
        .terminal_attach_pane(lead, "%0".into())
        .await
        .map(drop)
        .map_err(|problem| problem.0);
    assert_eq!(
        refused,
        Err(crate::terminal::TEAMMATES_SERVER_ENDED.to_owned()),
        "a pane attach on a lead whose servers ended was not refused as its teammates' server's end"
    );

    drop(owner);
    tokio::time::timeout(WAIT, service.shutdown())
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(socket.parent().unwrap()).unwrap();
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

/// OB-10: what a terminal view sends by itself is not the owner typing, so an answer
/// waiting for an idle live master is never held back by it. Wheel keys a full-screen app
/// reads, focus in and out as the owner switches tabs, and mouse reports set no owner key;
/// an arrow key the owner presses still does.
#[test]
fn wheel_keys_and_a_views_own_reports_never_count_as_owner_keys() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("engine.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    crate::team::initialize(&connection).unwrap();
    let typing = OwnerTyping::new(database.clone());
    let input = |bytes: &[u8]| TerminalRequest::Input {
        bytes: bytes.to_vec(),
    };
    let requests = [
        (
            11,
            TerminalRequest::Scroll {
                bytes: b"\x1b[A\x1b[A\x1bOB".to_vec(),
            },
        ),
        (12, input(b"\x1b[I")),
        (13, input(b"\x1b[O\x1b[I\x1b[O")),
        (14, input(b"\x1b[<65;10;5M\x1b[<0;3;4m")),
        (15, input(b"\x1b[M`!!")),
        (16, input(b"\x1b[A")),
        (17, input(b"\x1b[Ix")),
        (
            18,
            TerminalRequest::Scroll {
                bytes: b"\x1b[Ayes\r".to_vec(),
            },
        ),
    ];
    let at = crate::team::now_ms();
    for (agent, request) in &requests {
        if let Some(keys) = owner_keys(request) {
            typing.record(*agent, keys, at);
        }
    }
    let keyed = |agent: i64| owner_key_ms(&connection, Some(&typing), agent).unwrap();
    assert_eq!(keyed(11), None, "a wheel turn is not the owner typing");
    assert_eq!(
        (keyed(12), keyed(13)),
        (None, None),
        "focus reports a view sends as the owner switches tabs are not keys"
    );
    assert_eq!(
        (keyed(14), keyed(15)),
        (None, None),
        "mouse reports are not keys"
    );
    assert_eq!(
        keyed(16),
        Some(at),
        "an arrow key the owner presses still holds engine input"
    );
    assert_eq!(keyed(17), Some(at), "a key typed beside a report counts");
    assert_eq!(
        keyed(18),
        Some(at),
        "a wheel request carrying other keys counts as the owner typing (OB10-L1)"
    );
}

/// How long a held send waits for its row before it answers anyway, so a failed row never
/// leaves a blocking thread behind.
const HELD: Duration = Duration::from_secs(5);

/// A host whose every send of typed bytes reports them and then waits until the row lets it
/// go, at most `HELD`, as a tmux server slow to answer `send-keys` does; every other call
/// answers at once.
struct Holding {
    started: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl Host for Holding {
    fn spawn(
        &self,
        _: &AgentId,
        _: &InteractiveCommand,
    ) -> Result<crate::terminal::PaneRef, HostError> {
        Err(HostError("terminal_unavailable".into()))
    }

    fn attach(&self, _: &AgentId) -> Result<TerminalStream, HostError> {
        Err(HostError("terminal_agent_unknown".into()))
    }

    fn input(&self, _: &AgentId, bytes: &[u8]) -> Result<(), HostError> {
        let _ = self.started.send(bytes.to_vec());
        let _ = self.release.lock().unwrap().recv_timeout(HELD);
        Ok(())
    }

    fn resize(&self, _: &AgentId, _: u16, _: u16) -> Result<(), HostError> {
        Ok(())
    }

    fn kill(&self, _: &AgentId) -> Result<(), HostError> {
        Ok(())
    }

    fn alive(&self) -> Result<Vec<AgentId>, HostError> {
        Ok(Vec::new())
    }
}

/// A grant that always stands and keeps the prompts recorded as heard, so a row drives a
/// view's loop alone and reads what it recorded.
#[derive(Default)]
struct Open {
    heard: std::sync::Mutex<Vec<String>>,
}

impl Viewing for Open {
    fn allows(&self) -> bool {
        true
    }

    async fn holds(&self) -> bool {
        true
    }

    fn typed(&self, _: &TerminalRequest) {}

    fn heard(&self, text: String) -> impl Future<Output = ()> + Send {
        self.heard.lock().unwrap().push(text);
        async {}
    }
}

/// v242 typing B (BRIEF-typing-path): a send waiting on the host never stops the view's
/// output: what the agent prints while a typed key's send is held reaches the viewer before
/// that send answers, and the next key waits for that answer, so keys stay in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_reaches_the_view_while_a_typed_keys_send_is_held() {
    let (started, mut sends) = tokio::sync::mpsc::unbounded_channel();
    let (release, released) = std::sync::mpsc::channel();
    let host: Arc<dyn Host> = Arc::new(Holding {
        started,
        release: std::sync::Mutex::new(released),
    });
    let (feed, chunks) = crate::terminal::buffer::buffer();
    let viewer = TerminalStream {
        agent: AgentId::team(1).unwrap(),
        cols: 80,
        rows: 24,
        history: Vec::new(),
        screen: Vec::new(),
        chunks,
        at: None,
    };
    let (requests, mut asked) = tokio::sync::mpsc::channel(REQUESTS_QUEUED);
    let (mut ours, mut theirs) = tokio::io::duplex(1 << 16);
    let pumping =
        tokio::spawn(
            async move { pump(&mut ours, viewer, &mut asked, host, &Open::default()).await },
        );
    for key in [b"a", b"b"] {
        let bytes = key.to_vec();
        requests
            .send(TerminalRequest::Input { bytes })
            .await
            .unwrap();
    }
    assert_eq!(
        tokio::time::timeout(WAIT, sends.recv()).await,
        Ok(Some(b"a".to_vec())),
        "the first key's send started"
    );

    assert!(feed.push(b"printed-while-held"));
    let frame = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut theirs))
        .await
        .ok()
        .and_then(Result::ok)
        .and_then(|frame| serde_json::from_slice::<TerminalFrame>(&frame[4..]).ok());
    assert_eq!(
        frame,
        Some(TerminalFrame::Output {
            bytes: b"printed-while-held".to_vec()
        }),
        "output reached the view while a typed key's send was held"
    );
    assert!(
        sends.try_recv().is_err(),
        "the next key waits for the held send's answer"
    );

    release.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(WAIT, sends.recv()).await,
        Ok(Some(b"b".to_vec())),
        "keys are sent in the order typed"
    );
    release.send(()).unwrap();
    drop(requests);
    assert_eq!(
        tokio::time::timeout(WAIT, pumping).await.map(Result::ok),
        Ok(Some("terminal_detached")),
        "the view ends as detached once its requests end"
    );
    drop(feed);
}

/// v242 typing B fold: a prompt whose send is still held when its view ends is recorded as
/// heard once the agent took it, as it was while the loop awaited each send.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_prompt_held_as_its_view_ends_is_still_recorded_as_heard() {
    let (started, mut sends) = tokio::sync::mpsc::unbounded_channel();
    let (release, released) = std::sync::mpsc::channel();
    let host: Arc<dyn Host> = Arc::new(Holding {
        started,
        release: std::sync::Mutex::new(released),
    });
    let (feed, chunks) = crate::terminal::buffer::buffer();
    let viewer = TerminalStream {
        agent: AgentId::team(1).unwrap(),
        cols: 80,
        rows: 24,
        history: Vec::new(),
        screen: Vec::new(),
        chunks,
        at: None,
    };
    let (requests, mut asked) = tokio::sync::mpsc::channel(REQUESTS_QUEUED);
    let (mut ours, _theirs) = tokio::io::duplex(1 << 16);
    let viewing = Arc::new(Open::default());
    let mut pumping = {
        let viewing = viewing.clone();
        tokio::spawn(async move { pump(&mut ours, viewer, &mut asked, host, &*viewing).await })
    };
    let text = "go".to_owned();
    requests
        .send(TerminalRequest::Prompt { text: text.clone() })
        .await
        .unwrap();
    assert!(
        matches!(tokio::time::timeout(WAIT, sends.recv()).await, Ok(Some(_))),
        "the prompt's send started"
    );

    drop(feed);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut pumping)
            .await
            .is_err(),
        "a view whose terminal ended waits for its running send"
    );
    release.send(()).unwrap();
    release.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(WAIT, pumping).await.map(Result::ok),
        Ok(Some("terminal_ended")),
        "the view ends with its terminal"
    );
    assert_eq!(
        *viewing.heard.lock().unwrap(),
        vec![text],
        "the prompt the agent took is recorded as heard"
    );
    drop(requests);
}

/// v242 typing timing row (BRIEF-typing-path): 50 keys typed as fast as a view sends them
/// into a stand-in agent whose terminal echoes each key all echo back within a loose
/// budget; prints each key's echo time at p50 and p95 and the last echo's time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fifty_typed_keys_echo_back_within_budget() {
    const KEYS: usize = 50;
    const BUDGET: Duration = Duration::from_secs(10);
    let tmp = tempfile::Builder::new()
        .prefix("ca-term-typing-")
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
        argv: vec!["/bin/sh".into(), "-c".into(), "exec /bin/cat".into()],
        cwd: root.to_path_buf(),
        env: Vec::new(),
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
        mut output,
        mut control,
        ..
    } = owner.terminal_attach(agent).await.unwrap();

    let typing = tokio::spawn(async move {
        let mut sent = Vec::with_capacity(KEYS);
        for _ in 0..KEYS {
            sent.push(std::time::Instant::now());
            let bytes = b"%".to_vec();
            control
                .send(&TerminalRequest::Input { bytes })
                .await
                .unwrap();
        }
        (control, sent)
    });
    let started = std::time::Instant::now();
    let mut seen = Vec::with_capacity(KEYS);
    while seen.len() < KEYS {
        let left = BUDGET.saturating_sub(started.elapsed());
        let frame = tokio::time::timeout(left, output.next()).await;
        let bytes = match frame {
            Ok(Ok(TerminalFrame::Output { bytes })) => bytes,
            other => panic!(
                "all {KEYS} keys echoed within {BUDGET:?}, {} did: {other:?}",
                seen.len()
            ),
        };
        let at = std::time::Instant::now();
        seen.extend(bytes.iter().filter(|byte| **byte == b'%').map(|_| at));
    }
    let (control, sent) = typing.await.unwrap();
    assert_eq!(seen.len(), KEYS, "each key echoed once");
    assert_eq!(sent.len(), KEYS);
    let mut waits: Vec<Duration> = seen
        .iter()
        .zip(&sent)
        .map(|(seen, sent)| seen.saturating_duration_since(*sent))
        .collect();
    waits.sort();
    let last = seen[KEYS - 1].saturating_duration_since(sent[0]);
    eprintln!(
        "typing timing: {KEYS} keys, echo p50 {:?}, p95 {:?}, last echo {last:?} after the first key",
        waits[KEYS / 2],
        waits[KEYS * 95 / 100]
    );
    assert!(
        last <= BUDGET,
        "the last echo came within {BUDGET:?}: {last:?}"
    );

    drop(control);
    drop(owner);
    tokio::time::timeout(WAIT, service.shutdown())
        .await
        .unwrap()
        .unwrap();
}
