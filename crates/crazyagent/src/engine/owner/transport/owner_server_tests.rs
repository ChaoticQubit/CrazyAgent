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
        Self::start_with("").await
    }

    async fn start_with(settings: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-own-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(
            &global,
            format!("version = 1\ntimeout_ms = {OWNER_WAIT_MS}\n{settings}"),
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
async fn an_owner_client_past_the_client_slots_is_refused_plainly_while_the_others_hold_the_server()
{
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

    let mut held = Vec::new();
    for _ in 0..CLIENTS_MAX {
        held.push(admitted(&client).await);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let refused = client
        .request(read_capabilities("while-busy"))
        .await
        .unwrap_or_else(|error| panic!("the owner request past the slots answered {}", error.0));
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

    drop(held);
    served_again(&client, "after-busy").await;
    fixture.stop().await;
}

/// Sends a GitHub token request that says a token follows and never sends it, so the
/// engine serves it past the owner exchange while it waits for the token frame.
/// Input: the owner client. Output: the open connection.
async fn awaiting_a_token(client: &OwnerClient) -> UnixStream {
    let mut stream = admitted(client).await;
    let frame =
        encode_frame(&json!({"github": {"kind": "token"}, "secret_present": true})).unwrap();
    stream.write_all(&frame).await.unwrap();
    stream
}

/// Opens a terminal view that sends its owner packet and no attach request, so the engine
/// holds it as a view while it waits for the request. Input: the owner client. Output: the
/// open connection.
async fn terminal_waiting(client: &OwnerClient) -> UnixStream {
    let mut stream = connected(client).await;
    let mut packet = [0; 37];
    packet[..4].copy_from_slice(terminal::MAGIC);
    packet[5..].copy_from_slice(&client.token);
    stream.write_all(&packet).await.unwrap();
    stream
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_views_up_to_the_connection_bound_are_held_and_the_next_is_refused_by_name() {
    assert_eq!(crate::terminal::CONNECTIONS_MAX, 40);
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let mut held = Vec::new();
    for _ in 0..crate::terminal::CONNECTIONS_MAX {
        held.push(terminal_waiting(&client).await);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut open = 0;
    for stream in &mut held {
        let mut byte = [0; 1];
        if tokio::time::timeout(Duration::from_millis(20), stream.read(&mut byte))
            .await
            .is_err()
        {
            open += 1;
        }
    }
    assert_eq!(
        open,
        crate::terminal::CONNECTIONS_MAX,
        "views 9 to 40 are held like the first 8"
    );

    let mut past = terminal_waiting(&client).await;
    let attach = crate::terminal::TerminalRequest::Attach {
        agent_id: crate::terminal::AgentId::team(1).unwrap(),
    };
    past.write_all(&encode_frame(&attach).unwrap())
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut past))
        .await
        .expect("the view past the bound is answered in time")
        .expect("the view past the bound gets a reply, not a dropped connection");
    assert_eq!(
        serde_json::from_slice::<crate::terminal::TerminalFrame>(&reply[4..]).unwrap(),
        crate::terminal::TerminalFrame::Rejected {
            code: "terminal_viewers_full".into()
        }
    );

    drop(held);
    served_again(&client, "after-views").await;
    fixture.stop().await;
}

const _: () = assert!(DEFERRED_MAX < CLIENTS_MAX);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_requests_served_past_their_exchange_leave_the_client_slots_to_others() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let mut waiting = Vec::new();
    for _ in 0..DEFERRED_MAX {
        waiting.push(awaiting_a_token(&client).await);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    for _ in 1..CLIENTS_MAX {
        waiting.push(admitted(&client).await);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let served = client
        .request(read_capabilities("while-tokens-are-awaited"))
        .await
        .unwrap();
    let past_the_bound = client.github_status().await;

    drop(waiting);
    served_again(&client, "after-tokens").await;
    fixture.stop().await;
    assert!(
        matches!(served.response, OwnerResponse::Read { .. }),
        "{served:?}"
    );
    assert_eq!(
        past_the_bound.map(|_| ()).unwrap_err().0,
        "owner_busy",
        "at most DEFERRED_MAX requests are served past their exchange at once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_client_past_the_handshake_queue_is_refused_plainly() {
    let fixture = Fixture::start().await;
    let client = fixture.client().await;
    let mut silent = Vec::new();
    for _ in 0..HANDSHAKES_MAX {
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
    #[cfg(target_os = "macos")]
    let _keychain = {
        let mut keychain = crate::credentials::test_keychain::TestKeychain::temporary();
        keychain.serve(&crate::github::namespace(fixture.root.path()));
        keychain
    };
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

const CONFIG_ONLY_ACCOUNT: &str = r#"
[model_access]
accounts = ["claude-account"]
[[model_access.connections]]
id = "claude-connection"
provider = "anthropic"
harness = "claude-code"
protocol = "native"
account_ref = "claude-account"
endpoint = { kind = "native" }
model = { id = "configured-claude-model", author = "anthropic" }
auth = { method = "native_managed", profile_ref = "claude-profile" }
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_filled_by_config_only_accounts_still_leads_to_the_added_ones() {
    let fixture = Fixture::start_with(CONFIG_ONLY_ACCOUNT).await;
    let client = fixture.client().await;
    let closed = crate::spawn_gate::bind_tcp("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let added = client
        .add_account(
            crate::accounts::AccountDraft {
                provider: crate::accounts::Provider::Local,
                method: crate::accounts::SignInMethod::Url,
                name: "Lab box".into(),
                url: Some(format!("http://127.0.0.1:{closed}/v1")),
                model: Some("lab-model".into()),
            },
            None,
        )
        .await
        .unwrap();

    let first = client.list_accounts(None, 1).await.unwrap();
    assert_eq!(
        first
            .accounts
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        ["claude-connection"]
    );
    let next = first
        .next
        .expect("the added account follows the config-only page");
    let second = client.list_accounts(Some(next), 1).await.unwrap();
    assert_eq!(
        second
            .accounts
            .iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>(),
        [added.id]
    );
    assert_eq!(second.next, None);
    fixture.stop().await;
}

/// Runs git in a test checkout with an isolated home and no system config. Inputs: the
/// home, the folder and the arguments. Output: standard output.
fn checkout_git(home: &Path, dir: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_import_move_is_listed_committed_and_refused_again_through_the_owner_socket() {
    let fixture = Fixture::start().await;
    let (home, folder) = (
        fixture.root.path().join("home"),
        fixture.root.path().join("project"),
    );
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&folder).unwrap();
    for args in [
        &["init", "-q", "--template=", "-b", "main"][..],
        &["config", "user.name", "Repo Owner"],
        &["config", "user.email", "repo@example.invalid"],
    ] {
        checkout_git(&home, &folder, args);
    }
    std::fs::write(folder.join("CLAUDE.md"), "SOCKET-RULE\n").unwrap();
    checkout_git(&home, &folder, &["add", "CLAUDE.md"]);
    checkout_git(&home, &folder, &["commit", "-q", "-m", "Start"]);
    let project = crate::protocol::ProjectId::new("import-socket").unwrap();
    let setup = crate::project_setup::StoredSetup {
        project_id: project.clone(),
        name: "Import".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: crate::project_setup::TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 1,
        },
        documents: vec![],
        folder: folder.display().to_string(),
        created_ms: 1,
        revision: 1,
    };
    rusqlite::Connection::open(fixture.root.path().join("engine.db"))
        .unwrap()
        .execute(
            "INSERT INTO project_setups(project, body) VALUES (?1, ?2)",
            [
                project.as_str(),
                serde_json::to_string(&setup).unwrap().as_str(),
            ],
        )
        .unwrap();
    let client = fixture.client().await;
    assert_eq!(
        client
            .uncommitted_instruction_import(project.clone())
            .await
            .unwrap(),
        crate::engine::owner::UncommittedImport {
            files: Vec::new(),
            branch: None,
            checked_out: Some("main".to_owned())
        },
        "the reply names the checked-out branch for the project head while no move waits"
    );
    let moved = client
        .confirm_instruction_import(project.clone())
        .await
        .unwrap();
    assert_eq!(
        moved
            .iter()
            .map(|file| (file.path.as_str(), file.tracked, file.uncommitted))
            .collect::<Vec<_>>(),
        [("CLAUDE.md", true, true)]
    );
    assert_eq!(
        client
            .uncommitted_instruction_import(project.clone())
            .await
            .unwrap(),
        crate::engine::owner::UncommittedImport {
            files: vec!["CLAUDE.md".to_owned()],
            branch: Some("main".to_owned()),
            checked_out: Some("main".to_owned())
        },
        "the reply names the checked-out branch merges into are refused"
    );
    let made = client
        .commit_instruction_import(project.clone())
        .await
        .unwrap();
    assert_eq!(
        (made.branch.as_str(), made.files.as_slice()),
        ("main", ["CLAUDE.md".to_owned()].as_slice())
    );
    assert_eq!(
        checkout_git(&home, &folder, &["rev-parse", "main"]).trim(),
        made.commit
    );
    assert_eq!(checkout_git(&home, &folder, &["status", "--porcelain"]), "");
    assert_eq!(
        client
            .uncommitted_instruction_import(project.clone())
            .await
            .unwrap(),
        crate::engine::owner::UncommittedImport {
            files: Vec::new(),
            branch: None,
            checked_out: Some("main".to_owned())
        },
        "the reply names the checked-out branch for the project head while no move waits"
    );
    assert_eq!(
        client
            .commit_instruction_import(project)
            .await
            .unwrap_err()
            .0,
        "process_commit_none"
    );
    fixture.stop().await;
}
