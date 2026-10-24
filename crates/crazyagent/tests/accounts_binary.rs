#![cfg(unix)]

use crazyagent::accounts::{AccountDraft, AccountView, Provider, SignInMethod};
use crazyagent::client_controller::OwnerController;
use crazyagent::config::{AccountPool, ModelAccessEdit, ProviderQuotaPolicy};
use crazyagent::engine::owner::{
    AccountProfileOutcome, AccountProfileRequest, AccountSecret, ModelAccessCollection,
    ModelAccessRecord, NativeAuthScope, OwnerClient, OwnerClientKind, OwnerData, OwnerMutation,
    OwnerRead, OwnerResponse, OwnerResult,
};
use crazyagent::master_session::{MasterOwnerAction, MasterTurnPhase, MasterTurnView};
use crazyagent::protocol::{CredentialRef, ProjectId, ProjectSpec, RequestId};
use std::ffi::OsStr;
use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct Engine {
    child: Child,
    stdout: PathBuf,
}

impl Engine {
    fn start(root: &Path, run: &str) -> Self {
        Self::start_with(root, run, &[])
    }

    fn start_with(root: &Path, run: &str, extra: &[&OsStr]) -> Self {
        let stdout = root.join(format!("{run}.stdout"));
        let child = crazyagent::spawn_gate::spawn(
            Command::new(env!("CARGO_BIN_EXE_crazyagent"))
                .arg("engine")
                .arg(root)
                .arg(root.join("global.toml"))
                .arg(root.join("project.toml"))
                .arg("engine.sock")
                .arg("--writable-settings")
                .args(extra)
                .stdin(Stdio::null())
                .stdout(File::create(&stdout).unwrap())
                .stderr(File::create(root.join(format!("{run}.stderr"))).unwrap()),
        )
        .unwrap();
        let mut engine = Self { child, stdout };
        let deadline = Instant::now() + Duration::from_secs(20);
        while std::fs::read(&engine.stdout).unwrap() != b"ready\n" {
            assert!(
                engine.child.try_wait().unwrap().is_none(),
                "engine exited before ready"
            );
            assert!(Instant::now() < deadline, "engine readiness deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
        engine
    }

    fn stop(&mut self) {
        let terminated = crazyagent::spawn_gate::status(Command::new("/bin/kill").args([
            "-s",
            "TERM",
            &self.child.id().to_string(),
        ]))
        .unwrap();
        assert!(terminated.success());
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "engine shutdown deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
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

async fn client(root: &Path) -> OwnerClient {
    OwnerClient::discover(root, OwnerClientKind::Desktop, Duration::from_secs(20))
        .await
        .unwrap()
        .0
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

async fn serve_models(listener: &TcpListener) -> String {
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
    let body = r#"{"data":[{"id":"lab-model"}]}"#;
    let reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(reply.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
    String::from_utf8(request).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_engine_binary_keeps_accounts_and_keys_across_restart() {
    let root = tempfile::Builder::new()
        .prefix("ca-bacct-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::fs::canonicalize(root.path()).unwrap();
    let (_guard, root) = (root, path);
    let global = root.join("global.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let secret = format!("sk-binary-{}", std::process::id());
    let listener = crazyagent::spawn_gate::bind_tcp("127.0.0.1:0")
        .await
        .unwrap();
    let lab_url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );

    let mut first = Engine::start(&root, "first");
    let owner = client(&root).await;
    let lab = owner
        .add_account(
            AccountDraft {
                provider: Provider::OpenaiCompatible,
                method: SignInMethod::Url,
                name: "Lab server".into(),
                url: Some(lab_url.clone()),
                model: Some("lab-model".into()),
            },
            Some(AccountSecret::new(secret.as_bytes().to_vec()).unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(lab.status_label, "Key saved");
    let local = owner
        .add_account(
            AccountDraft {
                provider: Provider::Local,
                method: SignInMethod::Url,
                name: "Ollama".into(),
                url: None,
                model: Some("qwen2.5:0.5b".into()),
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(local.status_label, "Signed in");
    let claude = owner
        .add_account(
            AccountDraft {
                provider: Provider::Claude,
                method: SignInMethod::Subscription,
                name: "Work Claude".into(),
                url: None,
                model: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(claude.status_label, "Not signed in");
    drop(owner);
    first.stop();

    let mut second = Engine::start(&root, "second");
    let owner = client(&root).await;
    let listed = all_accounts(&owner).await;
    let shown: Vec<(&str, &str, &str)> = listed
        .iter()
        .map(|account| {
            (
                account.name.as_str(),
                account.provider_name.as_str(),
                account.status_label.as_str(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("Lab server", "Any OpenAI-compatible URL", "Key saved"),
            ("Ollama", "Local models (Ollama)", "Signed in"),
            ("Work Claude", "Claude", "Not signed in"),
        ]
    );
    let (request, checked) =
        tokio::join!(serve_models(&listener), owner.check_account(lab.id.clone()));
    assert!(
        request.contains(&format!("\r\nAuthorization: Bearer {secret}\r\n")),
        "the restarted binary must read the saved key"
    );
    assert_eq!(checked.unwrap().models, ["lab-model"]);
    drop(owner);
    second.stop();

    let held = holders(&root, secret.as_bytes());
    assert_eq!(
        held.len(),
        1,
        "only the private key file may hold the key, not config, database, stdout or stderr: {held:?}"
    );
    assert!(held[0].parent().unwrap().ends_with("account-keys"));
}

struct Retained(Option<tempfile::TempDir>);

impl Drop for Retained {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Some(root) = self.0.take()
        {
            eprintln!(
                "master accounts fixture retained at {}",
                root.keep().display()
            );
        }
    }
}

fn rcli_bundle() -> PathBuf {
    let bundle = std::env::var_os("CRAZYAGENT_RCLI")
        .map(PathBuf::from)
        .expect("UNAVAILABLE: set CRAZYAGENT_RCLI to the verified crazyagent-rcli bundle");
    assert!(
        bundle.is_absolute() && bundle.is_file(),
        "UNAVAILABLE: {bundle:?}"
    );
    bundle
}

fn git_repository(path: &Path) -> String {
    std::fs::create_dir(path).unwrap();
    std::fs::write(path.join("README.md"), "# hello\n").unwrap();
    for arguments in [
        &["init", "-q", "-b", "main"][..],
        &["add", "README.md"],
        &[
            "-c",
            "user.name=owner",
            "-c",
            "user.email=owner@example.invalid",
            "commit",
            "-qm",
            "init",
        ],
    ] {
        let status =
            crazyagent::spawn_gate::status(Command::new("git").args(arguments).current_dir(path))
                .unwrap();
        assert!(status.success(), "git {arguments:?}");
    }
    path.to_str().unwrap().to_owned()
}

async fn key_gate(listener: TcpListener, key: String, seen: Arc<Mutex<Vec<bool>>>) {
    loop {
        let Ok((mut client, _)) = crazyagent::spawn_gate::accept_tcp(&listener).await else {
            return;
        };
        let key = key.clone();
        let seen = seen.clone();
        tokio::spawn(async move {
            let mut head = Vec::new();
            let mut buffer = [0; 4096];
            while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                let Ok(size) = client.read(&mut buffer).await else {
                    return;
                };
                if size == 0 || head.len() > 65536 {
                    return;
                }
                head.extend_from_slice(&buffer[..size]);
            }
            let expected = format!("Bearer {key}");
            let allowed = String::from_utf8_lossy(&head).split("\r\n").any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("authorization") && value.trim() == expected
                })
            });
            seen.lock().unwrap().push(allowed);
            if !allowed {
                let body = r#"{"error":{"message":"missing or wrong key"}}"#;
                let reply = format!(
                    "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = client.write_all(reply.as_bytes()).await;
                return;
            }
            let Ok(mut upstream) = crazyagent::spawn_gate::connect_tcp("127.0.0.1:11434").await
            else {
                return;
            };
            if upstream.write_all(&head).await.is_ok() {
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            }
        });
    }
}

async fn apply(controller: &mut OwnerController, generation: &mut u64, mutation: OwnerMutation) {
    controller.read(OwnerRead::Capabilities).await.unwrap();
    *generation += 1;
    controller.begin_mutation(mutation, *generation).unwrap();
    let response = controller.send_pending().await.unwrap();
    assert!(
        matches!(&response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied),
        "{response:?}"
    );
}

async fn source(controller: &mut OwnerController) -> (crazyagent::protocol::ConfigId, String) {
    let state = controller
        .read(OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Accounts,
            cursor: None,
            limit: 16,
        })
        .await
        .unwrap();
    let OwnerData::ModelAccessPage { source_hash, .. } = state.data else {
        panic!("model access source required")
    };
    (state.config_id, source_hash)
}

async fn finished_turn(
    controller: &mut OwnerController,
    project_id: &ProjectId,
    session_id: &crazyagent::protocol::SessionId,
) -> MasterTurnView {
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        let state = controller
            .read(OwnerRead::ProjectMasterTurns {
                project_id: project_id.clone(),
                session_id: session_id.clone(),
                cursor: None,
                limit: 16,
            })
            .await
            .unwrap();
        let OwnerData::ProjectMasterTurns { page } = state.data else {
            panic!("master turns required")
        };
        if let Some(turn) = page.turns.into_iter().find(|turn| {
            matches!(
                turn.phase,
                MasterTurnPhase::Succeeded
                    | MasterTurnPhase::Failed
                    | MasterTurnPhase::Cancelled
                    | MasterTurnPhase::Uncertain
                    | MasterTurnPhase::Blocked
            )
        }) {
            return turn;
        }
        assert!(Instant::now() < deadline, "master turn deadline");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_masters_reach_models_with_saved_keys_and_local_accounts_through_rcli() {
    let bundle = rcli_bundle();
    let root = tempfile::Builder::new()
        .prefix("ca-macct-")
        .tempdir_in("/private/tmp")
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::fs::canonicalize(root.path()).unwrap();
    let (_guard, root) = (Retained(Some(root)), path);
    let global = root.join("global.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n").unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let secret = format!("sk-gate-{}", std::process::id());
    let listener = crazyagent::spawn_gate::bind_tcp("127.0.0.1:0")
        .await
        .unwrap();
    let gate_url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let gate = tokio::spawn(key_gate(listener, secret.clone(), seen.clone()));

    let mut engine = Engine::start_with(
        &root,
        "master",
        &[OsStr::new("--rcli-executable"), bundle.as_os_str()],
    );
    let (owner, identity) =
        OwnerClient::discover(&root, OwnerClientKind::Desktop, Duration::from_secs(20))
            .await
            .unwrap();
    let keyed = owner
        .add_account(
            AccountDraft {
                provider: Provider::OpenaiCompatible,
                method: SignInMethod::Url,
                name: "Key gate".into(),
                url: Some(gate_url),
                model: Some("qwen2.5:0.5b".into()),
            },
            Some(AccountSecret::new(secret.as_bytes().to_vec()).unwrap()),
        )
        .await
        .unwrap();
    let local = owner
        .add_account(
            AccountDraft {
                provider: Provider::Local,
                method: SignInMethod::Url,
                name: "Laptop".into(),
                url: None,
                model: Some("qwen2.5:0.5b".into()),
            },
            None,
        )
        .await
        .unwrap();
    let mut controller =
        OwnerController::attach(&root, OwnerClientKind::Tui, Duration::from_secs(20))
            .await
            .unwrap();
    let mut generation = 0;
    for (account, name) in [(&keyed, "gate"), (&local, "local")] {
        let project_id = ProjectId::new(format!("{name}-project")).unwrap();
        let repository = git_repository(&root.join(format!("{name}-repository")));
        apply(
            &mut controller,
            &mut generation,
            OwnerMutation::CreateProject {
                project_id: project_id.clone(),
                spec: ProjectSpec {
                    name: format!("{name} project"),
                    repositories: vec![repository],
                },
            },
        )
        .await;
        let (config_id, source_hash) = source(&mut controller).await;
        apply(
            &mut controller,
            &mut generation,
            OwnerMutation::ModelAccess {
                expected_config_id: config_id,
                expected_source_hash: source_hash,
                edit: ModelAccessEdit::CreatePool {
                    pool: AccountPool {
                        id: CredentialRef::new(format!("{name}-pool")).unwrap(),
                        name: None,
                        members: vec![account.id.clone()],
                        projects: vec![CredentialRef::new(project_id.as_str()).unwrap()],
                        roles: vec![CredentialRef::new("project_leader").unwrap()],
                        fallback_allowed: false,
                        provider_quota_policy: ProviderQuotaPolicy::BoundedAttempt,
                    },
                },
            },
        )
        .await;
        let record = controller
            .read(OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Connections,
                id: account.id.clone(),
            })
            .await
            .unwrap();
        let OwnerData::ModelAccessRecord {
            source_hash,
            record: ModelAccessRecord::Connection { .. },
            ..
        } = record.data
        else {
            panic!("the pooled account must be a config connection")
        };
        let configured = owner
            .configure_account_profile(
                AccountProfileRequest {
                    request_id: RequestId::new(format!("configure-{name}")).unwrap(),
                    scope: NativeAuthScope {
                        expected_database_instance: identity.database_instance().to_owned(),
                        connection: account.id.clone(),
                        expected_config_id: record.config_id,
                        expected_source_hash: source_hash,
                    },
                },
                None,
            )
            .await
            .unwrap();
        assert!(
            matches!(
                configured.outcome,
                AccountProfileOutcome::ConfiguredOnly { .. }
            ),
            "{configured:?}"
        );
        let open = crazyagent::tui::default_master_open(&mut controller, &project_id)
            .await
            .unwrap();
        apply(
            &mut controller,
            &mut generation,
            OwnerMutation::ProjectMaster { action: open },
        )
        .await;
        let state = controller
            .read(OwnerRead::ProjectMaster {
                project_id: project_id.clone(),
            })
            .await
            .unwrap();
        let OwnerData::ProjectMaster {
            session: Some(session),
        } = state.data
        else {
            panic!("open master session required")
        };
        let session = *session;
        apply(
            &mut controller,
            &mut generation,
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Submit {
                    project_id: project_id.clone(),
                    session_id: session.session_id.clone(),
                    text: "Say hi in five words.".into(),
                },
            },
        )
        .await;
        let turn = finished_turn(&mut controller, &project_id, &session.session_id).await;
        let gated = seen.lock().unwrap().clone();
        assert_eq!(
            turn.phase,
            MasterTurnPhase::Succeeded,
            "{name}: key gate saw {gated:?} (true = saved key present); turn {:?} reason {:?}",
            turn.phase,
            turn.reason
        );
        let reply = turn.reply.expect("a real model reply");
        assert!(!reply.text.trim().is_empty(), "{name}: empty reply");
    }
    let gated = seen.lock().unwrap().clone();
    assert!(
        !gated.is_empty() && gated.iter().all(|allowed| *allowed),
        "every request through the key gate must carry the saved key: {gated:?}"
    );
    drop(controller);
    drop(owner);
    engine.stop();
    gate.abort();

    for held in holders(&root, secret.as_bytes()) {
        let relative = held.strip_prefix(&root).unwrap();
        assert!(
            held.parent().unwrap().ends_with("account-keys")
                || relative.ends_with(".backboard/keys.json"),
            "the key may live only in its key file and the harness's private key store: {relative:?}"
        );
    }
}
