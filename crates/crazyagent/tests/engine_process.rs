#![cfg(unix)]

use crazyagent::{engine::Client, protocol::*};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Stdio};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}

fn launch(root: &std::path::Path, endpoint: &str) -> Process {
    let mut child = crazyagent::spawn_gate::spawn(
        std::process::Command::new(env!("CARGO_BIN_EXE_crazyagent"))
            .arg("engine")
            .arg(root)
            .arg(root.join("global.toml"))
            .arg(root.join("project.toml"))
            .arg(endpoint)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    let stdout = child.stdout.take().unwrap();
    let process = Process(child);
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = send.send(result);
    });
    let line = receive
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(line.trim(), "ready");
    process
}

#[tokio::test]
async fn disconnect_kill_restart_and_replay_executes_once() {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut process = launch(root.path(), "first.sock");
    let request = WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("desktop").unwrap(),
        request_id: RequestId::new("lost-ack").unwrap(),
        expected_revision: Cursor::new(0),
        command: Command::CreateProject {
            project_id: ProjectId::new("project").unwrap(),
            spec: ProjectSpec {
                name: "Real process".into(),
                repositories: vec![],
            },
        },
    };
    let mut stream = crazyagent::spawn_gate::connect_unix(root.path().join("first.sock"))
        .await
        .unwrap();
    stream
        .write_all(&encode_frame(&request).unwrap())
        .await
        .unwrap();
    drop(stream);
    let db = rusqlite::Connection::open_with_flags(
        root.path().join("engine.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut committed = false;
    for _ in 0..100 {
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM operations WHERE id='lost-ack'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if count == 1 {
            committed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(committed);
    let first_bytes: Vec<u8> = db
        .query_row(
            "SELECT reply FROM operations WHERE id='lost-ack'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let first: WireReply = serde_json::from_slice(&first_bytes).unwrap();
    drop(db);
    process.0.kill().unwrap();
    process.0.wait().unwrap();
    let _restarted = launch(root.path(), "restarted.sock");
    let client = Client::new(root.path().join("restarted.sock"), Duration::from_secs(2)).unwrap();
    let replay = client.request(request.clone()).await.unwrap();
    assert_eq!(first.outcome, replay.outcome);
    assert_eq!(first.revision, replay.revision);
    let mut snapshot = request;
    snapshot.request_id = RequestId::new("snapshot").unwrap();
    snapshot.command = Command::Snapshot;
    let reply = client.request(snapshot).await.unwrap();
    let Outcome::Snapshot { projects, .. } = reply.outcome else {
        panic!("snapshot required")
    };
    assert_eq!(projects.len(), 1);
}
