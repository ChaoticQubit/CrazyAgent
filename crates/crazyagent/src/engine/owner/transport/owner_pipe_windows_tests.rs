use super::*;
use std::path::PathBuf;

/// A fresh temp folder spelled as Windows names it, without the `\\?\` prefix, so an owner
/// root inside it passes W3-1's canonical twin. Output: the temp folder, kept alive by the
/// caller, and its plain canonical path.
fn canonical_temp() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let plain = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    (temp, plain)
}

/// An engine in a fresh temp folder and a private owner root below it, as bootstrap makes a
/// generation. Output: the temp folder, kept alive by the caller, the engine, its settings
/// and the owner root.
pub(super) async fn engine_and_owner_root() -> (
    tempfile::TempDir,
    Arc<Engine>,
    crate::config::EffectiveConfig,
    PathBuf,
) {
    let (temp, plain) = canonical_temp();
    let global = plain.join("g");
    std::fs::write(&global, "").unwrap();
    let config = crate::config::resolve_config(&global, &plain.join("p")).unwrap();
    let engine = Engine::open(&plain.join("db"), config.clone())
        .await
        .unwrap();
    let owners = plain.join("gen");
    drop(crazyagent_win_sandbox::private::dir(&owners).unwrap());
    (temp, Arc::new(engine), config, owners)
}

/// A capabilities read, the smallest owner request every client may make. Input: the
/// request id. Output: the request.
fn read_capabilities(id: &str) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(0),
        operation: OwnerOperation::Read {
            query: OwnerRead::Capabilities,
        },
    }
}

/// W3-3: the desktop client admits with its capability over the owner pipe and its read
/// comes back with the same request id.
#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_client_admits_and_reads_over_the_owner_pipe() {
    let (_temp, engine, config, owners) = engine_and_owner_root().await;
    let (server, _host) = OwnerServer::bind(&owners, engine.clone(), &config)
        .await
        .unwrap();
    let (stop, stopped) = oneshot::channel();
    let running = tokio::spawn(server.run(stopped));
    let client =
        OwnerClient::attach(&owners, OwnerClientKind::Desktop, Duration::from_secs(10)).unwrap();
    let reply = client
        .request(read_capabilities("desktop-read"))
        .await
        .unwrap();
    assert_eq!(reply.request_id.as_str(), "desktop-read");
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
    engine.close().await.unwrap();
}

/// R1: once the engine stops and a rogue publishes its own pipe for the owner endpoint, a
/// client that admitted earlier refuses it before any byte, so its capability never reaches
/// the rogue.
#[tokio::test(flavor = "multi_thread")]
async fn a_rogue_owner_instance_gets_no_byte_of_a_capability() {
    let (_temp, engine, config, owners) = engine_and_owner_root().await;
    let (server, _host) = OwnerServer::bind(&owners, engine.clone(), &config)
        .await
        .unwrap();
    let (stop, stopped) = oneshot::channel();
    let running = tokio::spawn(server.run(stopped));
    let client =
        OwnerClient::attach(&owners, OwnerClientKind::Desktop, Duration::from_secs(10)).unwrap();
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
    let rogue = crate::ipc::serve_unpinned(&owners.join(SOCKET)).unwrap();
    let reading = tokio::spawn(async move {
        let mut rogue = rogue;
        let connected = tokio::time::timeout(Duration::from_secs(5), rogue.connect()).await;
        let mut bytes = Vec::new();
        if connected.is_ok() {
            let _ =
                tokio::time::timeout(Duration::from_secs(5), rogue.read_to_end(&mut bytes)).await;
        }
        bytes
    });
    let refused = client
        .request(read_capabilities("to-rogue"))
        .await
        .unwrap_err();
    assert_eq!(refused.0, "owner_socket_replaced");
    assert!(reading.await.unwrap().is_empty());
    engine.close().await.unwrap();
}

/// W3-3 re-check: an endpoint whose pipes file is rewritten after a client connected reads
/// as another endpoint, so the check before the capability write refuses that stream.
#[tokio::test(flavor = "multi_thread")]
async fn the_check_before_the_capability_write_sees_a_rewritten_endpoint() {
    let (_temp, engine, config, owners) = engine_and_owner_root().await;
    let (server, _host) = OwnerServer::bind(&owners, engine.clone(), &config)
        .await
        .unwrap();
    let (stop, stopped) = oneshot::channel();
    let running = tokio::spawn(server.run(stopped));
    let root = Root::open(&owners).unwrap();
    let before = root.socket_identity().unwrap();
    let stream = crate::ipc::owner_connect(owners.join(SOCKET))
        .await
        .unwrap();
    verify_server(&stream).unwrap();
    assert!(root.socket_identity().unwrap() == before);
    let pipes = owners.join("pipes");
    let text = std::fs::read_to_string(&pipes).unwrap();
    let (line, pin) = text.trim_end().rsplit_once(' ').unwrap();
    let moved = pin.parse::<u64>().unwrap() + 1;
    std::fs::write(&pipes, format!("{line} {moved}\n")).unwrap();
    assert!(root.socket_identity().unwrap() != before);
    drop(stream);
    stop.send(()).unwrap();
    let _ = running.await.unwrap();
    engine.close().await.unwrap();
}

/// W3-3: an owner root whose pipes file already names another endpoint is refused at bind
/// with `pipes_purpose_taken`, and no capability file is left behind.
#[tokio::test(flavor = "multi_thread")]
async fn an_owner_bind_over_another_endpoint_is_refused_and_leaves_no_capability() {
    let (_temp, engine, config, owners) = engine_and_owner_root().await;
    let other = crate::ipc::owner_listener(owners.join("other.sock"), 1).unwrap();
    let refused = OwnerServer::bind(&owners, engine.clone(), &config)
        .await
        .err()
        .unwrap();
    assert!(refused.0.contains("pipes_purpose_taken"), "{}", refused.0);
    for key in KEYS {
        assert!(!owners.join(key).exists(), "{key} left behind");
    }
    crate::ipc::owner_close(other).unwrap();
    engine.close().await.unwrap();
}

/// W3-3: once `run` stops, the owner root holds no pipes file and no capability file, and
/// the endpoint no longer answers.
#[tokio::test(flavor = "multi_thread")]
async fn after_the_owner_server_stops_no_pipes_or_capability_file_is_left() {
    let (_temp, engine, config, owners) = engine_and_owner_root().await;
    let (server, _host) = OwnerServer::bind(&owners, engine.clone(), &config)
        .await
        .unwrap();
    let (stop, stopped) = oneshot::channel();
    let running = tokio::spawn(server.run(stopped));
    assert!(crate::ipc::owner_answering(&owners.join(SOCKET)));
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
    assert!(!owners.join("pipes").exists());
    for key in KEYS {
        assert!(!owners.join(key).exists(), "{key} left behind");
    }
    assert!(!crate::ipc::owner_answering(&owners.join(SOCKET)));
    engine.close().await.unwrap();
}

/// W3-3 bracket (rv-sec2 #719 condition 2): a client whose recorded endpoint no longer
/// matches the pipes file's first read refuses before it connects. The row changes only
/// the pin, so a client that went on would reach the real pipe and fail `verify_server`
/// with `peer_owner_mismatch`; `owner_socket_replaced` shows the refusal came first.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_whose_recorded_endpoint_differs_refuses_before_it_connects() {
    let (_temp, engine, config, owners) = engine_and_owner_root().await;
    let (server, _host) = OwnerServer::bind(&owners, engine.clone(), &config)
        .await
        .unwrap();
    let (stop, stopped) = oneshot::channel();
    let running = tokio::spawn(server.run(stopped));
    let client =
        OwnerClient::attach(&owners, OwnerClientKind::Desktop, Duration::from_secs(10)).unwrap();
    let pipes = owners.join("pipes");
    let text = std::fs::read_to_string(&pipes).unwrap();
    let (line, pin) = text.trim_end().rsplit_once(' ').unwrap();
    let moved = pin.parse::<u64>().unwrap() + 1;
    std::fs::write(&pipes, format!("{line} {moved}\n")).unwrap();
    let refused = client
        .request(read_capabilities("moved-endpoint"))
        .await
        .unwrap_err();
    assert_eq!(refused.0, "owner_socket_replaced");
    std::fs::write(&pipes, text).unwrap();
    stop.send(()).unwrap();
    let _ = running.await.unwrap();
    engine.close().await.unwrap();
}
