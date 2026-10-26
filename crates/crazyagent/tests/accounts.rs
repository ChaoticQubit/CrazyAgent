#![cfg(unix)]

use crazyagent::accounts::{AccountDraft, AccountStatus, AccountView, Provider, SignInMethod};
use crazyagent::config::{AccountPool, ModelAccessEdit, ProviderQuotaPolicy};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::*;
use crazyagent::protocol::{CredentialRef, Cursor, PROTOCOL_VERSION, RequestId};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;

struct Fixture {
    root: tempfile::TempDir,
    service: EngineService,
    _shutdown: watch::Sender<bool>,
    global: PathBuf,
}

impl Fixture {
    async fn start() -> Self {
        Self::start_with(5000).await
    }

    async fn start_with(timeout_ms: u32) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-acct-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, format!("version = 1\ntimeout_ms = {timeout_ms}\n")).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (shutdown, stopped) = watch::channel(false);
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
            _shutdown: shutdown,
            global,
        }
    }

    async fn restart(&mut self) {
        self.restart_after(|_| {}).await;
    }

    async fn restart_after(&mut self, change: impl FnOnce(&Path)) {
        tokio::time::timeout(Duration::from_secs(10), self.service.shutdown())
            .await
            .unwrap()
            .unwrap();
        change(self.root.path());
        let stopped = self._shutdown.subscribe();
        self.service = EngineService::start_with_settings_sources(
            self.root.path(),
            "e.sock",
            &self.global,
            &self.root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
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

fn draft(
    provider: Provider,
    method: SignInMethod,
    name: &str,
    url: Option<String>,
    model: Option<&str>,
) -> AccountDraft {
    AccountDraft {
        provider,
        method,
        name: name.into(),
        url,
        model: model.map(str::to_owned),
    }
}

fn key(value: &str) -> Option<AccountSecret> {
    Some(AccountSecret::new(value.as_bytes().to_vec()).unwrap())
}

async fn closed_port() -> u16 {
    let listener = crazyagent::spawn_gate::bind_tcp("127.0.0.1:0")
        .await
        .unwrap();
    listener.local_addr().unwrap().port()
}

async fn all_accounts(client: &OwnerClient) -> Vec<AccountView> {
    let mut accounts = Vec::new();
    let mut after = None;
    loop {
        let page = client.list_accounts(after, 200).await.unwrap();
        accounts.extend(page.accounts);
        match page.next {
            Some(next) => after = Some(next),
            None => return accounts,
        }
    }
}

fn files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        if metadata.is_dir() {
            found.extend(files(&path));
        } else if metadata.is_file() {
            found.push(path);
        }
    }
    found
}

fn holders(root: &Path, secret: &[u8]) -> Vec<PathBuf> {
    files(root)
        .into_iter()
        .filter(|path| {
            std::fs::read(path)
                .is_ok_and(|bytes| bytes.windows(secret.len()).any(|window| window == secret))
        })
        .collect()
}

async fn serve_once(listener: &TcpListener, status: &str, body: &str) -> String {
    serve_after(listener, Duration::ZERO, status, body).await
}

async fn serve_after(listener: &TcpListener, delay: Duration, status: &str, body: &str) -> String {
    let (mut stream, request) = loop {
        let (mut stream, _) = crazyagent::spawn_gate::accept_tcp(listener).await.unwrap();
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
    tokio::time::sleep(delay).await;
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(reply.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
    String::from_utf8(request).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_add_path_lists_plain_statuses_and_supports_rename_and_remove() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let catalog = client.account_catalog().await.unwrap();
    assert_eq!(catalog.len(), 7);
    assert_eq!(catalog[0].methods[0].label, "Sign in with ChatGPT");
    assert!(all_accounts(&client).await.is_empty());

    let up = crazyagent::spawn_gate::bind_tcp("127.0.0.1:0")
        .await
        .unwrap();
    let up_url = format!("http://127.0.0.1:{}/v1", up.local_addr().unwrap().port());
    let down_url = format!("http://127.0.0.1:{}/v1", closed_port().await);

    let chatgpt = client
        .add_account(
            draft(
                Provider::Chatgpt,
                SignInMethod::Subscription,
                "Work ChatGPT",
                None,
                None,
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        (chatgpt.name.as_str(), chatgpt.status_label.as_str()),
        ("Work ChatGPT", "Not signed in")
    );
    assert_eq!(chatgpt.method_label, "Sign in with ChatGPT");
    assert_eq!(
        (chatgpt.url.as_deref(), chatgpt.model.as_deref()),
        (None, None)
    );
    let claude = client
        .add_account(
            draft(Provider::Claude, SignInMethod::Subscription, "", None, None),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        (claude.name.as_str(), claude.status),
        ("Claude", AccountStatus::NotSignedIn)
    );
    let local = client
        .add_account(
            draft(
                Provider::Local,
                SignInMethod::Url,
                "Ollama",
                Some(up_url.clone()),
                Some("qwen2.5:0.5b"),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(local.status_label, "Signed in");
    assert_eq!(local.url.as_deref(), Some(up_url.as_str()));
    let gone = client
        .add_account(
            draft(
                Provider::Local,
                SignInMethod::Url,
                "Old box",
                Some(down_url),
                Some("m"),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(gone.status_label, "Can't reach this URL");
    let router = client
        .add_account(
            draft(
                Provider::Openrouter,
                SignInMethod::ApiKey,
                "Router",
                None,
                Some("openai/gpt-4o"),
            ),
            key("sk-or-placeholder-1"),
        )
        .await
        .unwrap();
    assert_eq!(router.status_label, "Key saved");
    assert_eq!(router.url.as_deref(), Some("https://openrouter.ai/api/v1"));
    assert_eq!(
        client
            .add_account(
                draft(
                    Provider::Openrouter,
                    SignInMethod::ApiKey,
                    "No key",
                    None,
                    Some("m")
                ),
                None
            )
            .await
            .unwrap_err()
            .0,
        "account_key_required"
    );
    assert_eq!(
        client
            .add_account(
                draft(
                    Provider::Local,
                    SignInMethod::Url,
                    "",
                    Some("http://10.0.0.1:11434/v1".into()),
                    Some("m")
                ),
                None
            )
            .await
            .unwrap_err()
            .0,
        "account_local_url_required"
    );

    let listed = all_accounts(&client).await;
    assert_eq!(
        listed
            .iter()
            .map(|account| (account.name.as_str(), account.status_label.as_str()))
            .collect::<Vec<_>>(),
        [
            ("Work ChatGPT", "Not signed in"),
            ("Claude", "Not signed in"),
            ("Ollama", "Signed in"),
            ("Old box", "Can't reach this URL"),
            ("Router", "Key saved"),
        ]
    );
    let renamed = client
        .rename_account(local.id.clone(), "  Home Ollama ".into())
        .await
        .unwrap();
    assert_eq!(renamed.name, "Home Ollama");
    assert_eq!(
        client
            .rename_account(local.id.clone(), "bad\u{202e}name".into())
            .await
            .unwrap_err()
            .0,
        "account_name_invalid"
    );
    client.remove_account(gone.id.clone()).await.unwrap();
    assert_eq!(
        client.remove_account(gone.id.clone()).await.unwrap_err().0,
        "account_not_found"
    );

    let page = match client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("pool-read").unwrap(),
            expected_revision: crazyagent::protocol::Cursor::new(0),
            operation: OwnerOperation::Read {
                query: OwnerRead::ModelAccessPage {
                    collection: ModelAccessCollection::Pools,
                    cursor: None,
                    limit: 16,
                },
            },
        })
        .await
        .unwrap()
        .response
    {
        OwnerResponse::Read { state } => state,
        other => panic!("owner read required: {other:?}"),
    };
    let OwnerData::ModelAccessPage { source_hash, .. } = page.data else {
        panic!("model access page required")
    };
    let pooled = client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("pool-create").unwrap(),
            expected_revision: page.revision,
            operation: OwnerOperation::Apply {
                mutation: OwnerMutation::ModelAccess {
                    expected_config_id: page.config_id,
                    expected_source_hash: source_hash,
                    edit: ModelAccessEdit::CreatePool {
                        pool: AccountPool {
                            id: CredentialRef::new("pool-one").unwrap(),
                            name: None,
                            members: vec![router.id.clone()],
                            projects: vec![],
                            roles: vec![],
                            fallback_allowed: false,
                            provider_quota_policy: ProviderQuotaPolicy::default(),
                        },
                    },
                },
            },
        })
        .await
        .unwrap();
    assert!(
        matches!(pooled.response, OwnerResponse::Applied { ref operation } if operation.result == OwnerResult::Applied),
        "{pooled:?}"
    );

    let listed = all_accounts(&client).await;
    assert_eq!(
        listed
            .iter()
            .map(|account| account.name.as_str())
            .collect::<Vec<_>>(),
        ["Work ChatGPT", "Claude", "Home Ollama", "Router"]
    );
    let saved = std::fs::read_to_string(&fixture.global).unwrap();
    assert!(
        saved.contains(chatgpt.id.as_str()),
        "subscription accounts are routable for sign-in"
    );
    assert!(
        saved.contains(router.id.as_str()),
        "pooled accounts are routable"
    );
    assert!(!saved.contains(local.id.as_str()) && !saved.contains(gone.id.as_str()));
    assert!(
        !saved.contains("Home Ollama"),
        "names stay in the account registry"
    );
    drop(up);
    fixture.stop().await;
}

/// Wiring test: a local stand-in server answers /v1/models with an invented model list
/// and an invented 401, so this checks what the engine sends and how it keeps the key,
/// not a provider. Real model list: an_openai_compatible_account_check_lists_the_real_ollama_models.
/// A real 401 needs an approved provider profile and is not covered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wiring_api_key_reaches_a_stand_in_server_only_in_the_authorization_header() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let secret = format!("sk-local-{}", std::process::id());
    let listener = crazyagent::spawn_gate::bind_tcp("127.0.0.1:0")
        .await
        .unwrap();
    let url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let account = client
        .add_account(
            draft(
                Provider::OpenaiCompatible,
                SignInMethod::Url,
                "Lab server",
                Some(url.clone()),
                Some("lab-model"),
            ),
            key(&secret),
        )
        .await
        .unwrap();
    assert_eq!(account.status_label, "Key saved");
    assert!(!format!("{account:?}").contains(&secret));

    let (request, checked) = tokio::join!(
        serve_once(
            &listener,
            "200 OK",
            r#"{"data":[{"id":"lab-model"},{"id":"other"}]}"#
        ),
        client.check_account(account.id.clone()),
    );
    let checked = checked.unwrap();
    assert!(
        request.starts_with("GET /v1/models HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(
        request.contains(&format!("\r\nAuthorization: Bearer {secret}\r\n")),
        "key must reach the request"
    );
    assert_eq!(checked.models, ["lab-model", "other"]);
    assert_eq!(checked.account.status_label, "Key saved");
    assert!(!format!("{checked:?}").contains(&secret));

    let (_, rejected) = tokio::join!(
        serve_once(&listener, "401 Unauthorized", r#"{"error":"bad key"}"#),
        client.check_account(account.id.clone()),
    );
    assert_eq!(rejected.unwrap().account.status_label, "Not signed in");

    let mut fixture = fixture;
    fixture.restart().await;
    let client = fixture.client().await;
    let after = all_accounts(&client).await;
    assert_eq!(
        after[0].status_label, "Key saved",
        "the saved key survives an engine restart"
    );
    let held = holders(fixture.root.path(), secret.as_bytes());
    assert_eq!(
        held.len(),
        1,
        "only the private key file may hold the key: {held:?}"
    );
    assert!(held[0].parent().unwrap().ends_with("account-keys"));
    assert_eq!(
        std::fs::metadata(&held[0]).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(held[0].parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert!(
        !std::fs::read_to_string(&fixture.global)
            .unwrap()
            .contains(&secret)
    );

    drop(listener);
    let (_, unreachable) = tokio::join!(async {}, client.check_account(account.id.clone()));
    assert_eq!(
        unreachable.unwrap().account.status_label,
        "Can't reach this URL"
    );
    client.remove_account(account.id.clone()).await.unwrap();
    assert!(
        holders(fixture.root.path(), secret.as_bytes()).is_empty(),
        "removing the account removes its key"
    );
    fixture.stop().await;
}

/// Rewrites the saved address of one account in the stopped engine's registry, as an
/// account saved before plain-http addresses outside this machine were refused would hold.
/// Inputs: the engine root, the account id and the old address. Output: none.
fn saved_before_the_fix(root: &Path, id: &CredentialRef, url: &str) {
    let db = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    let body: Vec<u8> = db
        .query_row(
            "SELECT body FROM accounts WHERE id = ?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(&body).unwrap();
    record["connection"]["endpoint"]["url"] = url.into();
    let changed = db
        .execute(
            "UPDATE accounts SET body = ?1 WHERE id = ?2",
            rusqlite::params![serde_json::to_vec(&record).unwrap(), id.as_str()],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

/// Accepts every connection already waiting on `listener`, or arriving within a short
/// window, and reads what each one sent until it closes or goes quiet.
/// Input: the listener. Output: the bytes of each connection, in arrival order.
async fn received(listener: &TcpListener) -> Vec<Vec<u8>> {
    let mut requests = Vec::new();
    while let Ok(Ok((mut stream, _))) = tokio::time::timeout(
        Duration::from_millis(300),
        crazyagent::spawn_gate::accept_tcp(listener),
    )
    .await
    {
        let mut request = Vec::new();
        let _ = tokio::time::timeout(Duration::from_millis(300), stream.read_to_end(&mut request))
            .await;
        requests.push(request);
    }
    requests
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_saved_key_never_goes_out_over_plain_http_to_another_host() {
    let mut fixture = Fixture::start().await;
    let client = fixture.client().await;
    let secret = format!("sk-remote-{}", std::process::id());
    let listener = crazyagent::spawn_gate::bind_tcp("127.0.0.1:0")
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let remote = format!("http://0.0.0.0:{port}/v1");
    let lab = |url: &str| {
        draft(
            Provider::OpenaiCompatible,
            SignInMethod::Url,
            "Lab server",
            Some(url.into()),
            Some("lab-model"),
        )
    };
    assert_eq!(
        client
            .add_account(lab(&remote), key(&secret))
            .await
            .unwrap_err()
            .0,
        "account_url_invalid"
    );
    assert!(
        holders(fixture.root.path(), secret.as_bytes()).is_empty(),
        "a refused account keeps no key"
    );

    let account = client
        .add_account(lab(&format!("http://127.0.0.1:{port}/v1")), key(&secret))
        .await
        .unwrap();
    drop(client);
    received(&listener).await;
    fixture
        .restart_after(|root| saved_before_the_fix(root, &account.id, &remote))
        .await;
    let client = fixture.client().await;
    let checked = client.check_account(account.id.clone()).await;
    let requests = received(&listener).await;
    assert!(
        !requests.iter().any(|request| request
            .windows(secret.len())
            .any(|window| window == secret.as_bytes())),
        "the saved key went out over plain http to a host outside this machine"
    );
    assert!(
        requests.is_empty(),
        "Check connected over plain http to a host outside this machine"
    );
    assert_eq!(checked.unwrap_err().0, "account_url_invalid");
    client.remove_account(account.id.clone()).await.unwrap();
    assert!(holders(fixture.root.path(), secret.as_bytes()).is_empty());
    fixture.stop().await;
}

/// Wiring test: a local stand-in server answers the check with an invented model list,
/// so this checks the key replacement and what the engine sends, not a provider.
/// Real model list: an_openai_compatible_account_check_lists_the_real_ollama_models.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wiring_replacing_a_key_keeps_the_account_and_deletes_the_old_key() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let old = format!("sk-old-{}-leaked", std::process::id());
    let new = format!("sk-new-{}-rotated", std::process::id());
    let listener = crazyagent::spawn_gate::bind_tcp("127.0.0.1:0")
        .await
        .unwrap();
    let url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let account = client
        .add_account(
            draft(
                Provider::OpenaiCompatible,
                SignInMethod::Url,
                "Lab server",
                Some(url),
                Some("lab-model"),
            ),
            key(&old),
        )
        .await
        .unwrap();
    assert_eq!(holders(fixture.root.path(), old.as_bytes()).len(), 1);

    let replaced = client
        .replace_account_key(account.id.clone(), key(&new).unwrap())
        .await
        .unwrap();
    assert_eq!(replaced.id, account.id, "the same account keeps its id");
    assert_eq!(replaced.name, "Lab server");
    assert_eq!(replaced.status_label, "Key saved");
    assert!(!format!("{replaced:?}").contains(&new));
    let listed = all_accounts(&client).await;
    assert_eq!(listed.len(), 1, "no second account: {listed:?}");
    assert_eq!(listed[0].id, account.id);
    assert!(
        holders(fixture.root.path(), old.as_bytes()).is_empty(),
        "the old key is gone from disk"
    );
    let held = holders(fixture.root.path(), new.as_bytes());
    assert_eq!(
        held.len(),
        1,
        "only the key file holds the new key: {held:?}"
    );
    assert!(held[0].parent().unwrap().ends_with("account-keys"));
    assert_eq!(
        std::fs::metadata(&held[0]).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::read_dir(held[0].parent().unwrap())
            .unwrap()
            .count(),
        1,
        "no temporary key file is left behind"
    );

    let (request, checked) = tokio::join!(
        serve_once(&listener, "200 OK", r#"{"data":[{"id":"lab-model"}]}"#),
        client.check_account(account.id.clone()),
    );
    assert_eq!(checked.unwrap().account.status_label, "Key saved");
    assert!(
        request.contains(&format!("\r\nAuthorization: Bearer {new}\r\n")),
        "the new key reaches the request"
    );
    assert!(!request.contains(&old), "the old key is never sent again");

    let local = client
        .add_account(
            draft(
                Provider::Local,
                SignInMethod::Url,
                "Ollama",
                None,
                Some("qwen2.5:0.5b"),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        client
            .replace_account_key(local.id.clone(), key(&new).unwrap())
            .await
            .unwrap_err()
            .0,
        "account_key_not_allowed"
    );
    client.remove_account(account.id.clone()).await.unwrap();
    assert_eq!(
        client
            .replace_account_key(account.id.clone(), key(&new).unwrap())
            .await
            .unwrap_err()
            .0,
        "account_not_found"
    );
    assert!(
        holders(fixture.root.path(), new.as_bytes()).is_empty(),
        "a refused replace saves nothing"
    );
    fixture.stop().await;
}

/// Wiring test: a local stand-in server holds an invented model list past the owner wait,
/// so this checks the engine's deadline handling; a slow real provider cannot be arranged.
/// Real model list: an_openai_compatible_account_check_lists_the_real_ollama_models.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wiring_an_account_check_slower_than_the_owner_wait_still_answers() {
    let fixture = Fixture::start_with(1000).await;
    let client = fixture.client().await;
    let listener = crazyagent::spawn_gate::bind_tcp("127.0.0.1:0")
        .await
        .unwrap();
    let url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let account = client
        .add_account(
            draft(
                Provider::OpenaiCompatible,
                SignInMethod::Url,
                "Slow lab server",
                Some(url),
                Some("lab-model"),
            ),
            key("slow-lab-key"),
        )
        .await
        .unwrap();
    let (waiting, _) = OwnerClient::discover(
        fixture.root.path(),
        OwnerClientKind::Tui,
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    let slow = Duration::from_millis(2500);
    let started = Instant::now();
    let (request, checked) = tokio::join!(
        serve_after(
            &listener,
            slow,
            "200 OK",
            r#"{"data":[{"id":"lab-model"}]}"#
        ),
        waiting.check_account(account.id.clone()),
    );
    assert!(
        request.starts_with("GET /v1/models HTTP/1.1\r\n"),
        "{request}"
    );
    let checked = checked.unwrap_or_else(|error| panic!("the check answered {}", error.0));
    assert!(started.elapsed() >= slow);
    assert_eq!(checked.models, ["lab-model"]);
    assert_eq!(checked.account.status_label, "Key saved");
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_model_discovery_reads_the_real_ollama() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let models = client
        .local_models("http://127.0.0.1:11434/v1".into())
        .await
        .unwrap();
    assert!(
        models.iter().any(|model| model == "qwen2.5:0.5b"),
        "real local ollama models: {models:?}"
    );
    assert_eq!(
        client
            .local_models("http://example.com:11434/v1".into())
            .await
            .unwrap_err()
            .0,
        "account_local_url_required"
    );
    fixture.stop().await;
}

/// Checks an OpenAI-compatible account pointed at the real local Ollama through the owner
/// API: the engine reads the model list Ollama really serves at /v1/models. This is the
/// real-provider counterpart of the stand-in server wiring tests; Ollama ignores the key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_openai_compatible_account_check_lists_the_real_ollama_models() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let account = client
        .add_account(
            draft(
                Provider::OpenaiCompatible,
                SignInMethod::Url,
                "Local Ollama",
                Some("http://127.0.0.1:11434/v1".into()),
                Some("qwen2.5:0.5b"),
            ),
            key("sk-local-ollama-check"),
        )
        .await
        .unwrap();
    let checked = client.check_account(account.id.clone()).await.unwrap();
    assert!(
        checked.models.iter().any(|model| model == "qwen2.5:0.5b"),
        "real local ollama models: {:?}",
        checked.models
    );
    assert_eq!(checked.account.status_label, "Key saved");
    client.remove_account(account.id.clone()).await.unwrap();
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_thousand_accounts_are_created_and_listed_within_bounds() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let url = format!("http://127.0.0.1:{}/v1", closed_port().await);
    let created = Instant::now();
    for index in 0..1000 {
        client
            .add_account(
                draft(
                    Provider::Local,
                    SignInMethod::Url,
                    &format!("Account {index}"),
                    Some(url.clone()),
                    Some("m"),
                ),
                None,
            )
            .await
            .unwrap();
    }
    let creation = created.elapsed();
    let listed = Instant::now();
    let accounts = all_accounts(&client).await;
    let listing = listed.elapsed();
    eprintln!("created 1000 accounts in {creation:?}; listed them in {listing:?}");
    assert_eq!(accounts.len(), 1000);
    assert_eq!(accounts[0].name, "Account 0");
    assert_eq!(accounts[999].name, "Account 999");
    assert_eq!(
        accounts
            .iter()
            .map(|account| &account.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        1000
    );
    assert!(
        listing < Duration::from_secs(5),
        "listing 1000 accounts took {listing:?}"
    );
    assert!(
        creation < Duration::from_secs(120),
        "creating 1000 accounts took {creation:?}"
    );
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscription_accounts_start_the_managed_sign_in_and_follow_its_phase() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let (_, identity) = OwnerClient::discover(
        fixture.root.path(),
        OwnerClientKind::Tui,
        Duration::from_secs(20),
    )
    .await
    .unwrap();
    for (provider, method, name) in [
        (
            Provider::Claude,
            NativeAuthMethod::ClaudeOfficial,
            "Work Claude",
        ),
        (
            Provider::Chatgpt,
            NativeAuthMethod::CodexDeviceCode,
            "Work ChatGPT",
        ),
    ] {
        let account = client
            .add_account(
                draft(provider, SignInMethod::Subscription, name, None, None),
                None,
            )
            .await
            .unwrap();
        assert_eq!(account.status_label, "Not signed in");
        let read = client
            .request(OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new(format!("read-{}", account.id.as_str())).unwrap(),
                expected_revision: Cursor::new(0),
                operation: OwnerOperation::Read {
                    query: OwnerRead::ModelAccessRecord {
                        collection: ModelAccessCollection::Connections,
                        id: account.id.clone(),
                    },
                },
            })
            .await
            .unwrap();
        let OwnerResponse::Read { state } = read.response else {
            panic!("connection read required")
        };
        let OwnerData::ModelAccessRecord {
            source_hash,
            record: ModelAccessRecord::Connection { connection },
            ..
        } = state.data
        else {
            panic!("the account must be a config connection the sign-in can bind to")
        };
        let reply = client
            .start_native_auth(NativeAuthStart {
                request_id: RequestId::new(format!("start-{}", account.id.as_str())).unwrap(),
                scope: NativeAuthScope {
                    expected_database_instance: identity.database_instance().to_owned(),
                    connection: account.id.clone(),
                    expected_config_id: state.config_id,
                    expected_source_hash: source_hash,
                },
                method,
            })
            .await
            .unwrap();
        assert_eq!(reply.fact.binding.connection, connection);
        assert_eq!(reply.fact.phase, NativeAuthPhase::Unavailable);
        let listed = all_accounts(&client).await;
        let shown = listed
            .iter()
            .find(|listed| listed.id == account.id)
            .unwrap();
        assert_eq!(shown.name, name);
        assert_eq!(shown.status_label, "Not signed in");
    }
    fixture.stop().await;
}

async fn pools_page(client: &OwnerClient, id: &str) -> (OwnerReadReply, String, Vec<AccountPool>) {
    let state = match client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(id).unwrap(),
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Read {
                query: OwnerRead::ModelAccessPage {
                    collection: ModelAccessCollection::Pools,
                    cursor: None,
                    limit: 16,
                },
            },
        })
        .await
        .unwrap()
        .response
    {
        OwnerResponse::Read { state } => state,
        other => panic!("owner read required: {other:?}"),
    };
    let OwnerData::ModelAccessPage {
        source_hash, items, ..
    } = &state.data
    else {
        panic!("model access page required")
    };
    let pools = items
        .iter()
        .filter_map(|record| match record {
            ModelAccessRecord::Pool { pool } => Some(pool.clone()),
            _ => None,
        })
        .collect();
    (state.clone(), source_hash.clone(), pools)
}

async fn create_pool(client: &OwnerClient, id: &str, members: Vec<CredentialRef>, project: &str) {
    let (page, source_hash, _) = pools_page(client, &format!("{id}-read")).await;
    let created = client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(format!("{id}-create")).unwrap(),
            expected_revision: page.revision,
            operation: OwnerOperation::Apply {
                mutation: OwnerMutation::ModelAccess {
                    expected_config_id: page.config_id,
                    expected_source_hash: source_hash,
                    edit: ModelAccessEdit::CreatePool {
                        pool: AccountPool {
                            id: CredentialRef::new(id).unwrap(),
                            name: None,
                            members,
                            projects: vec![CredentialRef::new(project).unwrap()],
                            roles: vec![CredentialRef::new("project_leader").unwrap()],
                            fallback_allowed: false,
                            provider_quota_policy: ProviderQuotaPolicy::default(),
                        },
                    },
                },
            },
        })
        .await
        .unwrap();
    assert!(
        matches!(created.response, OwnerResponse::Applied { ref operation } if operation.result == OwnerResult::Applied),
        "{created:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_an_account_in_pools_takes_it_out_of_them_in_one_operation() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let subscription =
        |provider, name: &str| draft(provider, SignInMethod::Subscription, name, None, None);
    let leaving = client
        .add_account(subscription(Provider::Chatgpt, "Leaving"), None)
        .await
        .unwrap();
    let staying = client
        .add_account(subscription(Provider::Claude, "Staying"), None)
        .await
        .unwrap();
    create_pool(
        &client,
        "shared-pool",
        vec![leaving.id.clone(), staying.id.clone()],
        "demo",
    )
    .await;
    create_pool(&client, "solo-pool", vec![leaving.id.clone()], "other").await;
    client.remove_account(leaving.id.clone()).await.unwrap();
    let (_, _, pools) = pools_page(&client, "pools-after").await;
    assert_eq!(pools.len(), 1);
    assert_eq!(pools[0].id.as_str(), "shared-pool");
    assert_eq!(pools[0].members, std::slice::from_ref(&staying.id));
    assert_eq!(pools[0].projects, [CredentialRef::new("demo").unwrap()]);
    let listed = all_accounts(&client).await;
    assert_eq!(
        listed
            .iter()
            .map(|account| account.name.as_str())
            .collect::<Vec<_>>(),
        ["Staying"]
    );
    let saved = std::fs::read_to_string(&fixture.global).unwrap();
    assert!(!saved.contains(leaving.id.as_str()));
    assert!(!saved.contains("solo-pool"));
    assert_eq!(
        client
            .remove_account(leaving.id.clone())
            .await
            .unwrap_err()
            .0,
        "account_not_found"
    );
    fixture.stop().await;
}
