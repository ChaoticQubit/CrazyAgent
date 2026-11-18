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
    let pumping = tokio::spawn(async move {
        pump(
            &mut ours,
            View::of(viewer),
            &mut asked,
            host,
            &Open::default(),
        )
        .await
    });
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
        tokio::spawn(
            async move { pump(&mut ours, View::of(viewer), &mut asked, host, &*viewing).await },
        )
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

/// A host that records each owner paste and typed request it takes, lists the agents a row
/// gives it as alive, none when it cannot list, and answers `input_live` as the row sets it;
/// every other call answers at once.
struct Recording {
    pasted: std::sync::Mutex<Vec<(AgentId, Vec<u8>)>>,
    typed: std::sync::Mutex<Vec<Vec<u8>>>,
    alive: Option<Vec<AgentId>>,
    live: std::sync::Mutex<Result<bool, String>>,
}

impl Recording {
    fn new(alive: Vec<AgentId>) -> Arc<Recording> {
        Recording::living(Some(alive), Ok(true))
    }

    fn living(alive: Option<Vec<AgentId>>, live: Result<bool, &str>) -> Arc<Recording> {
        Arc::new(Recording {
            pasted: std::sync::Mutex::new(Vec::new()),
            typed: std::sync::Mutex::new(Vec::new()),
            alive,
            live: std::sync::Mutex::new(live.map_err(str::to_owned)),
        })
    }
}

impl Host for Recording {
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
        self.typed.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }

    fn paste(&self, agent: &AgentId, keys: &[u8]) -> Result<(), HostError> {
        self.pasted
            .lock()
            .unwrap()
            .push((agent.clone(), keys.to_vec()));
        Ok(())
    }

    fn resize(&self, _: &AgentId, _: u16, _: u16) -> Result<(), HostError> {
        Ok(())
    }

    fn kill(&self, _: &AgentId) -> Result<(), HostError> {
        Ok(())
    }

    fn input_live(&self, _: &AgentId) -> Result<bool, HostError> {
        self.live.lock().unwrap().clone().map_err(HostError)
    }

    fn alive(&self) -> Result<Vec<AgentId>, HostError> {
        self.alive
            .clone()
            .ok_or_else(|| HostError("terminal_unavailable".into()))
    }
}

/// A grant that allows a set number of requests and then refuses the rest, as an owner
/// grant revoked part way does, while its periodic check still holds; it keeps the owner
/// keys each allowed request recorded.
struct Counted {
    allows_left: std::sync::atomic::AtomicUsize,
    keys: std::sync::Mutex<Vec<Vec<u8>>>,
}

impl Counted {
    fn allowing(requests: usize) -> Counted {
        Counted {
            allows_left: std::sync::atomic::AtomicUsize::new(requests),
            keys: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl Viewing for Counted {
    fn allows(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.allows_left
            .fetch_update(SeqCst, SeqCst, |left| left.checked_sub(1))
            .is_ok()
    }

    async fn holds(&self) -> bool {
        true
    }

    fn typed(&self, request: &TerminalRequest) {
        if let Some(keys) = owner_keys(request) {
            self.keys.lock().unwrap().push(keys.to_vec());
        }
    }

    async fn heard(&self, _: String) {}
}

/// Runs an input-only view's loop over requests queued up front, then closed, so the loop
/// takes them in order and ends as detached unless a request ends it first. Takes the host,
/// the grant, the agent and the requests. Returns why the loop ended.
async fn pumped(
    host: Arc<Recording>,
    viewing: &Counted,
    agent: &AgentId,
    requests: Vec<TerminalRequest>,
) -> &'static str {
    let (queue, mut asked) = tokio::sync::mpsc::channel(requests.len().max(1));
    for request in requests {
        queue.send(request).await.unwrap();
    }
    drop(queue);
    let (mut ours, _theirs) = tokio::io::duplex(1 << 16);
    let view = View {
        agent: agent.clone(),
        cols: 80,
        rows: 24,
        viewer: None,
    };
    let host: Arc<dyn Host> = host;
    tokio::time::timeout(WAIT, pump(&mut ours, view, &mut asked, host, viewing))
        .await
        .unwrap_or("no end within ten seconds")
}

/// Slice C (DEC-203 engine feed): one owner paste sent as chunks reaches the host once, after
/// its last chunk, filtered of every control character but tab and Enter, and never early;
/// each chunk counts as the owner typing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paste_chunks_reach_the_host_once_after_the_last_filtered() {
    let agent = AgentId::team(1).unwrap();
    let recording = Recording::new(vec![agent.clone()]);
    let viewing = Counted::allowing(usize::MAX);
    let second = "two\r\nthree\n\t\u{85}four\x7f".as_bytes().to_vec();
    let reason = pumped(
        recording.clone(),
        &viewing,
        &agent,
        vec![
            TerminalRequest::Paste {
                bytes: b"one\x1b[201~\x03 ".to_vec(),
                last: false,
            },
            TerminalRequest::Paste {
                bytes: second.clone(),
                last: true,
            },
        ],
    )
    .await;
    assert_eq!(reason, "terminal_detached");
    assert_eq!(
        *recording.pasted.lock().unwrap(),
        vec![(agent, b"one[201~ two\rthree\r\tfour".to_vec())]
    );
    assert!(recording.typed.lock().unwrap().is_empty());
    assert_eq!(
        *viewing.keys.lock().unwrap(),
        vec![b"one\x1b[201~\x03 ".to_vec(), second],
        "each paste chunk counts as the owner typing"
    );
}

/// Slice C: a paste chunk that is empty or over `INPUT_BYTES_MAX`, chunks past
/// `PASTE_BYTES_MAX` or a paste that is not UTF-8 are refused whole, so the view ends; a
/// paste of exactly `PASTE_BYTES_MAX` is taken.
#[test]
fn a_paste_out_of_bounds_is_refused_whole() {
    let full = [b'a'; INPUT_BYTES_MAX];
    assert_eq!(Pasting::default().add(&[], true), Err(()));
    assert_eq!(
        Pasting::default().add(&[b'a'; INPUT_BYTES_MAX + 1], true),
        Err(())
    );
    assert_eq!(
        Pasting::default().add(&[0xc3], true),
        Err(()),
        "half a UTF-8 character"
    );
    let mut over = Pasting::default();
    for _ in 0..PASTE_BYTES_MAX / INPUT_BYTES_MAX {
        assert_eq!(over.add(&full, false), Ok(None));
    }
    assert_eq!(
        over.add(b"a", true),
        Err(()),
        "one byte past PASTE_BYTES_MAX"
    );
    let mut whole = Pasting::default();
    for _ in 1..PASTE_BYTES_MAX / INPUT_BYTES_MAX {
        assert_eq!(whole.add(&full, false), Ok(None));
    }
    assert_eq!(
        whole
            .add(&full, true)
            .map(|keys| keys.map(|keys| keys.len())),
        Ok(Some(PASTE_BYTES_MAX))
    );
    assert!(!whole.open());
}

/// Slice C: inside an open paste any other request ends the view as an invalid request, and
/// nothing is pasted or typed; a grant revoked after a paste's first chunk ends the view as
/// stale and nothing is pasted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_open_paste_takes_no_other_request_and_a_revoked_grant_pastes_nothing() {
    let agent = AgentId::team(1).unwrap();
    let recording = Recording::new(vec![agent.clone()]);
    let reason = pumped(
        recording.clone(),
        &Counted::allowing(usize::MAX),
        &agent,
        vec![
            TerminalRequest::Paste {
                bytes: b"half".to_vec(),
                last: false,
            },
            TerminalRequest::Input {
                bytes: b"x".to_vec(),
            },
            TerminalRequest::Paste {
                bytes: b"rest".to_vec(),
                last: true,
            },
        ],
    )
    .await;
    assert_eq!(reason, "terminal_request_invalid");
    assert!(recording.pasted.lock().unwrap().is_empty());
    assert!(recording.typed.lock().unwrap().is_empty());

    let revoked = Recording::new(vec![agent.clone()]);
    let reason = pumped(
        revoked.clone(),
        &Counted::allowing(1),
        &agent,
        vec![
            TerminalRequest::Paste {
                bytes: b"half".to_vec(),
                last: false,
            },
            TerminalRequest::Paste {
                bytes: b"rest".to_vec(),
                last: true,
            },
        ],
    )
    .await;
    assert_eq!(reason, GRANT_ENDED);
    assert!(revoked.pasted.lock().unwrap().is_empty());
}

/// Slice C: an input-only view types its requests into the host, writes no output frame,
/// and ends as `terminal_ended` once its terminal answers gone at the grant check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_input_only_view_types_writes_nothing_and_ends_when_its_agent_is_gone() {
    use tokio::io::AsyncReadExt;
    let agent = AgentId::team(1).unwrap();
    let alive = Recording::new(vec![agent.clone()]);
    let reason = pumped(
        alive.clone(),
        &Counted::allowing(usize::MAX),
        &agent,
        vec![TerminalRequest::Input {
            bytes: b"typed".to_vec(),
        }],
    )
    .await;
    assert_eq!(reason, "terminal_detached");
    assert_eq!(*alive.typed.lock().unwrap(), vec![b"typed".to_vec()]);

    let gone: Arc<dyn Host> =
        Recording::living(Some(vec![agent.clone()]), Err("terminal_agent_unknown"));
    let (_queue, mut asked) = tokio::sync::mpsc::channel::<TerminalRequest>(1);
    let (mut ours, mut theirs) = tokio::io::duplex(1 << 16);
    let view = View {
        agent,
        cols: 80,
        rows: 24,
        viewer: None,
    };
    let viewing = Counted::allowing(usize::MAX);
    let reason =
        tokio::time::timeout(WAIT, pump(&mut ours, view, &mut asked, gone, &viewing)).await;
    assert_eq!(reason, Ok("terminal_ended"));
    drop(ours);
    let mut written = Vec::new();
    theirs.read_to_end(&mut written).await.unwrap();
    assert!(written.is_empty(), "an input-only view wrote no frame");
}

/// Slice C (DEC-203): an input-only view opens at the size it asked for and sends no output,
/// and its typed keys and its paste reach the agent's pane, as a full view of the same agent
/// then shows; a size out of bounds is refused at the handshake.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_input_only_view_types_and_pastes_without_sending_output() {
    let tmp = tempfile::Builder::new()
        .prefix("ca-term-input-")
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

    assert_eq!(
        owner
            .terminal_attach_input(agent.clone(), None, 1, 30)
            .await
            .err()
            .map(|error| error.0),
        Some("terminal_size_bounds".to_owned())
    );
    let TerminalSession {
        cols,
        rows,
        mut output,
        mut control,
        ..
    } = owner
        .terminal_attach_input(agent.clone(), None, 100, 30)
        .await
        .unwrap();
    assert_eq!((cols, rows), (100, 30));
    control.input(b"echo typed".to_vec()).await.unwrap();
    control.paste(b"-pasted".to_vec(), true).await.unwrap();
    control.input(b"\r".to_vec()).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(500), output.next())
            .await
            .is_err(),
        "an input-only view sends no output"
    );

    let TerminalSession {
        cols,
        rows,
        output: mut full,
        ..
    } = owner.terminal_attach(agent).await.unwrap();
    assert_eq!(
        (cols, rows),
        (100, 30),
        "the input-only attach sized the window"
    );
    let shown = tokio::time::timeout(WAIT, async {
        let mut seen = Vec::new();
        while String::from_utf8_lossy(&seen)
            .matches("typed-pasted")
            .count()
            < 2
        {
            match full.next().await {
                Ok(TerminalFrame::Output { bytes }) => seen.extend_from_slice(&bytes),
                other => return format!("{other:?}"),
            }
        }
        "typed and pasted".to_owned()
    })
    .await
    .unwrap_or_else(|_| "the pane never showed the typed and pasted line".into());
    assert_eq!(shown, "typed and pasted");

    drop(control);
    drop(owner);
    tokio::time::timeout(WAIT, service.shutdown())
        .await
        .unwrap()
        .unwrap();
}

/// #477 r1 F2: an input-only view's terminal is gone only by the call its keys take: a
/// teammate the engine adopted onto its lead's server answers there although the host's
/// agent list never names it, so it stays; an ended terminal, teammates' server or pane is
/// gone; any other failure counts as gone only once the host no longer lists the agent, and
/// a host that cannot list leaves the view open.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_input_only_view_is_gone_only_by_the_call_its_keys_take() {
    let agent = AgentId::team(1).unwrap();
    let listed = || Some(vec![agent.clone()]);
    for (alive, live, gone) in [
        (Some(Vec::new()), Ok(true), false),
        (Some(Vec::new()), Ok(false), false),
        (listed(), Err("terminal_agent_unknown"), true),
        (listed(), Err("terminal_teammates_server_ended"), true),
        (listed(), Err("terminal_pane_unknown"), true),
        (Some(Vec::new()), Err("terminal_command_failed"), true),
        (listed(), Err("terminal_command_failed"), false),
        (None, Err("terminal_command_failed"), false),
    ] {
        let host: Arc<dyn Host> = Recording::living(alive.clone(), live);
        assert_eq!(
            agent_gone(&host, &agent).await,
            gone,
            "listed {alive:?}, answer {live:?}"
        );
    }
}

/// #477 r1 F2: an input-only view of a terminal its host does not list, as an adopted
/// teammate is, stays open past the grant check that runs as it opens and takes its keys
/// until the owner detaches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_input_only_view_of_a_teammate_its_host_does_not_list_stays_open() {
    let agent = AgentId::team(1).unwrap();
    let host = Recording::living(Some(Vec::new()), Ok(true));
    let (queue, mut asked) = tokio::sync::mpsc::channel(4);
    let running = {
        let (host, agent): (Arc<dyn Host>, AgentId) = (host.clone(), agent.clone());
        tokio::spawn(async move {
            let (mut ours, _theirs) = tokio::io::duplex(1 << 16);
            let view = View {
                agent,
                cols: 80,
                rows: 24,
                viewer: None,
            };
            let viewing = Counted::allowing(usize::MAX);
            pump(&mut ours, view, &mut asked, host, &viewing).await
        })
    };
    queue
        .send(TerminalRequest::Input {
            bytes: b"x".to_vec(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !running.is_finished(),
        "the view stayed open past its first grant check"
    );
    assert_eq!(*host.typed.lock().unwrap(), vec![b"x".to_vec()]);
    drop(queue);
    assert_eq!(
        tokio::time::timeout(WAIT, running).await.unwrap().unwrap(),
        "terminal_detached"
    );
}
