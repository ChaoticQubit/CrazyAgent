use super::changes::change_sets;
use super::changes::tests::{Case as ChangeCase, NOW};
use super::*;
use crate::engine::Engine;
use crate::engine::owner::OwnerServer;
use crate::landing::{AcceptOutcome, ChangeState};
use crate::protocol::FRAME_BYTES_MAX;
use crate::sandbox::policy::AgentAccess;
use serde_json::json;
use std::ffi::OsStr;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const HELD: Duration = Duration::from_secs(2);

/// A real engine with its owner server running under a private temporary root.
struct Case {
    root: tempfile::TempDir,
    engine: Arc<Engine>,
    runtime: OwnerRuntimeHandle,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<Result<(), EngineError>>>,
}

impl Case {
    /// Opens the engine and binds its owner server in an `o00000000` generation folder,
    /// so the temporary root plays the engine root that holds `ide/`. Projects `alpha` and
    /// `beta` are set up with folders under `projects/`.
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-ide-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let generation = root.path().join("o00000000");
        std::fs::create_dir(&generation).unwrap();
        std::fs::set_permissions(&generation, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = crate::config::resolve_config(
            &root.path().join("global.toml"),
            &root.path().join("project.toml"),
        )
        .unwrap();
        let engine = Arc::new(
            Engine::open_with_settings_sources(
                &root.path().join("engine.db"),
                &root.path().join("global.toml"),
                &root.path().join("project.toml"),
            )
            .await
            .unwrap(),
        );
        let db = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        for project in ["alpha", "beta"] {
            let folder = root.path().join("projects").join(project);
            std::fs::create_dir_all(&folder).unwrap();
            crate::ide::workspace::tests::add_project(&db, project, &folder);
        }
        let (server, runtime) = OwnerServer::bind(&generation, engine.clone(), &config)
            .await
            .unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(server.run(stopped));
        Self {
            root,
            engine,
            runtime,
            stop: Some(stop),
            server: Some(server),
        }
    }

    /// The owner socket the IDE server is told to use, read from its launch environment.
    fn socket(&self, capability: &Path) -> PathBuf {
        let env = launch_env(&self.runtime, capability).unwrap();
        assert_eq!(env.len(), 2);
        assert_eq!(env[0].0.as_os_str(), OsStr::new(CAPABILITY_ENV));
        assert_eq!(env[0].1.as_os_str(), capability.as_os_str());
        assert_eq!(env[1].0.as_os_str(), OsStr::new(SOCKET_ENV));
        PathBuf::from(&env[1].1)
    }

    /// Stops the owner server and the engine and waits for both.
    async fn stop(&mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        self.server.take().unwrap().await.unwrap().unwrap();
        self.engine.close().await.unwrap();
    }
}

/// The 37-byte handshake: four magic bytes, a slot byte and a 32-byte token.
pub(in crate::ide::bridge) fn packet(magic: &[u8; 4], slot: u8, token: &[u8; 32]) -> [u8; 37] {
    let mut packet = [0; 37];
    packet[..4].copy_from_slice(magic);
    packet[4] = slot;
    packet[5..].copy_from_slice(token);
    packet
}

/// Decodes lowercase hexadecimal text.
fn unhex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

/// Reads a capability file the way the extension does: exactly 64 lowercase hex digits.
fn token(path: &Path) -> [u8; 32] {
    let text = std::fs::read_to_string(path).unwrap();
    assert_eq!(text.len(), 64);
    assert!(
        text.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    unhex(&text).try_into().unwrap()
}

/// Swaps a capability file's token for its session token, as the extension does first,
/// and checks the file is gone. Inputs: the owner socket and the capability path. Output:
/// the session token.
pub(in crate::ide::bridge) async fn session(socket: &Path, path: &Path) -> [u8; 32] {
    let file = token(path);
    let project = path
        .parent()
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        .unwrap()
        .to_owned();
    let body = request(&project, json!({ "name": "ide.session", "args": {} }));
    let Some(IdeReply::Ok { data }) = exchange(socket, packet(MAGIC, 0, &file), &body).await else {
        panic!("no session for {project}");
    };
    let session: [u8; 32] = unhex(data["token"].as_str().unwrap()).try_into().unwrap();
    assert_ne!(session, file);
    assert!(std::fs::symlink_metadata(path).is_err());
    session
}

/// An IDE request body for one project and one call.
fn request(project: &str, call: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&json!({ "project": project, "call": call })).unwrap()
}

/// The workspace call, which every capability may make for its own project.
fn workspace() -> serde_json::Value {
    json!({ "name": "ide.workspace", "args": {} })
}

/// A refusal reply with its code.
fn rejected(code: &str) -> Option<IdeReply> {
    Some(IdeReply::Rejected { code: code.into() })
}

/// Reads one reply frame, or none once the engine closed the connection.
async fn read_reply(stream: &mut UnixStream) -> Option<IdeReply> {
    let length = stream.read_u32().await.ok()? as usize;
    assert!(length > 0 && length <= FRAME_BYTES_MAX);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.ok()?;
    Some(serde_json::from_slice(&body).unwrap())
}

/// Connects to the owner socket and sends one handshake and one request frame.
async fn send(socket: &Path, packet: [u8; 37], body: &[u8]) -> UnixStream {
    let mut stream = crate::spawn_gate::connect_unix(socket).await.unwrap();
    stream.write_all(&packet).await.unwrap();
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend(body);
    let _ = stream.write_all(&frame).await;
    stream
}

/// Sends one request and reads its single reply.
async fn exchange(socket: &Path, packet: [u8; 37], body: &[u8]) -> Option<IdeReply> {
    let mut stream = send(socket, packet, body).await;
    read_reply(&mut stream).await
}

/// Opens an event subscription and waits for the engine to confirm it.
async fn subscribe(socket: &Path, packet: [u8; 37], project: &str) -> UnixStream {
    let body = request(project, json!({ "name": "ide.events", "args": {} }));
    let mut stream = send(socket, packet, &body).await;
    assert_eq!(
        read_reply(&mut stream).await,
        Some(IdeReply::Ok {
            data: serde_json::Value::Null
        })
    );
    stream
}

/// A change set of a real repository in the running engine's own store, with the session
/// the extension takes for its project's capability. Input: the case. Output: the change
/// set case, the owner socket and the session's handshake.
async fn change_set(case: &Case) -> (ChangeCase, PathBuf, [u8; 37]) {
    let changes = ChangeCase::in_store(Some(case.root.path()));
    let capability = issue(&case.runtime, &changes.project).unwrap();
    let socket = case.socket(&capability);
    let session = session(&socket, &capability).await;
    (changes, socket, packet(MAGIC, 0, &session))
}

/// The handshake timeout of the case's engine, from the same files `Case::start` resolves.
fn handshake_timeout(case: &Case) -> Duration {
    let config = crate::config::resolve_config(
        &case.root.path().join("global.toml"),
        &case.root.path().join("project.toml"),
    )
    .unwrap();
    Duration::from_millis(config.settings.timeout_ms)
}

/// A capability answers IDE requests for its own project only: another project, an owner
/// request body, a call outside the IDE list and an owner handshake carrying its token are
/// all refused, and a second project's capability is its own.
#[tokio::test]
async fn ide_capability_allows_only_ide_requests_for_its_project() {
    let mut case = Case::start().await;
    let alpha = ProjectId::new("alpha").unwrap();
    let beta = ProjectId::new("beta").unwrap();
    let path = issue(&case.runtime, &alpha).unwrap();
    assert_eq!(
        path,
        case.root
            .path()
            .join("ide")
            .join("alpha")
            .join("capability")
    );
    let file = std::fs::symlink_metadata(&path).unwrap();
    assert!(file.is_file());
    assert_eq!(file.mode() & 0o7777, 0o600);
    assert_eq!(file.len(), 64);
    for folder in [
        case.root.path().join("ide"),
        case.root.path().join("ide").join("alpha"),
    ] {
        let folder = std::fs::symlink_metadata(folder).unwrap();
        assert!(folder.is_dir());
        assert_eq!(folder.mode() & 0o7777, 0o700);
    }
    let socket = case.socket(&path);
    assert_eq!(
        socket,
        case.root.path().join("o00000000").join("owner.sock")
    );
    assert_eq!(
        exchange(
            &socket,
            packet(MAGIC, 0, &token(&path)),
            &request("alpha", workspace())
        )
        .await,
        rejected("ide_session_required")
    );
    let first = session(&socket, &path).await;
    let ide = packet(MAGIC, 0, &first);
    let file = case
        .root
        .path()
        .join("ide")
        .join("alpha")
        .join("crazyagent.code-workspace");
    let answer = Some(IdeReply::Ok {
        data: json!({ "path": file, "hidden": 0, "show_all": false }),
    });
    assert_eq!(
        exchange(&socket, ide, &request("alpha", workspace())).await,
        answer
    );
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        written["folders"],
        json!([{ "name": "Project", "path": case.root.path().join("projects").join("alpha") }])
    );
    assert_eq!(
        std::fs::symlink_metadata(&file).unwrap().mode() & 0o7777,
        0o600
    );
    let show_all = json!({ "name": "ide.show_all_agents", "args": { "on": true } });
    assert_eq!(
        exchange(&socket, ide, &request("alpha", show_all)).await,
        Some(IdeReply::Ok {
            data: json!({ "path": file, "hidden": 0, "show_all": true }),
        })
    );
    let show_newest = json!({ "name": "ide.show_all_agents", "args": { "on": false } });
    assert_eq!(
        exchange(&socket, ide, &request("alpha", show_newest)).await,
        answer
    );
    assert_eq!(
        exchange(&socket, ide, &request("beta", workspace())).await,
        rejected("ide_project_refused")
    );
    assert_eq!(
        exchange(&socket, ide, br#"{"usage":{"request_id":"usage-1"}}"#).await,
        rejected("ide_request_invalid")
    );
    let owner_call = json!({ "name": "owner.capabilities", "args": {} });
    assert_eq!(
        exchange(&socket, ide, &request("alpha", owner_call)).await,
        rejected("ide_request_invalid")
    );
    let change_sets = json!({ "name": "ide.change_sets", "args": {} });
    assert_eq!(
        exchange(&socket, ide, &request("alpha", change_sets)).await,
        Some(IdeReply::Ok { data: json!([]) })
    );
    let asked = json!({
        "name": "ide.extension_request",
        "args": { "id": "acme.tools", "version": "1.2.3" }
    });
    assert_eq!(
        exchange(&socket, ide, &request("alpha", asked)).await,
        Some(IdeReply::Ok {
            data: json!({ "state": "confirm" })
        }),
        "a capability may ask for an extension for its own project, which the owner confirms"
    );
    assert_eq!(
        exchange(
            &socket,
            packet(b"CAO1", 1, &first),
            br#"{"usage":{"request_id":"usage-2"}}"#
        )
        .await,
        None
    );
    assert_eq!(
        exchange(
            &socket,
            packet(MAGIC, 1, &first),
            &request("alpha", workspace())
        )
        .await,
        rejected("ide_capability_refused")
    );
    assert_eq!(
        exchange(
            &socket,
            packet(MAGIC, 0, &[7; 32]),
            &request("alpha", workspace())
        )
        .await,
        rejected("ide_capability_refused")
    );
    let second = session(&socket, &issue(&case.runtime, &beta).unwrap()).await;
    assert_ne!(second, first);
    let beta_ide = packet(MAGIC, 0, &second);
    assert_eq!(
        exchange(&socket, beta_ide, &request("alpha", workspace())).await,
        rejected("ide_project_refused")
    );
    assert!(matches!(
        exchange(&socket, beta_ide, &request("beta", workspace())).await,
        Some(IdeReply::Ok { .. })
    ));
    assert_eq!(
        exchange(&socket, ide, &request("alpha", workspace())).await,
        answer
    );
    case.stop().await;
}

/// Revoking a capability, which the IDE server stop does, kills its token, its file and its
/// live event subscriptions; a new launch gets a new token, and the owner server stopping
/// revokes every capability left.
#[tokio::test]
async fn ide_capability_is_dead_after_server_stop() {
    let mut case = Case::start().await;
    let alpha = ProjectId::new("alpha").unwrap();
    let path = issue(&case.runtime, &alpha).unwrap();
    let socket = case.socket(&path);
    let first = session(&socket, &path).await;
    let ide = packet(MAGIC, 0, &first);
    let mut subscriptions = Vec::new();
    for _ in 0..SUBSCRIBERS_MAX {
        subscriptions.push(subscribe(&socket, ide, "alpha").await);
    }
    let extra = request("alpha", json!({ "name": "ide.events", "args": {} }));
    assert_eq!(
        exchange(&socket, ide, &extra).await,
        rejected("ide_events_busy")
    );
    publish(&case.runtime, &alpha, IdeEvent::AgentsChanged);
    for subscription in &mut subscriptions {
        assert_eq!(
            read_reply(subscription).await,
            Some(IdeReply::Event {
                event: IdeEvent::AgentsChanged
            })
        );
    }
    revoke(&case.runtime, &alpha).unwrap();
    assert!(std::fs::symlink_metadata(&path).is_err());
    for subscription in &mut subscriptions {
        assert_eq!(
            read_reply(subscription).await,
            rejected("ide_capability_revoked")
        );
        assert_eq!(read_reply(subscription).await, None);
    }
    assert_eq!(
        exchange(&socket, ide, &request("alpha", workspace())).await,
        rejected("ide_capability_refused")
    );
    revoke(&case.runtime, &alpha).unwrap();
    let path = issue(&case.runtime, &alpha).unwrap();
    let second = session(&socket, &path).await;
    assert_ne!(second, first);
    assert_eq!(
        exchange(&socket, ide, &request("alpha", workspace())).await,
        rejected("ide_capability_refused")
    );
    assert!(matches!(
        exchange(
            &socket,
            packet(MAGIC, 0, &second),
            &request("alpha", workspace())
        )
        .await,
        Some(IdeReply::Ok { .. })
    ));
    let mut live = subscribe(&socket, packet(MAGIC, 0, &second), "alpha").await;
    case.stop().await;
    assert!(std::fs::symlink_metadata(&path).is_err());
    assert_eq!(read_reply(&mut live).await, None);
    assert_eq!(
        issue(&case.runtime, &alpha).err().unwrap().0,
        "ide_unavailable"
    );
}

/// The capability file's token admits only `ide.session`, which swaps it once for a
/// session token and removes the file, so a process that reads the file later gets nothing,
/// and a second session try ends the grant. A file someone replaced is kept, and its grant
/// is dead.
#[tokio::test]
async fn ide_session_takes_the_capability_file_once() {
    let mut case = Case::start().await;
    let alpha = ProjectId::new("alpha").unwrap();
    let path = issue(&case.runtime, &alpha).unwrap();
    let socket = case.socket(&path);
    let file = packet(MAGIC, 0, &token(&path));
    let calls = [
        workspace(),
        json!({ "name": "ide.events", "args": {} }),
        json!({ "name": "ide.terminal", "args": { "cols": 80, "rows": 24, "level": "sandboxed" } }),
    ];
    for call in &calls {
        assert_eq!(
            exchange(&socket, file, &request("alpha", call.clone())).await,
            rejected("ide_session_required"),
            "{call}"
        );
    }
    let taken = json!({ "name": "ide.session", "args": {} });
    assert_eq!(
        exchange(&socket, file, &request("beta", taken.clone())).await,
        rejected("ide_project_refused")
    );
    assert!(path.is_file());
    let first = session(&socket, &path).await;
    let ide = packet(MAGIC, 0, &first);
    assert!(matches!(
        exchange(&socket, ide, &request("alpha", workspace())).await,
        Some(IdeReply::Ok { .. })
    ));
    assert_eq!(
        exchange(&socket, ide, &request("alpha", taken.clone())).await,
        rejected("ide_session_taken")
    );
    assert_eq!(
        exchange(&socket, ide, &request("alpha", workspace())).await,
        rejected("ide_capability_refused"),
        "asking for the session again ends it"
    );
    assert_eq!(
        exchange(&socket, file, &request("alpha", taken.clone())).await,
        rejected("ide_capability_refused")
    );
    let beta = ProjectId::new("beta").unwrap();
    let replaced = issue(&case.runtime, &beta).unwrap();
    let beta_file = packet(MAGIC, 0, &token(&replaced));
    let other = replaced.with_file_name("other");
    std::fs::write(&other, "x").unwrap();
    std::fs::rename(&other, &replaced).unwrap();
    assert_eq!(
        exchange(&socket, beta_file, &request("beta", taken)).await,
        rejected("ide_capability_replaced_preserved")
    );
    assert_eq!(std::fs::read_to_string(&replaced).unwrap(), "x");
    assert_eq!(
        exchange(&socket, beta_file, &request("beta", workspace())).await,
        rejected("ide_capability_replaced_preserved"),
        "the spent token ends the dead grant and the replaced file is kept"
    );
    assert_eq!(std::fs::read_to_string(&replaced).unwrap(), "x");
    assert_eq!(
        exchange(&socket, beta_file, &request("beta", workspace())).await,
        rejected("ide_capability_refused")
    );
    std::fs::remove_file(&replaced).unwrap();
    case.stop().await;
}

/// Something in the IDE sandbox that read the capability file before the extension and
/// took the session first loses it when the extension tries the file's token: the grant
/// ends, its subscription closes and both its tokens are dead, so the race fails closed.
/// A new launch gets a working capability again.
#[tokio::test]
async fn a_second_session_attempt_revokes_the_first_session() {
    let mut case = Case::start().await;
    let alpha = ProjectId::new("alpha").unwrap();
    let path = issue(&case.runtime, &alpha).unwrap();
    let socket = case.socket(&path);
    let file = packet(MAGIC, 0, &token(&path));
    let thief = packet(MAGIC, 0, &session(&socket, &path).await);
    let mut events = subscribe(&socket, thief, "alpha").await;
    let taken = request("alpha", json!({ "name": "ide.session", "args": {} }));

    assert_eq!(
        exchange(&socket, file, &taken).await,
        rejected("ide_session_taken")
    );

    assert_eq!(
        read_reply(&mut events).await,
        rejected("ide_capability_revoked")
    );
    assert_eq!(
        exchange(&socket, thief, &request("alpha", workspace())).await,
        rejected("ide_capability_refused")
    );
    assert_eq!(
        exchange(&socket, file, &taken).await,
        rejected("ide_capability_refused")
    );
    let again = issue(&case.runtime, &alpha).unwrap();
    let owner = packet(MAGIC, 0, &session(&socket, &again).await);
    assert!(matches!(
        exchange(&socket, owner, &request("alpha", workspace())).await,
        Some(IdeReply::Ok { .. })
    ));
    case.stop().await;
}

/// Capabilities stop at `GRANTS_MAX` projects, and no project id can name the `ide`
/// folder itself, so none gets a capability there.
#[tokio::test]
async fn ide_capabilities_stop_at_their_bound() {
    let mut case = Case::start().await;
    assert!(
        ProjectId::new(".").is_err(),
        "a project id never names the ide folder itself"
    );
    for index in 0..GRANTS_MAX {
        issue(&case.runtime, &ProjectId::new(format!("p{index}")).unwrap()).unwrap();
    }
    assert_eq!(
        issue(&case.runtime, &ProjectId::new("one-more").unwrap())
            .err()
            .unwrap()
            .0,
        "ide_capability_capacity"
    );
    let again = issue(&case.runtime, &ProjectId::new("p0").unwrap()).unwrap();
    assert!(again.is_file());
    case.stop().await;
    assert_eq!(
        std::fs::read_dir(case.root.path().join("ide"))
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().path().join("capability").exists())
            .count(),
        0
    );
}

/// Every frame in the extension's fixture is exactly what the engine's own `encode_frame`
/// makes of the same request or reply, the session swap included, so the TypeScript
/// client and the engine agree byte for byte, and the fixture's handshake is the layout
/// the engine admits.
#[test]
fn ide_frames_match_the_extension_fixture() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../apps/ide-extension/test/frames.json"
    ))
    .unwrap();
    let token: [u8; 32] = unhex(fixture["token"].as_str().unwrap())
        .try_into()
        .unwrap();
    assert_eq!(
        hex(&packet(MAGIC, 0, &token)),
        fixture["handshake"].as_str().unwrap()
    );
    let requests = fixture["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 5);
    for entry in requests {
        let frame = unhex(entry["frame"].as_str().unwrap());
        let request: IdeRequest = serde_json::from_slice(&frame[4..]).unwrap();
        request.validate().unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), entry["value"]);
        assert_eq!(encode_frame(&request).unwrap(), frame);
    }
    let replies = fixture["replies"].as_array().unwrap();
    assert_eq!(replies.len(), 6);
    for entry in replies {
        let frame = unhex(entry["frame"].as_str().unwrap());
        let reply: IdeReply = serde_json::from_slice(&frame[4..]).unwrap();
        assert_eq!(serde_json::to_value(&reply).unwrap(), entry["value"]);
        assert_eq!(encode_frame(&reply).unwrap(), frame);
    }
    let session = &fixture["session"];
    let frame = unhex(session["request"]["frame"].as_str().unwrap());
    let request: IdeRequest = serde_json::from_slice(&frame[4..]).unwrap();
    request.validate().unwrap();
    assert_eq!(request.call, IdeCall::Session {});
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        session["request"]["value"]
    );
    assert_eq!(encode_frame(&request).unwrap(), frame);
    let token: [u8; 32] = unhex(session["token"].as_str().unwrap())
        .try_into()
        .unwrap();
    let reply = IdeReply::Ok {
        data: json!({ "token": hex(&token) }),
    };
    assert_eq!(
        encode_frame(&reply).unwrap(),
        unhex(session["reply"]["frame"].as_str().unwrap())
    );
}

/// The extension's Sandboxed terminal fixture is exactly what the engine encodes: the
/// `ide.terminal` call, the input, resize and detach requests, the `ok` reply, then the
/// output and closing frames.
#[test]
fn ide_terminal_frames_match_the_extension_fixture() {
    use crate::terminal::{TerminalFrame, TerminalRequest};
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../apps/ide-extension/test/frames.json"
    ))
    .unwrap();
    let terminal = &fixture["terminal"];
    let open = unhex(terminal["open"]["frame"].as_str().unwrap());
    let request: IdeRequest = serde_json::from_slice(&open[4..]).unwrap();
    request.validate().unwrap();
    assert_eq!(
        request.call,
        IdeCall::Terminal {
            cols: 80,
            rows: 24,
            level: AgentAccess::Sandboxed
        }
    );
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        terminal["open"]["value"]
    );
    assert_eq!(encode_frame(&request).unwrap(), open);
    let requests = terminal["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 3);
    for entry in requests {
        let frame = unhex(entry["frame"].as_str().unwrap());
        let request: TerminalRequest = serde_json::from_slice(&frame[4..]).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), entry["value"]);
        assert_eq!(encode_frame(&request).unwrap(), frame);
    }
    let frames = terminal["frames"].as_array().unwrap();
    let (opened, rest) = frames.split_first().unwrap();
    let frame = unhex(opened["frame"].as_str().unwrap());
    let reply: IdeReply = serde_json::from_slice(&frame[4..]).unwrap();
    assert_eq!(serde_json::to_value(&reply).unwrap(), opened["value"]);
    assert_eq!(encode_frame(&reply).unwrap(), frame);
    assert_eq!(rest.len(), 3);
    for entry in rest {
        let frame = unhex(entry["frame"].as_str().unwrap());
        let pushed: TerminalFrame = serde_json::from_slice(&frame[4..]).unwrap();
        assert_eq!(serde_json::to_value(&pushed).unwrap(), entry["value"]);
        assert_eq!(encode_frame(&pushed).unwrap(), frame);
    }
}

/// Request fields past their bounds are refused before any handler runs.
#[test]
fn ide_request_bounds_are_refused() {
    let id = "0123456789abcdef0123456789abcdef";
    let head = "a".repeat(40);
    let refused = [
        json!({ "name": "ide.base_blob", "args": { "id": id, "head": head, "path": "", "page": 0 } }),
        json!({ "name": "ide.base_blob", "args": { "id": id, "head": head, "path": "a".repeat(PATH_BYTES_MAX + 1), "page": 0 } }),
        json!({ "name": "ide.base_blob", "args": { "id": id, "head": head, "path": "src/a\u{0}b.rs", "page": 0 } }),
        json!({ "name": "ide.config_blob", "args": { "id": id, "head": head, "path": "", "page": 0 } }),
        json!({ "name": "ide.config_blob", "args": { "id": id, "head": head, "path": "../agent-7/.vscode/settings.json", "page": 0 } }),
        json!({ "name": "ide.head_blob", "args": { "id": id, "head": head, "path": "", "page": 0 } }),
        json!({ "name": "ide.head_blob", "args": { "id": id, "head": head, "path": "/etc/passwd", "page": 0 } }),
        json!({ "name": "ide.head_blob", "args": { "id": id, "head": head, "path": "../repo/a.rs", "page": 0 } }),
        json!({ "name": "ide.comment", "args": { "id": id, "path": "a.rs", "line": 0, "text": "x" } }),
        json!({ "name": "ide.comment", "args": { "id": id, "path": "a.rs", "line": 1, "text": " " } }),
        json!({ "name": "ide.comment", "args": { "id": id, "path": "a.rs", "line": 1, "text": "x".repeat(COMMENT_BYTES_MAX + 1) } }),
        json!({ "name": "ide.accept", "args": { "id": id, "head": "a".repeat(40), "message": "m".repeat(TEXT_BYTES_MAX + 1) } }),
        json!({ "name": "ide.extension_request", "args": { "id": "../evil", "version": "1.0.0" } }),
        json!({ "name": "ide.extension_request", "args": { "id": "pub.name", "version": "1.0.0/x" } }),
        json!({ "name": "ide.terminal", "args": { "cols": SIZE_MIN - 1, "rows": 24, "level": "sandboxed" } }),
        json!({ "name": "ide.terminal", "args": { "cols": 80, "rows": SIZE_MAX + 1, "level": "sandboxed" } }),
        json!({ "name": "ide.git", "args": { "args": [], "folder": "/tmp/project" } }),
        json!({ "name": "ide.git", "args": { "args": vec!["fetch"; GIT_ARGS_MAX + 1], "folder": "/tmp/project" } }),
        json!({ "name": "ide.git", "args": { "args": ["push", ""], "folder": "/tmp/project" } }),
        json!({ "name": "ide.git", "args": { "args": ["push", "a".repeat(GIT_ARG_BYTES_MAX + 1)], "folder": "/tmp/project" } }),
        json!({ "name": "ide.git", "args": { "args": ["push", "or\u{0}igin"], "folder": "/tmp/project" } }),
        json!({ "name": "ide.git", "args": { "args": ["push"], "folder": "project" } }),
        json!({ "name": "ide.git", "args": { "args": ["push"], "folder": format!("/{}", "a".repeat(PATH_BYTES_MAX)) } }),
        json!({ "name": "ide.git", "args": { "args": ["push"], "folder": "/tmp/pro\u{0}ject" } }),
    ];
    for call in refused {
        let request: IdeRequest = serde_json::from_slice(&request("alpha", call.clone())).unwrap();
        assert_eq!(
            request.validate().err().unwrap().0,
            "ide_request_invalid",
            "{call}"
        );
    }
    let fine: IdeRequest = serde_json::from_slice(&request(
        "alpha",
        json!({ "name": "ide.extension_request", "args": { "id": "redhat.vscode-yaml", "version": "1.18.0" } }),
    ))
    .unwrap();
    fine.validate().unwrap();
    let terminal: IdeRequest = serde_json::from_slice(&request(
        "alpha",
        json!({ "name": "ide.terminal", "args": { "cols": 80, "rows": 24, "level": "sandboxed" } }),
    ))
    .unwrap();
    terminal.validate().unwrap();
    let git: IdeRequest = serde_json::from_slice(&request(
        "alpha",
        json!({ "name": "ide.git", "args": { "args": ["push", "-u", "origin", "main"], "folder": "/tmp/project" } }),
    ))
    .unwrap();
    git.validate().unwrap();
    let elsewhere = request(
        "alpha",
        json!({ "name": "ide.terminal", "args": { "cols": 80, "rows": 24, "level": "sandboxed", "cwd": "/tmp" } }),
    );
    assert!(serde_json::from_slice::<IdeRequest>(&elsewhere).is_err());
    for args in [
        json!({ "cols": 80, "rows": 24 }),
        json!({ "cols": 80, "rows": 24, "level": "root" }),
    ] {
        let unlevelled = request("alpha", json!({ "name": "ide.terminal", "args": args }));
        assert!(
            serde_json::from_slice::<IdeRequest>(&unlevelled).is_err(),
            "{args}"
        );
    }
}

/// Every file tsc compiled into the extension's `out` folder ships in the embedded
/// extension, so an import between its modules never fails inside the IDE.
#[test]
fn every_compiled_extension_file_is_embedded() {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/ide-extension/out");
    let mut compiled = Vec::new();
    let mut folders = vec![out.clone()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else {
                let name = path.strip_prefix(&out).unwrap().to_str().unwrap();
                compiled.push(format!("out/{name}"));
            }
        }
    }
    compiled.sort();
    let mut embedded: Vec<String> = EXTENSION_FILES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .filter(|name| name.starts_with("out/"))
        .collect();
    embedded.sort();
    assert!(compiled.len() > 2, "{compiled:?}");
    assert_eq!(embedded, compiled);
}

/// The extension installs as one folder named `publisher.name-version` from its
/// `package.json`, holding exactly the embedded files, every file tsc compiled among them;
/// a changed file is put back and an unchanged folder is left as it is.
#[test]
fn extension_installs_as_one_folder_with_the_embedded_files() {
    let manifest: serde_json::Value = serde_json::from_slice(EXTENSION_FILES[0].1).unwrap();
    assert_eq!(EXTENSION_FILES[0].0, "package.json");
    assert_eq!(
        EXTENSION,
        format!(
            "{}.{}-{}",
            manifest["publisher"].as_str().unwrap(),
            manifest["name"].as_str().unwrap(),
            manifest["version"].as_str().unwrap()
        )
    );
    let main = manifest["main"].as_str().unwrap().trim_start_matches("./");
    assert!(EXTENSION_FILES.iter().any(|(name, _)| *name == main));
    let extension = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/ide-extension");
    let mut compiled = Vec::new();
    let mut folders = vec![extension.join("out")];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else {
                let name = path.strip_prefix(&extension).unwrap();
                compiled.push(name.to_str().unwrap().to_owned());
            }
        }
    }
    compiled.sort();
    let mut embedded: Vec<&str> = EXTENSION_FILES
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| name.starts_with("out/"))
        .collect();
    embedded.sort_unstable();
    assert_eq!(compiled, embedded);
    let extensions = tempfile::tempdir().unwrap();
    let folder = install_extension(extensions.path()).unwrap();
    assert_eq!(folder, extensions.path().join(EXTENSION));
    for (name, bytes) in EXTENSION_FILES {
        assert_eq!(std::fs::read(folder.join(name)).unwrap(), bytes);
    }
    let before = std::fs::symlink_metadata(&folder).unwrap().ino();
    install_extension(extensions.path()).unwrap();
    assert_eq!(std::fs::symlink_metadata(&folder).unwrap().ino(), before);
    std::fs::write(folder.join(EXTENSION_FILES[1].0), b"changed").unwrap();
    install_extension(extensions.path()).unwrap();
    assert_eq!(
        std::fs::read(folder.join(EXTENSION_FILES[1].0)).unwrap(),
        EXTENSION_FILES[1].1
    );
    let mut left: Vec<String> = std::fs::read_dir(extensions.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, vec![EXTENSION.to_owned(), PROFILE.to_owned()]);
}

/// The IDE server loads only the extensions its profile file lists and deletes every other
/// folder at start, so the built-in is listed beside the approved ones the server's own
/// installs wrote, once, and an unchanged list is left alone.
#[test]
fn the_built_in_extension_is_listed_in_the_server_profile() {
    let extensions = tempfile::tempdir().unwrap();
    let profile = extensions.path().join(PROFILE);
    let approved = json!({
        "identifier": { "id": "golang.go", "uuid": "d6f6cfea-4b6f-41f4-b571-6ad2ab7918da" },
        "version": "0.47.1",
        "location": { "$mid": 1, "path": "/elsewhere/golang.go-0.47.1", "scheme": "file" },
        "relativeLocation": "golang.go-0.47.1",
        "metadata": { "installedTimestamp": 1_761_000_000_000_u64, "source": "vsix" }
    });
    let older = json!({
        "identifier": { "id": "CrazyAgent.CrazyAgent-IDE" },
        "version": "0.0.9",
        "location": { "$mid": 1, "path": "/elsewhere/crazyagent.crazyagent-ide-0.0.9", "scheme": "file" },
        "relativeLocation": "crazyagent.crazyagent-ide-0.0.9"
    });
    std::fs::write(
        &profile,
        serde_json::to_vec(&json!([older, approved])).unwrap(),
    )
    .unwrap();
    let folder = install_extension(extensions.path()).unwrap();
    let listed: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(&profile).unwrap()).unwrap();
    assert_eq!(
        listed,
        vec![
            approved,
            json!({
                "identifier": { "id": "crazyagent.crazyagent-ide" },
                "version": "0.1.0",
                "location": { "$mid": 1, "path": folder.to_str().unwrap(), "scheme": "file" },
                "relativeLocation": EXTENSION
            })
        ]
    );
    let before = std::fs::symlink_metadata(&profile).unwrap().ino();
    install_extension(extensions.path()).unwrap();
    assert_eq!(std::fs::symlink_metadata(&profile).unwrap().ino(), before);
    std::fs::write(&profile, b"not json").unwrap();
    assert_eq!(
        install_extension(extensions.path()).unwrap_err().0,
        "ide_extension_install_failed"
    );
    std::fs::remove_file(&profile).unwrap();
    std::os::unix::fs::symlink(extensions.path().join(EXTENSION), &profile).unwrap();
    assert_eq!(
        install_extension(extensions.path()).unwrap_err().0,
        "ide_extension_install_failed"
    );
    std::fs::remove_file(&profile).unwrap();
    install_extension(extensions.path()).unwrap();
    let listed: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(&profile).unwrap()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["identifier"]["id"], "crazyagent.crazyagent-ide");
}

/// Every file the extension's `package.json` names by a relative path, its entry point and
/// its two color themes among them, ships in the embedded extension with the repository's
/// bytes, so the installed folder holds each file VSCodium loads from it.
#[test]
fn every_file_the_extension_manifest_names_is_embedded() {
    let manifest: serde_json::Value = serde_json::from_slice(EXTENSION_FILES[0].1).unwrap();
    let mut named = Vec::new();
    let mut values = vec![&manifest];
    while let Some(value) = values.pop() {
        match value {
            serde_json::Value::String(text) if text.starts_with("./") => {
                named.push(text.trim_start_matches("./").to_owned());
            }
            serde_json::Value::Array(items) => values.extend(items),
            serde_json::Value::Object(fields) => values.extend(fields.values()),
            _ => {}
        }
    }
    for wanted in [
        "out/extension.js",
        "themes/crazyagent-dark.json",
        "themes/crazyagent-light.json",
    ] {
        assert!(named.iter().any(|name| name == wanted), "{named:?}");
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/ide-extension");
    for name in &named {
        let (_, bytes) = EXTENSION_FILES
            .iter()
            .find(|(path, _)| *path == name.as_str())
            .unwrap_or_else(|| panic!("{name} is named in package.json but not embedded"));
        assert_eq!(
            *bytes,
            std::fs::read(source.join(name)).unwrap().as_slice(),
            "{name}"
        );
    }
}

/// An Accept slower than the handshake timeout, here because another writer holds the
/// engine store for `HELD`, answers with the commit it landed instead of a timeout, and
/// the owner's branch holds that commit.
#[tokio::test]
async fn a_slow_ide_accept_answers_with_the_commit_it_landed() {
    let mut case = Case::start().await;
    let (changes, socket, ide) = change_set(&case).await;
    assert!(HELD > handshake_timeout(&case));
    let set = changes.only(NOW);
    let start = changes.owner_head();
    let accept = json!({
        "name": "ide.accept",
        "args": { "id": set.id, "head": set.head, "message": "Land the agent's edits" }
    });
    changes.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    let mut stream = send(&socket, ide, &request(changes.project.as_str(), accept)).await;
    tokio::time::sleep(HELD).await;
    assert_eq!(
        changes.owner_head(),
        start,
        "the Accept waits for the store"
    );
    changes.db.execute_batch("COMMIT").unwrap();
    let reply = read_reply(&mut stream).await;
    assert!(started.elapsed() >= HELD);
    let Some(IdeReply::Ok { data }) = reply else {
        panic!("{reply:?}");
    };
    let landed: AcceptOutcome = serde_json::from_value(data).unwrap();
    let AcceptOutcome::Landed { commit, branch } = landed else {
        panic!("{landed:?}");
    };
    assert_eq!(branch, "main");
    assert_ne!(commit, start);
    assert_eq!(changes.owner_head(), commit);
    assert!(
        change_sets(&changes.db, &changes.project, NOW)
            .unwrap()
            .is_empty(),
        "an accepted set has no group"
    );
    case.stop().await;
}

/// A Discard still running past the handshake timeout when its capability is revoked
/// answers with what it did: the set is discarded and restorable until the time it names,
/// while the revoked capability is refused from then on.
#[tokio::test]
async fn an_ide_discard_answers_with_its_outcome_after_its_capability_is_revoked() {
    let mut case = Case::start().await;
    let (changes, socket, ide) = change_set(&case).await;
    assert!(HELD > handshake_timeout(&case));
    let set = changes.only(NOW);
    let discard = json!({ "name": "ide.discard", "args": { "id": set.id, "head": set.head } });
    changes.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut stream = send(&socket, ide, &request(changes.project.as_str(), discard)).await;
    tokio::time::sleep(HELD).await;
    revoke(&case.runtime, &changes.project).unwrap();
    changes.db.execute_batch("COMMIT").unwrap();
    let reply = read_reply(&mut stream).await;
    let Some(IdeReply::Ok { data }) = reply else {
        panic!("{reply:?}");
    };
    let until: u64 = serde_json::from_value(data).unwrap();
    let discarded = changes.only(until - 1);
    assert_eq!(discarded.state, ChangeState::Discarded);
    assert_eq!(discarded.restorable_until, Some(until));
    assert_eq!(
        exchange(
            &socket,
            ide,
            &request(changes.project.as_str(), workspace())
        )
        .await,
        rejected("ide_capability_refused")
    );
    case.stop().await;
}

/// Every page of a change set's diff and texts is read at the head the IDE listed, which
/// Accept and Discard name too: a head the set does not hold now, such as the commit it
/// started from, is refused as stale on every read, so the owner never reviews one head
/// while Accept lands another (V2PRE-L1). The listed head reads.
#[tokio::test]
async fn ide_pages_asked_at_a_head_the_set_does_not_hold_are_refused_as_stale() {
    let mut case = Case::start().await;
    let (changes, socket, ide) = change_set(&case).await;
    let set = changes.only(NOW);
    let older = changes.owner_head();
    assert_ne!(older, set.head);
    let reads = |head: &str| {
        let file = |name: &str| {
            json!({
                "name": name,
                "args": { "id": set.id, "head": head, "path": "README", "page": 0 }
            })
        };
        [
            json!({ "name": "ide.change_diff", "args": { "id": set.id, "head": head, "page": 0 } }),
            file("ide.base_blob"),
            file("ide.head_blob"),
        ]
    };
    for head in [older.as_str(), "HEAD", ""] {
        for call in reads(head) {
            let body = request(changes.project.as_str(), call.clone());
            assert_eq!(
                exchange(&socket, ide, &body).await,
                rejected("changes_stale"),
                "{call}"
            );
        }
    }
    for call in reads(&set.head) {
        let body = request(changes.project.as_str(), call.clone());
        let reply = exchange(&socket, ide, &body).await;
        assert!(
            matches!(reply, Some(IdeReply::Ok { .. })),
            "{call} {reply:?}"
        );
    }
    case.stop().await;
}

#[test]
fn ide_changes_reads_end_before_the_extension_stops_waiting_and_writes_keep_the_changes_bound() {
    let extension = include_str!("../../../../../apps/ide-extension/src/engine.ts");
    let request_ms: u64 = extension
        .split("const REQUEST_TIME_MS = ")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("the extension declares its request timer")
        .replace('_', "")
        .parse()
        .unwrap();
    assert!(READ_TIME < Duration::from_millis(request_ms));
    assert!(READ_TIME < CHANGES_TIMEOUT);

    let id = ChangeSetId::new("0123456789abcdef0123456789abcdef").unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    let reads = [
        IdeCall::ChangeSets {},
        IdeCall::ChangeDiff {
            id: id.clone(),
            head: "0".repeat(40),
            page: 0,
        },
        IdeCall::BaseBlob {
            id: id.clone(),
            head: "0".repeat(40),
            path: "src/lib.rs".into(),
            page: 0,
        },
        IdeCall::HeadBlob {
            id: id.clone(),
            head: "0".repeat(40),
            path: "src/lib.rs".into(),
            page: 0,
        },
    ];
    for call in &reads {
        let before = Instant::now();
        let answered_by = bound(call, deadline, IDE_WRITE_TIME);
        assert!(answered_by >= before + READ_TIME, "{call:?}");
        assert!(answered_by <= Instant::now() + READ_TIME, "{call:?}");
    }
    let writes = [
        IdeCall::Accept {
            id: id.clone(),
            head: "0".repeat(40),
            message: "Land it".into(),
        },
        IdeCall::Discard {
            id: id.clone(),
            head: "0".repeat(40),
        },
        IdeCall::Restore { id },
    ];
    for call in &writes {
        let before = Instant::now();
        let answered_by = bound(call, deadline, IDE_WRITE_TIME);
        assert!(answered_by >= before + CHANGES_TIMEOUT, "{call:?}");
        assert!(answered_by <= Instant::now() + CHANGES_TIMEOUT, "{call:?}");
    }
    for call in [IdeCall::Workspace {}, IdeCall::Agents {}] {
        assert_eq!(bound(&call, deadline, IDE_WRITE_TIME), deadline, "{call:?}");
    }
    let late = Instant::now() + CHANGES_TIMEOUT + READ_TIME;
    assert_eq!(bound(&IdeCall::ChangeSets {}, late, IDE_WRITE_TIME), late);
}

/// A diff comment, an owner edit, a message to an agent, a memory or extension decision, an
/// extension request and the show-all switch run under their own write bound, past the
/// handshake deadline and within the extension's request timer, and past it each answers
/// its own outcome-unknown code, which the IDE has a sentence for, as Accept, Discard and
/// Restore answer theirs; reads and other calls write nothing and keep the plain timeout.
#[test]
fn ide_writes_run_under_their_own_bound_and_answer_an_unknown_outcome_past_it() {
    assert!(IDE_WRITE_TIME <= READ_TIME);
    let sentences = include_str!("../../../../../apps/ide-extension/src/scm-model.ts");
    let id = ChangeSetId::new("0123456789abcdef0123456789abcdef").unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    let writes = [
        (
            IdeCall::Comment {
                id: id.clone(),
                path: "README".into(),
                line: 1,
                text: "Say more here".into(),
            },
            "ide_comment_outcome_unknown",
        ),
        (
            IdeCall::OwnerEdit {
                agent: AgentId::new("codex-1").unwrap(),
                path: "README".into(),
            },
            "ide_owner_edit_outcome_unknown",
        ),
        (
            IdeCall::AgentMessage {
                agent: 7,
                text: "Go on".into(),
            },
            "ide_agent_message_outcome_unknown",
        ),
        (
            IdeCall::MemoryDecide {
                id: RequestId::new("memory-1").unwrap(),
                revision: 1,
                decision: Decision::Approve,
            },
            "ide_memory_decide_outcome_unknown",
        ),
        (
            IdeCall::ExtensionDecide {
                id: "publisher.name".into(),
                version: "1.0.0".into(),
                approve: true,
            },
            "ide_extension_decide_outcome_unknown",
        ),
        (
            IdeCall::ExtensionRequest {
                id: "publisher.name".into(),
                version: "1.0.0".into(),
            },
            "ide_extension_request_outcome_unknown",
        ),
        (
            IdeCall::ShowAllAgents { on: true },
            "ide_show_all_agents_outcome_unknown",
        ),
    ];
    for (call, code) in &writes {
        let before = Instant::now();
        let answered_by = bound(call, deadline, IDE_WRITE_TIME);
        assert!(answered_by >= before + IDE_WRITE_TIME, "{call:?}");
        assert!(answered_by <= Instant::now() + IDE_WRITE_TIME, "{call:?}");
        assert_eq!(outcome_unknown(call), Some(*code), "{call:?}");
        assert!(
            sentences.contains(&format!("{code}: \"")),
            "the IDE has no sentence for {code}"
        );
    }
    let changes = [
        IdeCall::Accept {
            id: id.clone(),
            head: "0".repeat(40),
            message: "Land it".into(),
        },
        IdeCall::Discard {
            id: id.clone(),
            head: "0".repeat(40),
        },
        IdeCall::Restore { id: id.clone() },
    ];
    for call in &changes {
        assert_eq!(
            outcome_unknown(call),
            Some("changes_outcome_unknown"),
            "{call:?}"
        );
    }
    let reads = [
        IdeCall::Workspace {},
        IdeCall::Agents {},
        IdeCall::ChangeSets {},
        IdeCall::HeadBlob {
            id,
            head: "0".repeat(40),
            path: "README".into(),
            page: 0,
        },
        IdeCall::Memories { after: None },
        IdeCall::Extensions {},
    ];
    for call in &reads {
        assert_eq!(outcome_unknown(call), None, "{call:?}");
    }
}

/// Takes the engine database's write lock on a connection of the test's own, as a long
/// engine write or a slow disk would, so an IDE write waits on it within the busy timeout.
/// Input: the engine root. Output: the connection, holding the lock until it rolls back.
fn held_writes(root: &Path) -> rusqlite::Connection {
    let held = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    held.busy_timeout(crate::landing::BUSY_WAIT).unwrap();
    held.execute_batch("BEGIN IMMEDIATE").unwrap();
    held
}

/// Over a real connection, a write still waiting on the engine database when its bound
/// passes is answered with its own outcome-unknown code and nothing else, never the plain
/// timeout, since it may still take effect. The handshake keeps 2 s to read the frame, so
/// only the write runs out of time while it waits within the busy timeout.
#[tokio::test]
async fn a_write_past_its_bound_answers_its_unknown_outcome_over_the_connection() {
    let mut case = Case::start().await;
    let alpha = ProjectId::new("alpha").unwrap();
    let path = issue(&case.runtime, &alpha).unwrap();
    let socket = case.socket(&path);
    let ide = packet(MAGIC, 0, &session(&socket, &path).await);
    let handshake = Duration::from_secs(2);
    assert!(
        handshake < crate::landing::BUSY_WAIT,
        "the write still waits when its bound passes"
    );
    let held = held_writes(case.root.path());
    let (client, server) = crate::spawn_gate::unix_pair_std().unwrap();
    for end in [&client, &server] {
        end.set_nonblocking(true).unwrap();
    }
    let mut client = UnixStream::from_std(client).unwrap();
    let server = UnixStream::from_std(server).unwrap();
    let serving = tokio::spawn(serve_within(
        server,
        ide,
        case.runtime.clone(),
        Instant::now() + handshake,
        Duration::from_millis(300),
    ));
    let body = request(
        "alpha",
        json!({ "name": "ide.show_all_agents", "args": { "on": true } }),
    );
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend(body);
    client.write_all(&frame).await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(20), read_reply(&mut client))
        .await
        .expect("the write's bound ends the wait");
    assert_eq!(reply, rejected("ide_show_all_agents_outcome_unknown"));
    assert_eq!(
        read_reply(&mut client).await,
        None,
        "one answer, then the end"
    );
    held.execute_batch("ROLLBACK").unwrap();
    serving.await.unwrap();
    case.stop().await;
}

/// A write that finishes after its capability was revoked still reports what it did over
/// the owner socket, since it took effect; the test revokes once the handshake deadline
/// passed while the write waits on the engine database, then lets it finish.
#[tokio::test]
async fn a_write_that_finishes_after_its_capability_was_revoked_still_reports_it() {
    let mut case = Case::start().await;
    let alpha = ProjectId::new("alpha").unwrap();
    let path = issue(&case.runtime, &alpha).unwrap();
    let socket = case.socket(&path);
    let ide = packet(MAGIC, 0, &session(&socket, &path).await);
    let past = handshake_timeout(&case) + Duration::from_secs(1);
    assert!(
        past < crate::landing::BUSY_WAIT,
        "the write still waits within the busy timeout"
    );
    let held = held_writes(case.root.path());
    let body = request(
        "alpha",
        json!({ "name": "ide.show_all_agents", "args": { "on": true } }),
    );
    let mut stream = send(&socket, ide, &body).await;
    tokio::time::sleep(past).await;
    revoke(&case.runtime, &alpha).unwrap();
    held.execute_batch("ROLLBACK").unwrap();
    let file = case
        .root
        .path()
        .join("ide")
        .join("alpha")
        .join("crazyagent.code-workspace");
    assert_eq!(
        read_reply(&mut stream).await,
        Some(IdeReply::Ok {
            data: json!({ "path": file, "hidden": 0, "show_all": true }),
        })
    );
    assert_eq!(
        exchange(&socket, ide, &request("alpha", workspace())).await,
        rejected("ide_capability_refused"),
        "a call after the revoke is refused"
    );
    case.stop().await;
}

/// An IDE git command runs under `GIT_TIME` and answers within its margin, before the
/// extension's git relay stops waiting, so the relay always hands git the engine's answer
/// or its refusal, never a timer of its own.
#[test]
fn an_ide_git_command_answers_before_the_extension_relay_stops_waiting() {
    let relay = include_str!("../../../../../apps/ide-extension/src/git.ts");
    let relay_ms: u64 = relay
        .split("export const GIT_REQUEST_MS = ")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("the extension declares its git request timer")
        .replace('_', "")
        .parse()
        .unwrap();
    assert!(GIT_TIME + GIT_ANSWER_MARGIN < Duration::from_millis(relay_ms));
    let call = IdeCall::Git {
        args: vec!["push".into(), "origin".into(), "main".into()],
        folder: "/tmp/project".into(),
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    let before = Instant::now();
    let answered_by = bound(&call, deadline, IDE_WRITE_TIME);
    assert!(answered_by >= before + GIT_TIME + GIT_ANSWER_MARGIN);
    assert!(answered_by <= Instant::now() + GIT_TIME + GIT_ANSWER_MARGIN);
}

/// The IDE's push runs in the engine for the project's own folder and lands on a real git
/// daemon on 127.0.0.1, while another project's folder, a `-c` override and an unknown
/// remote are refused without running git.
#[tokio::test]
async fn the_ide_pushes_its_project_folder_through_the_engine_and_nothing_else() {
    use super::remote_git::tests::{Daemon, ok, owner_repository};
    let mut case = Case::start().await;
    let served = case.root.path().join("served");
    std::fs::create_dir(&served).unwrap();
    ok(
        &served,
        &["init", "-q", "--bare", "-b", "main", "remote.git"],
    );
    let daemon = Daemon::start(&served);
    let folder = case.root.path().join("projects").join("alpha");
    owner_repository(&folder, &daemon.url("remote.git"));
    std::fs::write(folder.join("notes.md"), "Planting notes.\n").unwrap();
    ok(&folder, &["add", "notes.md"]);
    ok(&folder, &["commit", "-q", "-m", "Write the notes"]);
    let head = ok(&folder, &["rev-parse", "HEAD"]);
    let project = ProjectId::new("alpha").unwrap();
    let capability = issue(&case.runtime, &project).unwrap();
    let socket = case.socket(&capability);
    let ide = packet(MAGIC, 0, &session(&socket, &capability).await);
    let call = |args: &[&str], folder: &Path| {
        request(
            "alpha",
            json!({ "name": "ide.git", "args": { "args": args, "folder": folder } }),
        )
    };

    let pushed = exchange(
        &socket,
        ide,
        &call(&["push", "origin", "main:main"], &folder),
    )
    .await;
    let Some(IdeReply::Ok { data }) = pushed else {
        panic!("{pushed:?}");
    };
    assert_eq!(data["code"], json!(0), "{data}");
    assert!(
        data["stderr"].as_str().unwrap().contains("main -> main"),
        "{data}"
    );
    assert_eq!(
        ok(
            &served.join("remote.git"),
            &["rev-parse", "refs/heads/main"]
        ),
        head
    );

    let beta = case.root.path().join("projects").join("beta");
    for (args, folder, code) in [
        (
            ["push", "origin", "main"].as_slice(),
            beta.as_path(),
            "ide_git_folder_refused",
        ),
        (
            &["push", "origin", "main"],
            folder.join("src").as_path(),
            "ide_git_folder_refused",
        ),
        (
            &["-c", "core.sshCommand=touch /tmp/owned", "push"],
            folder.as_path(),
            "ide_git_refused",
        ),
        (
            &["push", "upstream", "main"],
            folder.as_path(),
            "ide_git_remote_unknown",
        ),
    ] {
        assert_eq!(
            exchange(&socket, ide, &call(args, folder)).await,
            rejected(code),
            "{args:?} in {}",
            folder.display()
        );
    }
    drop(daemon);
    case.stop().await;
}

/// While git waits on a remote that never answers, each `ide.git` call keeps its place: a
/// project's call past `PROJECT_GIT_CALLS_MAX` and any call past `GIT_CALLS_MAX` in all are
/// refused at once, and once the remote lets go the places are given back.
#[tokio::test]
async fn ide_git_calls_past_their_cap_are_refused_while_the_others_run() {
    use super::remote_git::tests::ok;
    use super::remote_git::{GIT_CALLS_MAX, PROJECT_GIT_CALLS_MAX};
    const LISTEN: Duration = Duration::from_secs(30);
    const REFUSED: Duration = Duration::from_secs(10);
    const { assert!(PROJECT_GIT_CALLS_MAX < GIT_CALLS_MAX) };
    let mut case = Case::start().await;
    let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let url = format!(
        "git://127.0.0.1:{}/slow.git",
        listener.local_addr().unwrap().port()
    );
    let (accepted, mut held) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for stream in crate::spawn_gate::incoming_tcp_std(&listener).flatten() {
            if accepted.send(stream).is_err() {
                return;
            }
        }
    });
    let mut ides = Vec::new();
    for name in ["alpha", "beta"] {
        let folder = case.root.path().join("projects").join(name);
        ok(&folder, &["init", "-q", "-b", "main"]);
        ok(&folder, &["remote", "add", "slow", &url]);
        let capability = issue(&case.runtime, &ProjectId::new(name).unwrap()).unwrap();
        let socket = case.socket(&capability);
        let ide = packet(MAGIC, 0, &session(&socket, &capability).await);
        let body = request(
            name,
            json!({ "name": "ide.git", "args": { "args": ["fetch", "slow"], "folder": folder } }),
        );
        ides.push((socket, ide, body));
    }
    let start = |at: usize| {
        let (socket, ide, body) = ides[at].clone();
        tokio::spawn(async move { exchange(&socket, ide, &body).await })
    };
    let mut running = Vec::new();
    let mut streams = Vec::new();
    for at in std::iter::repeat_n(0, PROJECT_GIT_CALLS_MAX).chain(std::iter::repeat_n(
        1,
        GIT_CALLS_MAX - PROJECT_GIT_CALLS_MAX,
    )) {
        running.push(start(at));
        let stream = tokio::time::timeout(LISTEN, held.recv()).await;
        streams.push(stream.expect("git reached the remote").unwrap());
        if at == 0 && running.len() == PROJECT_GIT_CALLS_MAX {
            let (socket, ide, body) = &ides[0];
            let refused = tokio::time::timeout(REFUSED, exchange(socket, *ide, body)).await;
            assert_eq!(
                refused.expect("refused at once"),
                rejected("ide_git_busy"),
                "a project's call past its own cap"
            );
        }
    }
    let (socket, ide, body) = &ides[1];
    let refused = tokio::time::timeout(REFUSED, exchange(socket, *ide, body)).await;
    assert_eq!(
        refused.expect("refused at once"),
        rejected("ide_git_busy"),
        "a call past the cap of the owner server"
    );
    drop(streams);
    drop(held);
    for call in running {
        let answered = call.await.unwrap();
        let Some(IdeReply::Ok { data }) = answered else {
            panic!("{answered:?}");
        };
        assert_ne!(data["code"], json!(0), "{data}");
    }
    for (socket, ide, body) in &ides {
        let answered = exchange(socket, *ide, body).await;
        assert!(
            matches!(answered, Some(IdeReply::Ok { .. })),
            "a place given back admits the next call: {answered:?}"
        );
    }
    case.stop().await;
}
/// Reads one frame of any kind as JSON, or none once the engine closed the connection.
#[cfg(target_os = "macos")]
async fn read_value(stream: &mut UnixStream) -> Option<serde_json::Value> {
    let length = stream.read_u32().await.ok()? as usize;
    assert!(length > 0 && length <= FRAME_BYTES_MAX);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.ok()?;
    Some(serde_json::from_slice(&body).unwrap())
}

/// Sends one control frame on an open terminal connection.
#[cfg(target_os = "macos")]
async fn control(stream: &mut UnixStream, request: &crate::terminal::TerminalRequest) {
    let body = serde_json::to_vec(request).unwrap();
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend(body);
    stream.write_all(&frame).await.unwrap();
}

/// Reads terminal output until it holds `wanted`, or with no `wanted` until the shell
/// printed anything that is not blank, within 20 seconds. Returns everything read.
#[cfg(target_os = "macos")]
async fn output_until(stream: &mut UnixStream, wanted: Option<&str>) -> String {
    let mut seen = Vec::new();
    let read = async {
        loop {
            let frame = read_value(stream).await.expect("the terminal closed early");
            assert_eq!(frame["kind"], "output", "{frame}");
            for byte in frame["bytes"].as_array().unwrap() {
                seen.push(u8::try_from(byte.as_u64().unwrap()).unwrap());
            }
            let done = match wanted {
                Some(wanted) => String::from_utf8_lossy(&seen).contains(wanted),
                None => seen.iter().any(|byte| !byte.is_ascii_whitespace()),
            };
            if done {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(20), read)
        .await
        .expect("the wanted output never came");
    String::from_utf8_lossy(&seen).into_owned()
}

/// Skips output frames until the terminal's final frame, within 20 seconds.
#[cfg(target_os = "macos")]
async fn closing(stream: &mut UnixStream) -> serde_json::Value {
    let read = async {
        loop {
            let frame = read_value(stream).await.expect("no closing frame");
            if frame["kind"] != "output" {
                return frame;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(20), read)
        .await
        .expect("the terminal never closed")
}

/// Opens a Sandboxed terminal and reads its `ok`. Returns the stream and the pane's name.
#[cfg(target_os = "macos")]
async fn open_terminal(socket: &Path, packet: [u8; 37], body: &[u8]) -> (UnixStream, AgentId) {
    let mut stream = send(socket, packet, body).await;
    let opened = read_value(&mut stream).await.unwrap();
    assert_eq!(opened["kind"], "ok", "{opened}");
    let name = AgentId::new(opened["data"]["terminal"].as_str().unwrap()).unwrap();
    (stream, name)
}

/// The Sandboxed terminal runs in a pane the engine itself starts, outside the IDE
/// server's sandbox, under the project's agent profile: it starts in the project folder,
/// typed input runs, the project's file is readable, the engine database and an IDE
/// capability file are not, the network is closed, at most `TERMINALS_MAX` run at once, a
/// detached terminal's pane ends, and revoking the capability ends the rest.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn sandboxed_terminal_runs_in_an_engine_pane_and_cannot_read_the_capability() {
    use crate::terminal::TerminalRequest;
    let mut case = Case::start().await;
    let alpha = ProjectId::new("alpha").unwrap();
    let folder = case.root.path().join("projects").join("alpha");
    std::fs::write(folder.join("README.md"), "alpha readme\n").unwrap();
    let database = case.root.path().join("engine.db");
    let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let path = issue(&case.runtime, &alpha).unwrap();
    let socket = case.socket(&path);
    let ide = packet(MAGIC, 0, &session(&socket, &path).await);
    let unread = issue(&case.runtime, &ProjectId::new("beta").unwrap()).unwrap();
    let secret = std::fs::read_to_string(&unread).unwrap();
    let body = request(
        "alpha",
        json!({ "name": "ide.terminal", "args": { "cols": 100, "rows": 30, "level": "sandboxed" } }),
    );
    let mut stream = send(&socket, ide, &body).await;
    let opened = read_value(&mut stream).await.unwrap();
    assert_eq!(opened["kind"], "ok", "{opened}");
    assert_eq!(opened["data"]["cols"], 100);
    assert_eq!(opened["data"]["rows"], 30);
    let name = AgentId::new(opened["data"]["terminal"].as_str().unwrap()).unwrap();
    assert!(name.as_str().starts_with("ide-"));
    let host = case.runtime.terminal().unwrap();
    assert!(host.alive().unwrap().contains(&name));
    output_until(&mut stream, None).await;
    let test = |label: &str, command: String| {
        format!(
            "if {command} >/dev/null 2>&1; then echo {label}-$((1)); else echo {label}-$((0)); fi; "
        )
    };
    let mut typed = format!("cat '{}'; echo cwd-$((1))=$(pwd -P); ", unread.display());
    typed.push_str(&test("db", format!("/bin/cat '{}'", database.display())));
    typed.push_str(&test("project", "/bin/cat README.md".to_owned()));
    typed.push_str(&test(
        "net",
        format!("/usr/bin/nc -z -G 2 127.0.0.1 {port}"),
    ));
    typed.push_str("echo ide-$((40+2))\r");
    control(
        &mut stream,
        &TerminalRequest::Input {
            bytes: typed.into_bytes(),
        },
    )
    .await;
    let seen = output_until(&mut stream, Some("ide-42")).await;
    assert!(seen.contains("Operation not permitted"), "{seen}");
    assert!(!seen.contains(&secret), "{seen}");
    for answer in [
        format!("cwd-1={}", folder.display()),
        "db-0".to_owned(),
        "project-1".to_owned(),
        "net-0".to_owned(),
    ] {
        assert!(seen.contains(&answer), "{answer} missing in: {seen}");
    }
    control(&mut stream, &TerminalRequest::Resize { cols: 90, rows: 20 }).await;
    let mut more = Vec::new();
    for _ in 1..terminal::TERMINALS_MAX {
        more.push(open_terminal(&socket, ide, &body).await);
    }
    assert_eq!(
        exchange(&socket, ide, &body).await,
        rejected("ide_terminals_busy")
    );
    for (mut other, other_name) in more {
        control(&mut other, &TerminalRequest::Detach).await;
        assert_eq!(
            closing(&mut other).await,
            json!({ "kind": "closed", "reason": "terminal_detached" })
        );
        assert!(!host.alive().unwrap().contains(&other_name));
    }
    let (mut again, again_name) = open_terminal(&socket, ide, &body).await;
    revoke(&case.runtime, &alpha).unwrap();
    for (open, open_name) in [(&mut stream, &name), (&mut again, &again_name)] {
        assert_eq!(
            closing(open).await,
            json!({ "kind": "closed", "reason": "ide_capability_revoked" })
        );
        assert!(!host.alive().unwrap().contains(open_name));
    }
    case.stop().await;
}

/// CA-SEC-005: input sent on an open Sandboxed terminal after its capability was revoked
/// never reaches the shell. The terminal closes as revoked at that input rather than at the
/// next periodic grant check, and its pane ends. The idle wait before the revoke lets the
/// periodic check's first tick, which fires at once, pass while the grant is still live.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn input_after_its_capability_was_revoked_never_reaches_the_terminal() {
    use crate::terminal::TerminalRequest;
    let mut case = Case::start().await;
    let alpha = ProjectId::new("alpha").unwrap();
    let marker = case
        .root
        .path()
        .join("projects")
        .join("alpha")
        .join("typed-after-revoke");
    let path = issue(&case.runtime, &alpha).unwrap();
    let socket = case.socket(&path);
    let ide = packet(MAGIC, 0, &session(&socket, &path).await);
    let body = request(
        "alpha",
        json!({ "name": "ide.terminal", "args": { "cols": 80, "rows": 24, "level": "sandboxed" } }),
    );
    let (mut stream, name) = open_terminal(&socket, ide, &body).await;
    let host = case.runtime.terminal().unwrap();
    output_until(&mut stream, None).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    revoke(&case.runtime, &alpha).unwrap();
    control(
        &mut stream,
        &TerminalRequest::Input {
            bytes: format!("/usr/bin/touch '{}'\r", marker.display()).into_bytes(),
        },
    )
    .await;
    assert_eq!(
        closing(&mut stream).await,
        json!({ "kind": "closed", "reason": "ide_capability_revoked" })
    );
    assert!(!host.alive().unwrap().contains(&name));
    assert!(
        !marker.exists(),
        "the shell ran input sent after the capability was revoked"
    );
    case.stop().await;
}

/// A terminal asked for above the project's level is refused before any pane starts.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_terminal_asked_above_the_projects_level_never_starts_a_pane() {
    let mut case = Case::start().await;
    let alpha = ProjectId::new("alpha").unwrap();
    let path = issue(&case.runtime, &alpha).unwrap();
    let socket = case.socket(&path);
    let body = request(
        "alpha",
        json!({ "name": "ide.terminal", "args": { "cols": 80, "rows": 24, "level": "own_branch" } }),
    );
    let ide = packet(MAGIC, 0, &session(&socket, &path).await);
    assert_eq!(
        exchange(&socket, ide, &body).await,
        rejected("ide_terminal_above_project")
    );
    assert!(!case.root.path().join("ide-shell").exists());
    case.stop().await;
}

/// Settings that stop reading after the engine started stop a new terminal, which never starts
/// with no local model port kept.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_settings_file_that_cannot_be_read_stops_the_terminal() {
    let mut case = Case::start().await;
    std::fs::write(case.root.path().join("global.toml"), "not = = toml").unwrap();
    let alpha = ProjectId::new("alpha").unwrap();
    let path = issue(&case.runtime, &alpha).unwrap();
    let socket = case.socket(&path);
    let body = request(
        "alpha",
        json!({ "name": "ide.terminal", "args": { "cols": 80, "rows": 24, "level": "sandboxed" } }),
    );
    let ide = packet(MAGIC, 0, &session(&socket, &path).await);
    assert_eq!(
        exchange(&socket, ide, &body).await,
        rejected("ide_settings_unavailable")
    );
    assert!(!case.root.path().join("ide-shell").exists());
    case.stop().await;
}

/// A level above the project's is refused.
#[test]
fn a_terminal_above_the_projects_level_is_refused() {
    assert_eq!(
        terminal::terminal_level(AgentAccess::Sandboxed, AgentAccess::OwnBranch),
        Err("ide_terminal_above_project")
    );
}

/// The engine never starts a Full access terminal, even for a Full access project.
#[test]
fn an_engine_terminal_is_never_full_access() {
    assert_eq!(
        terminal::terminal_level(AgentAccess::Full, AgentAccess::Full),
        Err("ide_terminal_level_refused")
    );
}

/// A Full access project may still open a narrower terminal.
#[test]
fn a_full_access_project_can_open_a_narrower_terminal() {
    assert_eq!(
        terminal::terminal_level(AgentAccess::Full, AgentAccess::Sandboxed),
        Ok(AgentAccess::Sandboxed)
    );
}

/// Where no sandbox is qualified for the Sandboxed terminal, the call is refused before
/// any pane starts.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn sandboxed_terminal_is_refused_where_the_sandbox_is_not_qualified() {
    let mut case = Case::start().await;
    let alpha = ProjectId::new("alpha").unwrap();
    let path = issue(&case.runtime, &alpha).unwrap();
    let socket = case.socket(&path);
    let body = request(
        "alpha",
        json!({ "name": "ide.terminal", "args": { "cols": 80, "rows": 24, "level": "sandboxed" } }),
    );
    let ide = packet(MAGIC, 0, &session(&socket, &path).await);
    assert_eq!(
        exchange(&socket, ide, &body).await,
        rejected("ide_terminal_unavailable")
    );
    assert!(!case.root.path().join("ide-shell").exists());
    case.stop().await;
}

/// Revoking every capability as the owner server stops keeps removing the later files after
/// one fails, and answers that first failure.
#[test]
fn revoking_every_capability_keeps_going_past_a_replaced_file() {
    let root = tempfile::tempdir().unwrap();
    let grants = Grants::default();
    let alpha = grants
        .issue(root.path(), &ProjectId::new("alpha").unwrap())
        .unwrap();
    let beta = grants
        .issue(root.path(), &ProjectId::new("beta").unwrap())
        .unwrap();
    let replacement = alpha.with_extension("new");
    std::fs::write(&replacement, "replaced\n").unwrap();
    std::fs::rename(&replacement, &alpha).unwrap();
    assert_eq!(
        grants.revoke_all().unwrap_err().0,
        "ide_capability_replaced_preserved"
    );
    assert_eq!(std::fs::read_to_string(&alpha).unwrap(), "replaced\n");
    assert!(
        std::fs::symlink_metadata(&beta).is_err(),
        "the capability after the replaced one is still revoked"
    );
    assert!(grants.revoke_all().is_ok(), "nothing is left to revoke");
}
