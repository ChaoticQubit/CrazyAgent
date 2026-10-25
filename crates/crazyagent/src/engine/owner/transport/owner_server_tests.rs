use super::*;
use crate::engine::bootstrap::EngineService;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

const OWNER_WAIT_MS: u64 = 5000;

struct Fixture {
    root: tempfile::TempDir,
    service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
}

impl Fixture {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-own-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(
            &global,
            format!("version = 1\ntimeout_ms = {OWNER_WAIT_MS}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        Self {
            root,
            service,
            _stop: stop,
        }
    }

    async fn client(&self) -> OwnerClient {
        OwnerClient::discover(
            self.root.path(),
            OwnerClientKind::Tui,
            Duration::from_secs(20),
        )
        .await
        .unwrap()
        .0
    }

    async fn stop(mut self) {
        tokio::time::timeout(Duration::from_secs(10), self.service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

async fn connected(client: &OwnerClient) -> UnixStream {
    crate::spawn_gate::connect_unix(client.root.path.join(SOCKET))
        .await
        .unwrap()
}

async fn admitted(client: &OwnerClient) -> UnixStream {
    let mut stream = connected(client).await;
    let mut packet = [0; 37];
    packet[..4].copy_from_slice(b"CAO1");
    packet[5..].copy_from_slice(&client.token);
    stream.write_all(&packet).await.unwrap();
    stream
}

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

async fn served_again(client: &OwnerClient, id: &str) {
    for _ in 0..50 {
        if let Ok(reply) = client.request(read_capabilities(id)).await
            && matches!(reply.response, OwnerResponse::Read { .. })
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the owner server did not serve again once the held clients left");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_third_owner_client_is_refused_plainly_while_two_hold_the_server() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let before = client
        .request(read_capabilities("before-busy"))
        .await
        .unwrap();
    assert!(
        matches!(before.response, OwnerResponse::Read { .. }),
        "{before:?}"
    );

    let first = admitted(&client).await;
    let second = admitted(&client).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let refused = client
        .request(read_capabilities("while-busy"))
        .await
        .unwrap_or_else(|error| panic!("the third owner request answered {}", error.0));
    assert_eq!(refused.request_id, RequestId::new("while-busy").unwrap());
    assert_eq!(
        refused.response,
        OwnerResponse::Rejected {
            code: "owner_busy".into()
        }
    );
    assert_eq!(client.github_status().await.unwrap_err().0, "owner_busy");
    assert_eq!(
        client.list_accounts(None, 10).await.unwrap_err().0,
        "owner_busy"
    );

    drop(first);
    drop(second);
    served_again(&client, "after-busy").await;
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_client_past_the_handshake_queue_is_refused_plainly() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let mut silent = Vec::new();
    for _ in 0..3 {
        silent.push(connected(&client).await);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let refused = client
        .request(read_capabilities("queue-full"))
        .await
        .unwrap_or_else(|error| panic!("the queued owner request answered {}", error.0));
    assert_eq!(
        refused.response,
        OwnerResponse::Rejected {
            code: "owner_busy".into()
        }
    );

    drop(silent);
    served_again(&client, "after-queue").await;
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_owner_request_is_answered_and_the_server_keeps_serving() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let mut stream = admitted(&client).await;
    stream
        .write_all(&encode_frame(&json!({ "test_panic": {} })).unwrap())
        .await
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(10), read_frame(&mut stream))
        .await
        .unwrap()
        .unwrap_or_else(|error| panic!("the panicking request answered {}", error.0));
    let reply: serde_json::Value = serde_json::from_slice(&frame[4..]).unwrap();
    assert_eq!(
        reply,
        json!({ "kind": "rejected", "code": "owner_request_failed" })
    );

    let after = client
        .request(read_capabilities("after-panic"))
        .await
        .unwrap_or_else(|error| panic!("after the panic the owner answered {}", error.0));
    assert!(
        matches!(after.response, OwnerResponse::Read { .. }),
        "{after:?}"
    );
    assert!(!client.github_status().await.unwrap().signed_in);
    fixture.stop().await;
}
