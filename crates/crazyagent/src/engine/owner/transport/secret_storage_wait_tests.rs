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
    _disk: tokio::sync::MutexGuard<'static, ()>,
}

impl Busy {
    async fn start() -> Self {
        let disk = super::usage_tests::busy_storage().await;
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
            _disk: disk,
        }
    }

    fn engine_root(&self) -> PathBuf {
        let handle = self.service.owner_runtime().unwrap();
        handle.authority.root.path.parent().unwrap().to_path_buf()
    }

    fn admission(&self) -> crate::engine::owner::Admission {
        let mut packet = [0; 37];
        packet[..4].copy_from_slice(b"CAO1");
        packet[5..].copy_from_slice(&self.client.token);
        self.service
            .owner_runtime()
            .unwrap()
            .authority
            .authenticate(&packet)
            .unwrap()
    }

    /// Queues a real storage sweep of `SWEPT_FILES` files ahead of every later storage
    /// job, in a removed skill folder of the private plugin store made as the engine makes
    /// it. Output: the swept folder and the sweep.
    fn occupy_storage(&self) -> (PathBuf, Sweep) {
        let swept =
            crate::storage::plugins::store_folder(&self.engine_root(), &["skills", "global"])
                .unwrap()
                .join(".removed-busy-storage");
        std::fs::create_dir(&swept).unwrap();
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
            Vec::new(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_added_with_secrets_while_storage_is_busy_is_kept_with_every_one_of_them() {
    let busy = Busy::start().await;
    let controller = crate::client_controller::OwnerController::attach(
        busy.root.path(),
        OwnerClientKind::Tui,
        Duration::from_secs(20),
    )
    .await
    .unwrap();
    let first = format!("first-added-secret-{}", std::process::id());
    let second = format!("second-added-secret-{}", std::process::id());

    let (swept, sweep) = busy.occupy_storage();
    let added = controller
        .add_mcp_server(
            Scope::Global,
            "busy-added".into(),
            ServerSpec::Stdio {
                command: "/usr/bin/true".into(),
                args: Vec::new(),
            },
            vec![
                (
                    "API_TOKEN".into(),
                    AccountSecret::new(first.as_bytes().to_vec()).unwrap(),
                ),
                (
                    "API_KEY".into(),
                    AccountSecret::new(second.as_bytes().to_vec()).unwrap(),
                ),
            ],
        )
        .await;
    let code = added.err().map(|error| error.0).unwrap_or_default();
    assert!(
        code.ends_with("outcome_unknown") || code.contains("outcome_unknown_"),
        "the add must outlast the owner wait behind the real sweep: {code}"
    );
    settled(&swept, sweep).await;

    let servers = controller.plugin_servers(None).await.unwrap();
    assert_eq!(servers.len(), 1, "storage committed the add");
    let secrets: Vec<(&str, bool)> = servers[0]
        .secrets
        .iter()
        .map(|secret| (secret.name.as_str(), secret.saved))
        .collect();
    assert_eq!(
        secrets,
        [("API_TOKEN", true), ("API_KEY", true)],
        "a server is never left with only part of the secrets it was added with"
    );
    for secret in [&first, &second] {
        assert_eq!(holders(&busy.engine_root(), secret.as_bytes()), 1);
    }
    drop(controller);
    busy.stop().await;
}

/// Waits up to ten seconds until no file under the engine root holds the secret: once
/// storage answers late, the engine deletes it in a task of its own after the owner's
/// wait. Inputs: the root and the secret. Output: true once no file holds it.
async fn none_hold(root: &Path, secret: &[u8]) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while holders(root, secret) > 0 {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    true
}

/// Finds the one file under the engine root that holds the secret.
/// Inputs: the root and the secret. Output: its path.
fn holding(root: &Path, secret: &[u8]) -> PathBuf {
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                folders.push(path);
            } else if metadata.is_file()
                && std::fs::read(&path)
                    .is_ok_and(|bytes| bytes.windows(secret.len()).any(|window| window == secret))
            {
                found.push(path);
            }
        }
    }
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0)
}

async fn server_with_secrets(busy: &Busy, name: &str, secrets: &[(&str, &str)]) -> String {
    let server = busy
        .client
        .add_mcp_server(
            Scope::Global,
            name.into(),
            ServerSpec::Stdio {
                command: "/usr/bin/true".into(),
                args: Vec::new(),
            },
            Vec::new(),
        )
        .await
        .unwrap();
    for (secret_name, value) in secrets {
        busy.client
            .set_mcp_secret(
                server.id.clone(),
                (*secret_name).into(),
                AccountSecret::new(value.as_bytes().to_vec()).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(holders(&busy.engine_root(), value.as_bytes()), 1);
    }
    server.id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_removed_while_storage_is_busy_leaves_no_key_on_disk() {
    let busy = Busy::start().await;
    let listener = crate::spawn_gate::bind_tcp("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let secret = format!("sk-removed-{}", std::process::id());
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
        .await
        .unwrap();
    assert_eq!(holders(&busy.engine_root(), secret.as_bytes()), 1);

    let (swept, sweep) = busy.occupy_storage();
    let removed = busy.client.remove_account(added.id.clone()).await;
    let code = removed.err().map(|error| error.0).unwrap_or_default();
    assert!(
        code.contains("outcome_unknown"),
        "the request must outlast the owner wait behind the real sweep: {code}"
    );
    settled(&swept, sweep).await;

    assert!(
        busy.client
            .list_accounts(None, 10)
            .await
            .unwrap()
            .accounts
            .is_empty(),
        "storage committed the removal after the wait"
    );
    assert!(
        none_hold(&busy.engine_root(), secret.as_bytes()).await,
        "the key of a removed account must not stay on disk"
    );
    busy.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_removed_while_storage_is_busy_leaves_none_of_its_secrets_on_disk() {
    let busy = Busy::start().await;
    let first = format!("removed-first-{}", std::process::id());
    let second = format!("removed-second-{}", std::process::id());
    let id = server_with_secrets(
        &busy,
        "busy-removed",
        &[
            ("API_TOKEN", first.as_str()),
            ("OTHER_TOKEN", second.as_str()),
        ],
    )
    .await;

    let version = busy.client.plugin_server(id.clone()).await.unwrap().version;
    let (swept, sweep) = busy.occupy_storage();
    let removed = busy.client.remove_mcp_server(id.clone(), version).await;
    outlasted_the_owner_wait(removed, "plugins_outcome_unknown");
    settled(&swept, sweep).await;

    assert!(
        busy.client.plugin_server(id).await.is_err(),
        "storage committed the removal after the wait"
    );
    for value in [&first, &second] {
        assert!(
            none_hold(&busy.engine_root(), value.as_bytes()).await,
            "a secret of a removed server must not stay on disk"
        );
    }
    busy.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_secret_removed_while_storage_is_busy_leaves_no_file_on_disk() {
    let busy = Busy::start().await;
    let value = format!("detached-busy-secret-{}", std::process::id());
    let id = server_with_secrets(&busy, "busy-detached", &[("API_TOKEN", value.as_str())]).await;

    let (swept, sweep) = busy.occupy_storage();
    let removed = busy
        .client
        .remove_mcp_secret(id.clone(), "API_TOKEN".into())
        .await;
    outlasted_the_owner_wait(removed, "plugins_outcome_unknown");
    settled(&swept, sweep).await;

    assert!(
        busy.client
            .plugin_server(id)
            .await
            .unwrap()
            .secrets
            .is_empty(),
        "storage committed the removal after the wait"
    );
    assert!(
        none_hold(&busy.engine_root(), value.as_bytes()).await,
        "a removed secret must not stay on disk"
    );
    busy.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_secret_file_that_cannot_be_deleted_keeps_none_of_the_others_of_a_removed_server() {
    let busy = Busy::start().await;
    let first = format!("stuck-first-{}", std::process::id());
    let second = format!("stuck-second-{}", std::process::id());
    let id = server_with_secrets(
        &busy,
        "stuck-removed",
        &[
            ("API_TOKEN", first.as_str()),
            ("OTHER_TOKEN", second.as_str()),
        ],
    )
    .await;
    let stuck = holding(&busy.engine_root(), first.as_bytes());
    std::fs::remove_file(&stuck).unwrap();
    std::fs::create_dir(&stuck).unwrap();

    let version = busy.client.plugin_server(id.clone()).await.unwrap().version;
    assert!(
        busy.client
            .remove_mcp_server(id.clone(), version)
            .await
            .is_err(),
        "the owner hears that a secret file could not be deleted"
    );
    assert!(busy.client.plugin_server(id).await.is_err());
    assert_eq!(
        holders(&busy.engine_root(), second.as_bytes()),
        0,
        "one file that cannot be deleted must not keep the server's other secrets"
    );
    busy.stop().await;
}

/// Waits up to ten seconds until the path is gone: a late removal runs on a blocking
/// thread of its own after the owner's wait. Input: the path. Output: true once gone.
async fn gone(path: &Path) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while std::fs::symlink_metadata(path).is_ok() {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    true
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_checked_server_removed_while_storage_is_busy_leaves_no_check_folder() {
    let busy = Busy::start().await;
    let value = format!("checked-busy-secret-{}", std::process::id());
    let id = server_with_secrets(&busy, "busy-checked", &[("API_TOKEN", value.as_str())]).await;
    let checked = busy
        .engine_root()
        .join(super::plugins::CHECK_FOLDER)
        .join(&id);
    let home = checked.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let cached = format!("cached-by-the-checked-server-{}", std::process::id());
    std::fs::write(home.join("token-cache"), &cached).unwrap();
    let profiles = busy
        .engine_root()
        .join(super::plugins::CHECK_FOLDER)
        .join(super::plugins::CHECK_PROFILES);
    std::fs::create_dir_all(&profiles).unwrap();
    let profile = profiles.join(format!("{id}.sb"));
    std::fs::write(&profile, "(version 1)\n").unwrap();

    let version = busy.client.plugin_server(id.clone()).await.unwrap().version;
    let (swept, sweep) = busy.occupy_storage();
    let removed = busy.client.remove_mcp_server(id.clone(), version).await;
    outlasted_the_owner_wait(removed, "plugins_outcome_unknown");
    settled(&swept, sweep).await;

    assert!(
        busy.client.plugin_server(id).await.is_err(),
        "storage committed the removal after the wait"
    );
    assert!(
        gone(&checked).await,
        "what a check wrote while it held the server's secrets must not stay on disk"
    );
    assert!(
        none_hold(&busy.engine_root(), cached.as_bytes()).await,
        "what the check wrote must be deleted after its folder moved aside"
    );
    assert!(
        gone(&profile).await,
        "the removed server's check profile must not stay behind"
    );
    assert!(none_hold(&busy.engine_root(), value.as_bytes()).await);
    busy.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_replaced_after_its_account_was_removed_leaves_no_key_on_disk() {
    let busy = Busy::start().await;
    let listener = crate::spawn_gate::bind_tcp("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let first = format!("sk-before-replace-{}", std::process::id());
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
            Some(AccountSecret::new(first.as_bytes().to_vec()).unwrap()),
        )
        .await
        .unwrap();
    let handle = busy.service.owner_runtime().unwrap();
    let admission = busy.admission();
    let Ok(crate::storage::accounts::ResultValue::Account(listed)) = handle
        .engine
        .native_auth_job(|reply| {
            crate::storage::Job::Accounts(
                admission.clone(),
                crate::storage::accounts::Command::Get(added.id.clone()),
                reply,
            )
        })
        .await
    else {
        panic!("storage lists the added account");
    };
    let crate::config::ModelAuthentication::ApiKey { credential_ref } =
        listed.connection.auth.clone()
    else {
        panic!("the added account keeps an API key");
    };

    busy.client.remove_account(added.id.clone()).await.unwrap();
    assert!(none_hold(&busy.engine_root(), first.as_bytes()).await);
    let second = format!("sk-after-replace-{}", std::process::id());
    super::accounts::replace_key(
        &handle.authority.root,
        &credential_ref,
        AccountSecret::new(second.as_bytes().to_vec()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        holders(&busy.engine_root(), second.as_bytes()),
        1,
        "a replace that read the account before the Remove recreates its key file"
    );

    let (swept, sweep) = busy.occupy_storage();
    let replaced = super::accounts::key_replaced(
        &handle.engine,
        &admission,
        &handle.authority,
        added.id.clone(),
        credential_ref,
    )
    .await;
    outlasted_the_owner_wait(
        replaced.map(|_| ()),
        "native_auth_outcome_unknown_read_same_operation",
    );
    settled(&swept, sweep).await;

    assert!(
        none_hold(&busy.engine_root(), second.as_bytes()).await,
        "a key replaced for an account storage removed must not stay on disk"
    );
    busy.stop().await;
}

/// SECAUDIT configured-rcli-home-keeps-provider-key: removing an API key account leaves its
/// key in no file under the engine root, the generated SDK home included.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_an_account_leaves_no_key_in_its_generated_home() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::DirBuilderExt;
    let busy = Busy::start().await;
    let listener = crate::spawn_gate::bind_tcp("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let secret = format!("dummy-key-{}", std::process::id());
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
        .await
        .unwrap();
    let handle = busy.service.owner_runtime().unwrap();
    let admission = busy.admission();
    let Ok(crate::storage::accounts::ResultValue::Account(listed)) = handle
        .engine
        .native_auth_job(|reply| {
            crate::storage::Job::Accounts(
                admission.clone(),
                crate::storage::accounts::Command::Get(added.id.clone()),
                reply,
            )
        })
        .await
    else {
        panic!("storage lists the added account");
    };
    let crate::config::ModelAuthentication::ApiKey { credential_ref } =
        listed.connection.auth.clone()
    else {
        panic!("the added account keeps an API key");
    };
    let home = busy
        .engine_root()
        .join("configured-sdk-homes")
        .join(format!(
            "{:x}",
            Sha256::digest(credential_ref.as_str().as_bytes())
        ));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.join(".backboard"))
        .unwrap();
    std::fs::write(home.join(".backboard/keys.json"), secret.as_bytes()).unwrap();
    std::fs::set_permissions(
        home.join(".backboard/keys.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(holders(&busy.engine_root(), secret.as_bytes()), 2);

    busy.client.remove_account(added.id.clone()).await.unwrap();
    assert!(
        none_hold(&busy.engine_root(), secret.as_bytes()).await,
        "the generated home keeps the removed account's key"
    );
    assert!(!home.exists(), "the generated home goes whole");
    busy.stop().await;
}
