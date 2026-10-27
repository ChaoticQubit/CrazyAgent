use super::*;
use crate::engine::Engine;
use crate::engine::owner::OwnerServer;
use crate::protocol::FRAME_BYTES_MAX;
use serde_json::json;
use std::ffi::OsStr;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    /// so the temporary root plays the engine root that holds `ide/`.
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
            Engine::open(&root.path().join("engine.db"), config.clone())
                .await
                .unwrap(),
        );
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
fn packet(magic: &[u8; 4], slot: u8, token: &[u8; 32]) -> [u8; 37] {
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
    let first = token(&path);
    let ide = packet(MAGIC, 0, &first);
    let answer = Some(IdeReply::Ok {
        data: json!({
            "path": case.root.path().join("ide").join("alpha").join("crazyagent.code-workspace")
        }),
    });
    assert_eq!(
        exchange(&socket, ide, &request("alpha", workspace())).await,
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
    let unbuilt = json!({ "name": "ide.change_sets", "args": {} });
    assert_eq!(
        exchange(&socket, ide, &request("alpha", unbuilt)).await,
        rejected("ide_request_unavailable")
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
    let second = token(&issue(&case.runtime, &beta).unwrap());
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
    let first = token(&path);
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
    let second = token(&path);
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

/// Capabilities stop at `GRANTS_MAX` projects, and a project id that names the `ide`
/// folder itself never gets a capability.
#[tokio::test]
async fn ide_capabilities_stop_at_their_bound() {
    let mut case = Case::start().await;
    assert_eq!(
        issue(&case.runtime, &ProjectId::new(".").unwrap())
            .err()
            .unwrap()
            .0,
        "ide_project_invalid"
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
/// makes of the same request or reply, so the TypeScript client and the engine agree byte
/// for byte, and the fixture's handshake is the layout the engine admits.
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
}

/// Request fields past their bounds are refused before any handler runs.
#[test]
fn ide_request_bounds_are_refused() {
    let id = "0123456789abcdef0123456789abcdef";
    let refused = [
        json!({ "name": "ide.base_blob", "args": { "id": id, "path": "" } }),
        json!({ "name": "ide.base_blob", "args": { "id": id, "path": "a".repeat(PATH_BYTES_MAX + 1) } }),
        json!({ "name": "ide.base_blob", "args": { "id": id, "path": "src/a\u{0}b.rs" } }),
        json!({ "name": "ide.comment", "args": { "id": id, "path": "a.rs", "line": 0, "text": "x" } }),
        json!({ "name": "ide.comment", "args": { "id": id, "path": "a.rs", "line": 1, "text": " " } }),
        json!({ "name": "ide.accept", "args": { "id": id, "message": "m".repeat(TEXT_BYTES_MAX + 1) } }),
        json!({ "name": "ide.extension_request", "args": { "id": "../evil", "version": "1.0.0" } }),
        json!({ "name": "ide.extension_request", "args": { "id": "pub.name", "version": "1.0.0/x" } }),
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
/// `package.json`, holding exactly the embedded files; a changed file is put back and an
/// unchanged folder is left as it is.
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
    assert_eq!(std::fs::read_dir(extensions.path()).unwrap().count(), 1);
}
