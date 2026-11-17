use super::usage_tests::hold_storage;
use super::*;
use crate::accounts::{AccountDraft, Provider, SignInMethod};
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{AccountSecret, OwnerClient, OwnerClientKind};
use crate::plugins::{Scope, ServerSpec};
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Busy {
    root: tempfile::TempDir,
    service: EngineService,
    _shutdown: tokio::sync::watch::Sender<bool>,
    client: OwnerClient,
    _disk: tokio::sync::MutexGuard<'static, ()>,
}

impl Busy {
    async fn start() -> Self {
        Self::with_settings("timeout_ms = 250\n").await
    }

    /// Starts the engine on owner-only settings files whose global file holds `settings`
    /// after its version line. Input: the settings text. Output: the running engine and
    /// its owner client.
    async fn with_settings(settings: &str) -> Self {
        let disk = super::usage_tests::busy_storage().await;
        let root = tempfile::Builder::new()
            .prefix("ca-busy-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, format!("version = 1\n{settings}")).unwrap();
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

    /// The engine's storage queue, which [`hold_storage`] holds. Output: a sender on it.
    fn storage(&self) -> tokio::sync::mpsc::Sender<crate::storage::Job> {
        let handle = self.service.owner_runtime().unwrap();
        handle.engine.storage.sender.clone()
    }

    /// Lets held storage go on, then waits until it has run everything queued behind the
    /// hold: storage answers its jobs one at a time in order, so one more job answers only
    /// after the request that outlasted the owner wait has committed, and the reads that
    /// follow do not race it. Input: the hold's sender.
    async fn settled(&self, release: oneshot::Sender<()>) {
        release.send(()).unwrap();
        let (send, barrier) = oneshot::channel();
        self.storage()
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

    async fn stop(mut self) {
        drop(self.client);
        tokio::time::timeout(Duration::from_secs(10), self.service.shutdown())
            .await
            .unwrap()
            .unwrap();
        drop(self.root);
    }
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
        "the request must outlast the owner wait behind the held storage"
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
    let release = hold_storage(&busy.storage());
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
    busy.settled(release).await;

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

    let release = hold_storage(&busy.storage());
    let replaced = busy
        .client
        .set_mcp_secret(
            server.id.clone(),
            "API_TOKEN".into(),
            AccountSecret::new(second.as_bytes().to_vec()).unwrap(),
        )
        .await;
    outlasted_the_owner_wait(replaced, "plugins_outcome_unknown");
    busy.settled(release).await;

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

    let release = hold_storage(&busy.storage());
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
        "the add must outlast the owner wait behind the held storage: {code}"
    );
    busy.settled(release).await;

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

    let release = hold_storage(&busy.storage());
    let removed = busy.client.remove_account(added.id.clone()).await;
    let code = removed.err().map(|error| error.0).unwrap_or_default();
    assert!(
        code.contains("outcome_unknown"),
        "the request must outlast the owner wait behind the held storage: {code}"
    );
    busy.settled(release).await;

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
    let release = hold_storage(&busy.storage());
    let removed = busy.client.remove_mcp_server(id.clone(), version).await;
    outlasted_the_owner_wait(removed, "plugins_outcome_unknown");
    busy.settled(release).await;

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

    let release = hold_storage(&busy.storage());
    let removed = busy
        .client
        .remove_mcp_secret(id.clone(), "API_TOKEN".into())
        .await;
    outlasted_the_owner_wait(removed, "plugins_outcome_unknown");
    busy.settled(release).await;

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
    let release = hold_storage(&busy.storage());
    let removed = busy.client.remove_mcp_server(id.clone(), version).await;
    outlasted_the_owner_wait(removed, "plugins_outcome_unknown");
    busy.settled(release).await;

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

    let release = hold_storage(&busy.storage());
    let replaced =
        super::accounts::key_replaced(&handle.engine, &admission, added.id.clone(), credential_ref)
            .await;
    outlasted_the_owner_wait(
        replaced.map(|_| ()),
        "native_auth_outcome_unknown_read_same_operation",
    );
    busy.settled(release).await;

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

const SHARED_KEY: &str = "lab-shared-key";

/// Settings that hold two connections of one account on one API key, `SHARED_KEY`, at a
/// local port that refuses, with an owner wait no removal outlasts. Input: the port.
/// Output: the settings text for [`Busy::with_settings`].
fn shared_key_settings(port: u16) -> String {
    let connection = |id: &str, model: &str| {
        format!(
            "  {{ id = \"{id}\", provider = \"openai-compatible\", harness = \"rcli\", protocol = \"openai-chat\", account_ref = \"lab-account\", endpoint = {{ kind = \"http\", url = \"http://127.0.0.1:{port}/v1\" }}, model = {{ id = \"{model}\", author = \"openai-compatible\" }}, auth = {{ method = \"api_key\", credential_ref = \"{SHARED_KEY}\" }} }},\n"
        )
    };
    format!(
        "timeout_ms = 10000\n\n[model_access]\naccounts = [\"lab-account\"]\nconnections = [\n{}{}]\n",
        connection("lab-first", "lab-model"),
        connection("lab-second", "lab-model-2"),
    )
}

/// Writes `secret` as the key file `name` in the engine's owner-only key folder, where an
/// account added with that key keeps it. Inputs: the engine root, the key name and the
/// secret. Output: the key file's path.
fn planted_key(root: &Path, name: &str, secret: &str) -> PathBuf {
    use std::os::unix::fs::DirBuilderExt;
    let folder = root.join(crate::storage::accounts::ACCOUNT_KEYS);
    if !folder.exists() {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&folder)
            .unwrap();
    }
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    let key = folder.join(name);
    std::fs::write(&key, secret.as_bytes()).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(holders(root, secret.as_bytes()), 1);
    key
}

/// ACCOUNT-REMOVE-KEY: removing one of two connections on one API key keeps the key file
/// the other still uses, as removing a native sign-in keeps a profile another connection
/// names, and removing the last of them deletes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_one_of_two_connections_on_one_key_keeps_the_key_until_the_last_goes() {
    let (_refusing, port) = crate::spawn_gate::refusing_tcp_port().unwrap();
    let busy = Busy::with_settings(&shared_key_settings(port)).await;
    let secret = format!("sk-shared-{}", std::process::id());
    let key = planted_key(&busy.engine_root(), SHARED_KEY, &secret);
    let first = crate::protocol::CredentialRef::new("lab-first").unwrap();
    let second = crate::protocol::CredentialRef::new("lab-second").unwrap();

    busy.client.remove_account(first).await.unwrap();
    assert_eq!(
        std::fs::read(&key).ok().as_deref(),
        Some(secret.as_bytes()),
        "the key the other connection still uses stays"
    );
    let left = busy.client.list_accounts(None, 10).await.unwrap().accounts;
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0].id, second);

    busy.client.remove_account(second).await.unwrap();
    assert!(
        none_hold(&busy.engine_root(), secret.as_bytes()).await,
        "the key goes with the last connection that used it"
    );
    busy.stop().await;
}

/// ACCOUNT-REMOVE-KEY: a key replaced for a connection that was removed meanwhile stays on
/// disk while another connection on the same key still uses it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_replaced_after_its_connection_was_removed_stays_for_the_other_on_that_key() {
    let (_refusing, port) = crate::spawn_gate::refusing_tcp_port().unwrap();
    let busy = Busy::with_settings(&shared_key_settings(port)).await;
    let before = format!("sk-shared-before-{}", std::process::id());
    planted_key(&busy.engine_root(), SHARED_KEY, &before);
    let first = crate::protocol::CredentialRef::new("lab-first").unwrap();
    let shared = crate::protocol::CredentialRef::new(SHARED_KEY).unwrap();
    let handle = busy.service.owner_runtime().unwrap();
    let admission = busy.admission();

    busy.client.remove_account(first.clone()).await.unwrap();
    let after = format!("sk-shared-after-{}", std::process::id());
    super::accounts::replace_key(
        &handle.authority.root,
        &shared,
        AccountSecret::new(after.as_bytes().to_vec()).unwrap(),
    )
    .unwrap();
    let replaced = super::accounts::key_replaced(&handle.engine, &admission, first, shared).await;
    assert_eq!(
        replaced.err().map(|error| error.0).as_deref(),
        Some("account_not_found")
    );
    assert_eq!(
        holders(&busy.engine_root(), after.as_bytes()),
        1,
        "the replaced key the other connection still uses stays"
    );
    busy.stop().await;
}

/// Reads one model-access page or record from the owner. Inputs: the engine, a request
/// label and the read. Output: the state the engine reads back.
async fn owner_read(busy: &Busy, label: &str, query: OwnerRead) -> OwnerReadReply {
    let read = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(format!("read-{label}")).unwrap(),
        expected_revision: Cursor::new(0),
        operation: OwnerOperation::Read { query },
    };
    let reply = busy.client.request(read).await.unwrap();
    let OwnerResponse::Read { state } = reply.response else {
        panic!("the owner read answers: {reply:?}");
    };
    state
}

/// Applies one settings edit through the owner's ModelAccess mutation, as the model-access
/// screen does, on the basis the engine reads back, and asserts it applies. Inputs: the
/// engine, a request label and the edit.
async fn owner_edit(busy: &Busy, label: &str, edit: crate::config::ModelAccessEdit) {
    let state = owner_read(
        busy,
        label,
        OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Connections,
            cursor: None,
            limit: 16,
        },
    )
    .await;
    let OwnerData::ModelAccessPage { source_hash, .. } = state.data else {
        panic!("the read answers with the model access page");
    };
    let change = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(format!("edit-{label}")).unwrap(),
        expected_revision: state.revision,
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::ModelAccess {
                expected_config_id: state.config_id,
                expected_source_hash: source_hash,
                edit,
            },
        },
    };
    let reply = busy.client.request(change).await.unwrap();
    let OwnerResponse::Applied { operation } = &reply.response else {
        panic!("the edit is applied: {reply:?}");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
}

/// Removes one connection from the settings, as the model-access screen does.
/// Inputs: the engine and the connection id.
async fn remove_connection(busy: &Busy, id: &str) {
    owner_edit(
        busy,
        &format!("remove-{id}"),
        crate::config::ModelAccessEdit::RemoveConnection {
            id: CredentialRef::new(id).unwrap(),
        },
    )
    .await;
}

/// ACCOUNT-REMOVE-KEY rv-secreview F1: an API key whose connections leave the settings
/// through the model-access edit stays while one still names it, and goes with its
/// generated home once the last one leaves, so no secret stays on disk with nothing naming
/// it and no note to sweep.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_whose_last_connection_leaves_the_settings_goes_with_its_generated_home() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::DirBuilderExt;
    let (_refusing, port) = crate::spawn_gate::refusing_tcp_port().unwrap();
    let busy = Busy::with_settings(&shared_key_settings(port)).await;
    let secret = format!("sk-shared-edit-{}", std::process::id());
    let key = planted_key(&busy.engine_root(), SHARED_KEY, &secret);
    let home = busy
        .engine_root()
        .join("configured-sdk-homes")
        .join(format!("{:x}", Sha256::digest(SHARED_KEY.as_bytes())));
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

    remove_connection(&busy, "lab-first").await;
    assert_eq!(
        std::fs::read(&key).ok().as_deref(),
        Some(secret.as_bytes()),
        "the key the other connection still names stays"
    );
    assert_eq!(holders(&busy.engine_root(), secret.as_bytes()), 2);

    remove_connection(&busy, "lab-second").await;
    assert!(
        none_hold(&busy.engine_root(), secret.as_bytes()).await,
        "the key goes once the last connection naming it leaves the settings"
    );
    assert!(!home.exists(), "its generated home goes whole");
    busy.stop().await;
}

/// Reads one connection of the settings catalog. Inputs: the engine and the connection id.
/// Output: the connection as the settings hold it.
async fn catalog_connection(busy: &Busy, id: &CredentialRef) -> crate::config::ModelConnection {
    let state = owner_read(
        busy,
        &format!("connection-{}", id.as_str()),
        OwnerRead::ModelAccessRecord {
            collection: ModelAccessCollection::Connections,
            id: id.clone(),
        },
    )
    .await;
    let OwnerData::ModelAccessRecord {
        record: ModelAccessRecord::Connection { connection },
        ..
    } = state.data
    else {
        panic!("the settings hold the connection {}", id.as_str());
    };
    connection
}

/// ACCOUNT-RECORD-LEG (rv-sec3 r2 F1): a saved API key account whose catalog connection was
/// moved to another key reference, then removed, leaves neither its key file nor its
/// generated home on disk, though the catalog no longer named that key at the removal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_saved_account_moved_to_another_key_and_removed_leaves_its_first_key_nowhere() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::DirBuilderExt;
    let busy = Busy::with_settings("timeout_ms = 10000\n").await;
    let listener = crate::spawn_gate::bind_tcp("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let secret = format!("sk-record-leg-{}", std::process::id());
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
    owner_edit(
        &busy,
        "pool",
        crate::config::ModelAccessEdit::CreatePool {
            pool: crate::config::AccountPool {
                id: CredentialRef::new("lab-pool").unwrap(),
                name: None,
                members: vec![added.id.clone()],
                projects: vec![],
                roles: vec![],
                fallback_allowed: false,
                provider_quota_policy: Default::default(),
            },
        },
    )
    .await;
    let mut moved = catalog_connection(&busy, &added.id).await;
    let crate::config::ModelAuthentication::ApiKey { credential_ref } = &mut moved.auth else {
        panic!("the pooled account keeps its API key");
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
    *credential_ref = CredentialRef::new("lab-moved-key").unwrap();
    owner_edit(
        &busy,
        "move",
        crate::config::ModelAccessEdit::ReplaceConnection { connection: moved },
    )
    .await;
    assert_eq!(
        holders(&busy.engine_root(), secret.as_bytes()),
        2,
        "the saved account still names its first key"
    );

    busy.client.remove_account(added.id.clone()).await.unwrap();
    assert!(
        none_hold(&busy.engine_root(), secret.as_bytes()).await,
        "the first key of the removed saved account goes with its generated home"
    );
    assert!(!home.exists(), "the generated home goes whole");
    busy.stop().await;
}

/// ACCOUNT-NATIVE-SIGN-IN-NOTE (rv-ds241 N1): a native connection that leaves the settings
/// with no saved account naming its profile loses its sign-in files, and the rest of its
/// profile slot stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_native_connection_that_leaves_the_settings_loses_its_sign_in_files() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::DirBuilderExt;
    let profile = "lab-native-profile";
    let busy = Busy::with_settings(&format!(
        "timeout_ms = 10000\n\n[model_access]\naccounts = [\"lab-native-account\"]\nconnections = [\n  {{ id = \"lab-native\", provider = \"provider-chatgpt\", harness = \"codex\", protocol = \"native\", account_ref = \"lab-native-account\", endpoint = {{ kind = \"native\" }}, model = {{ id = \"model-chatgpt\", author = \"author-chatgpt\" }}, auth = {{ method = \"native_managed\", profile_ref = \"{profile}\" }} }},\n]\n"
    ))
    .await;
    let secret = format!("native-sign-in-{}", std::process::id());
    let slot = busy
        .engine_root()
        .join("native-profiles")
        .join(format!("{:x}", Sha256::digest(profile.as_bytes())));
    let home = slot.join("home");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.join("Library/Keychains"))
        .unwrap();
    let kept = [slot.join("identity.json"), home.join("keep.txt")];
    let signed = [
        home.join("auth.json"),
        home.join(".credentials.json"),
        home.join("Library/Keychains/login.keychain-db"),
    ];
    for (file, bytes) in kept
        .iter()
        .map(|file| (file, b"kept".as_slice()))
        .chain(signed.iter().map(|file| (file, secret.as_bytes())))
    {
        std::fs::write(file, bytes).unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(holders(&busy.engine_root(), secret.as_bytes()), 3);

    remove_connection(&busy, "lab-native").await;
    assert!(
        none_hold(&busy.engine_root(), secret.as_bytes()).await,
        "the sign-in files of a profile nothing names go"
    );
    for file in &kept {
        assert!(file.exists(), "{}", file.display());
    }
    busy.stop().await;
}
