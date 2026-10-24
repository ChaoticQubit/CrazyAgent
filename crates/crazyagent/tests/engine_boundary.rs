#![cfg(unix)]

use crazyagent::{
    config::resolve_config,
    engine::{Client, Engine, Server},
    protocol::*,
};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn request(id: &str) -> WireRequest {
    WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("tui").unwrap(),
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(0),
        command: Command::Snapshot,
    }
}

#[tokio::test]
async fn socket_clients_reconnect_and_stalled_clients_are_bounded() {
    let root = root();
    let g = root.path().join("global");
    std::fs::write(&g, "timeout_ms = 100\nclients_max = 2").unwrap();
    let cfg = resolve_config(&g, &root.path().join("project")).unwrap();
    let engine = Arc::new(
        Engine::open(&root.path().join("db"), cfg.clone())
            .await
            .unwrap(),
    );
    let socket = root.path().join("engine.sock");
    let server = Server::bind(&socket, engine.clone(), &cfg).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(server.run(stopped));
    let mut stalled = crazyagent::spawn_gate::connect_unix(&socket).await.unwrap();
    stalled.write_all(&[0]).await.unwrap();
    let client = Client::new(socket.clone(), Duration::from_millis(1000)).unwrap();
    let first = client.request(request("snapshot")).await.unwrap();
    assert!(matches!(first.outcome, Outcome::Snapshot { .. }));
    assert_eq!(first, client.request(request("snapshot")).await.unwrap());
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), stalled.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!socket.exists());
    engine.close().await.unwrap();
}

#[tokio::test]
async fn endpoint_rejects_insecure_and_preexisting_paths() {
    let root = root();
    let cfg = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let engine = Arc::new(
        Engine::open(&root.path().join("db"), cfg.clone())
            .await
            .unwrap(),
    );
    let socket = root.path().join("engine.sock");
    std::fs::write(&socket, "untouched").unwrap();
    assert!(Server::bind(&socket, engine.clone(), &cfg).is_err());
    assert_eq!(std::fs::read_to_string(&socket).unwrap(), "untouched");
    let insecure = root.path().join("insecure");
    std::fs::create_dir(&insecure).unwrap();
    std::fs::set_permissions(&insecure, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Server::bind(&insecure.join("engine.sock"), engine.clone(), &cfg).is_err());
    let link = root.path().join("link");
    std::os::unix::fs::symlink(root.path(), &link).unwrap();
    assert!(Server::bind(&link.join("linked.sock"), engine.clone(), &cfg).is_err());
    engine.close().await.unwrap();
}

#[tokio::test]
async fn complete_request_commits_when_peer_disconnects_before_accept() {
    let root = root();
    let cfg = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let db_path = root.path().join("db");
    let engine = Arc::new(Engine::open(&db_path, cfg.clone()).await.unwrap());
    let path = root.path().join("engine.sock");
    let server = Server::bind(&path, engine.clone(), &cfg).unwrap();
    let mut operation = request("disconnected-before-accept");
    operation.command = Command::CreateProject {
        project_id: ProjectId::new("disconnected-project").unwrap(),
        spec: ProjectSpec {
            name: "Disconnected request".into(),
            repositories: vec![],
        },
    };
    let mut stream = crazyagent::spawn_gate::connect_unix(&path).await.unwrap();
    stream
        .write_all(&encode_frame(&operation).unwrap())
        .await
        .unwrap();
    drop(stream);
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(server.run(stopped));
    let db =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut committed = false;
    for _ in 0..100 {
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM operations WHERE id=?1",
                [operation.request_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        if count == 1 {
            committed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
    engine.close().await.unwrap();
    assert!(committed);
}

#[tokio::test]
async fn socket_capacity_and_malformed_frames_fail_without_success() {
    let root = root();
    let g = root.path().join("g");
    std::fs::write(&g, "clients_max = 1\ntimeout_ms = 500").unwrap();
    let cfg = resolve_config(&g, &root.path().join("p")).unwrap();
    let engine = Arc::new(
        Engine::open(&root.path().join("db"), cfg.clone())
            .await
            .unwrap(),
    );
    let path = root.path().join("engine.sock");
    let server = Server::bind(&path, engine.clone(), &cfg).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(server.run(stopped));
    let stalled = crazyagent::spawn_gate::connect_unix(&path).await.unwrap();
    tokio::time::sleep(Duration::from_millis(25)).await;
    let client = Client::new(path.clone(), Duration::from_millis(250)).unwrap();
    assert!(client.request(request("full")).await.is_err());
    drop(stalled);
    tokio::time::sleep(Duration::from_millis(25)).await;
    let mut version = request("bad-version");
    version.protocol_version = 2;
    let cases = vec![
        encode_frame(&version).unwrap(),
        vec![0, 1, 0, 1],
        vec![0, 0, 0, 1, b'{'],
        vec![0, 0, 0, 0],
    ];
    for frame in cases {
        let mut stream = crazyagent::spawn_gate::connect_unix(&path).await.unwrap();
        stream.write_all(&frame).await.unwrap();
        let mut byte = [0];
        let result = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut byte))
            .await
            .unwrap();
        assert!(matches!(result, Ok(0) | Err(_)));
        tokio::task::yield_now().await;
    }
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
    engine.close().await.unwrap();
}
