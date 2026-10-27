use super::*;
use crate::accounts::{AccountDraft, Provider, SignInMethod};
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{AccountSecret, OwnerClient, OwnerClientKind};
use crate::plugins::{Scope, ServerSpec};
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SWEPT_FILES: usize = 40_000;

struct Busy {
    root: tempfile::TempDir,
    service: EngineService,
    _shutdown: tokio::sync::watch::Sender<bool>,
    client: OwnerClient,
}

impl Busy {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-busy-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, "version = 1\ntimeout_ms = 250\n").unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (shutdown, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let client =
            OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(20))
                .await
                .unwrap()
                .0;
        Self {
            root,
            service,
            _shutdown: shutdown,
            client,
        }
    }

    fn engine_root(&self) -> PathBuf {
        let handle = self.service.owner_runtime().unwrap();
        handle.authority.root.path.parent().unwrap().to_path_buf()
    }

    fn occupy_storage(&self) -> (PathBuf, Sweep) {
        let swept = self
            .engine_root()
            .join(crate::storage::plugins::STORE)
            .join("skills")
            .join("global")
            .join(".removed-busy-storage");
        std::fs::create_dir_all(&swept).unwrap();
        for index in 0..SWEPT_FILES {
            std::fs::write(swept.join(index.to_string()), b"x").unwrap();
        }
        let storage = self
            .service
            .owner_runtime()
            .unwrap()
            .engine
            .storage
            .sender
            .clone();
        let (send, answer) = oneshot::channel();
        storage
            .try_send(crate::storage::Job::Setup(
                crate::storage::setup::Command::Sweep,
                send,
            ))
            .unwrap();
        (swept, Sweep { answer, storage })
    }

    async fn stop(mut self) {
        drop(self.client);
        tokio::time::timeout(Duration::from_secs(10), self.service.shutdown())
            .await
            .unwrap()
            .unwrap();
        drop(self.root);
    }
}

/// The real sweep that holds storage, and the storage queue it sits in. Fields: the
/// sweep's answer, and the storage sender [`settled`] queues its barrier on.
struct Sweep {
    answer: oneshot::Receiver<Result<(), EngineError>>,
    storage: tokio::sync::mpsc::Sender<crate::storage::Job>,
}

/// Waits until storage has run everything queued behind the real sweep: the sweep's own
/// answer, then the answer to one more sweep queued after it. Storage answers its jobs one
/// at a time in order, so the request that outlasted the owner wait has run by then and
/// the reads that follow do not race its commit. Inputs: the swept folder and the sweep.
async fn settled(swept: &Path, sweep: Sweep) {
    tokio::time::timeout(Duration::from_secs(60), sweep.answer)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!swept.exists());
    let (send, barrier) = oneshot::channel();
    sweep
        .storage
        .send(crate::storage::Job::Setup(
            crate::storage::setup::Command::Sweep,
            send,
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(60), barrier)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

fn holders(root: &Path, secret: &[u8]) -> usize {
    let mut found = 0;
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        if metadata.is_dir() {
            found += holders(&path, secret);
        } else if metadata.is_file()
            && std::fs::read(&path)
                .is_ok_and(|bytes| bytes.windows(secret.len()).any(|window| window == secret))
        {
            found += 1;
        }
    }
    found
}

async fn serve_once(listener: &tokio::net::TcpListener) -> String {
    let (mut stream, request) = loop {
        let (mut stream, _) = crate::spawn_gate::accept_tcp(listener).await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let size = stream.read(&mut buffer).await.unwrap();
            if size == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..size]);
        }
        if !request.is_empty() {
            break (stream, request);
        }
    };
    let body = r#"{"data":[{"id":"lab-model"}]}"#;
    let reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(reply.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
    String::from_utf8(request).unwrap()
}

fn outlasted_the_owner_wait(result: Result<impl std::fmt::Debug, EngineError>, code: &str) {
    assert_eq!(
        result.err().map(|error| error.0).as_deref(),
        Some(code),
        "the request must outlast the owner wait behind the real sweep"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_added_while_storage_is_busy_keeps_the_key_its_record_refers_to() {
    let busy = Busy::start().await;
    let listener = crate::spawn_gate::bind_tcp("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let secret = format!("sk-busy-{}", std::process::id());
    let (swept, sweep) = busy.occupy_storage();
    let added = busy
        .client
        .add_account(
            AccountDraft {
                provider: Provider::OpenaiCompatible,
                method: SignInMethod::Url,
                name: "Lab server".into(),
                url: Some(url),
                model: Some("lab-model".into()),
            },
            Some(AccountSecret::new(secret.as_bytes().to_vec()).unwrap()),
        )
        .await;
    outlasted_the_owner_wait(added, "native_auth_outcome_unknown_read_same_operation");
    settled(&swept, sweep).await;

    let accounts = busy.client.list_accounts(None, 10).await.unwrap().accounts;
    assert_eq!(
        accounts.len(),
        1,
        "storage committed the add after the wait"
    );
    assert_eq!(
        holders(&busy.engine_root(), secret.as_bytes()),
        1,
        "the key the committed account refers to must stay on disk"
    );
    assert_eq!(accounts[0].status_label, "Key saved");
    let (request, checked) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(10), serve_once(&listener)),
        busy.client.check_account(accounts[0].id.clone()),
    );
    let request = request.expect("the check read the saved key and reached the server");
    assert!(
        request.contains(&format!("\r\nAuthorization: Bearer {secret}\r\n")),
        "the saved key must reach the request"
    );
    assert_eq!(checked.unwrap().account.status_label, "Key saved");
    busy.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_secret_replaced_while_storage_is_busy_keeps_the_new_one_and_drops_the_old() {
    let busy = Busy::start().await;
    let server = busy
        .client
        .add_mcp_server(
            Scope::Global,
            "busy-server".into(),
            ServerSpec::Stdio {
                command: "/usr/bin/true".into(),
                args: Vec::new(),
            },
        )
        .await
        .unwrap();
    let first = format!("first-busy-secret-{}", std::process::id());
    let second = format!("second-busy-secret-{}", std::process::id());
    busy.client
        .set_mcp_secret(
            server.id.clone(),
            "API_TOKEN".into(),
            AccountSecret::new(first.as_bytes().to_vec()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(holders(&busy.engine_root(), first.as_bytes()), 1);

    let (swept, sweep) = busy.occupy_storage();
    let replaced = busy
        .client
        .set_mcp_secret(
            server.id.clone(),
            "API_TOKEN".into(),
            AccountSecret::new(second.as_bytes().to_vec()).unwrap(),
        )
        .await;
    outlasted_the_owner_wait(replaced, "plugins_outcome_unknown");
    settled(&swept, sweep).await;

    let after = busy.client.plugin_server(server.id.clone()).await.unwrap();
    assert_eq!(after.secrets.len(), 1, "storage committed the new secret");
    assert_eq!(
        holders(&busy.engine_root(), second.as_bytes()),
        1,
        "the secret the record refers to must stay on disk"
    );
    assert_eq!(
        holders(&busy.engine_root(), first.as_bytes()),
        0,
        "the replaced secret must not stay behind"
    );
    busy.stop().await;
}
