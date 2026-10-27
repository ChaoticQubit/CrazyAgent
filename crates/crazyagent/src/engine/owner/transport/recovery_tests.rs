use super::*;
use crate::engine::bootstrap::EngineService;
use std::os::unix::fs::PermissionsExt;

const OWNER_WAIT_MS: u64 = 5000;
const IN_FLIGHT: Duration = Duration::from_millis(300);
const FILL_WRITES_MAX: usize = 4096;

/// A real engine with an owner client and the engine root its action ledger lives in.
struct Running {
    root: tempfile::TempDir,
    service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
    client: OwnerClient,
    engine_root: PathBuf,
}

impl Running {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-recovery-")
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
        let client =
            OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(20))
                .await
                .unwrap()
                .0;
        let engine_root =
            super::changes::engine_root(&service.owner_runtime().unwrap().authority).unwrap();
        Self {
            root,
            service,
            _stop: stop,
            client,
            engine_root,
        }
    }

    fn runtime(&self) -> &OwnerRuntimeHandle {
        self.service.owner_runtime().unwrap()
    }

    /// The handshake packet the owner client sends. Output: the packet.
    fn packet(&self) -> [u8; 37] {
        let mut packet = [0; 37];
        packet[..4].copy_from_slice(b"CAO1");
        packet[5..].copy_from_slice(&self.client.token);
        packet
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

/// Records one agent start whose outcome is unknown, as a notice the owner may dismiss.
/// Inputs: the ledger and the team agent. Output: the notice's id.
fn notice(ledger: &rusqlite::Connection, team_agent: u32) -> ledger::ActionId {
    ledger::begin(
        ledger,
        ActionKind::AgentSpawn,
        &format!("team-{team_agent}"),
        &format!("{{\"team_agent\":{team_agent},\"pane\":false,\"unknown\":true}}"),
    )
    .unwrap()
}

fn root() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    (directory, root)
}

/// Sends one dismiss through this protocol's request parsing and handler, as an owner
/// client's frame body. Inputs: the engine root, the kind and number. Output: Ok once
/// dismissed, or the refusal code.
fn dismiss(root: &Path, kind: ActionKind, id: i64) -> Result<(), EngineError> {
    let body = serde_json::json!({
        "recovery": {"kind": "dismiss", "action_kind": kind, "id": id}
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    assert!(matches!(protocol(&bytes).unwrap(), Protocol::Recovery));
    let reply = run(root, parse(&bytes)?)?;
    assert!(matches!(reply, Reply::Dismissed));
    Ok(())
}

#[test]
fn the_owner_dismisses_a_notice_once_and_never_an_intent_still_checked() {
    let (_directory, root) = root();
    let ledger = ledger::open(&root).unwrap();
    let checked = ledger::begin(
        &ledger,
        ActionKind::AgentSpawn,
        "team-3",
        "{\"team_agent\":3,\"pane\":true}",
    )
    .unwrap();
    let notice = ledger::begin(
        &ledger,
        ActionKind::AgentSpawn,
        "team-4",
        "{\"team_agent\":4,\"pane\":false,\"unknown\":true}",
    )
    .unwrap();
    let accept = ledger::begin(
        &ledger,
        ActionKind::LandingAccept,
        "set-1",
        "{\"project\":\"p\",\"change_set\":\"set-1\",\"landed\":false}",
    )
    .unwrap();

    let refused = [
        dismiss(&root, ActionKind::AgentSpawn, checked.get())
            .unwrap_err()
            .0,
        dismiss(&root, ActionKind::WorktreeCreate, notice.get())
            .unwrap_err()
            .0,
    ];
    assert_eq!(refused, ["ledger_not_notice", "ledger_missing"]);
    dismiss(&root, ActionKind::AgentSpawn, notice.get()).unwrap();
    dismiss(&root, ActionKind::LandingAccept, accept.get()).unwrap();
    let again = dismiss(&root, ActionKind::AgentSpawn, notice.get())
        .unwrap_err()
        .0;
    assert_eq!(again, "ledger_finished");

    let left = ledger::pending(&ledger, ActionKind::AgentSpawn).unwrap();
    assert_eq!(
        left.iter().map(|action| action.id).collect::<Vec<_>>(),
        vec![checked]
    );
    assert!(
        ledger::pending(&ledger, ActionKind::LandingAccept)
            .unwrap()
            .is_empty()
    );
    let outcome: String = ledger
        .query_row(
            "SELECT outcome FROM ledger_actions WHERE id=?1",
            [accept.get()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "failed");
    for code in refused.iter().chain([&again]) {
        assert!(RECOVERY_CODES.contains(&code.as_str()), "{code}");
    }
}

#[test]
fn malformed_or_oversized_recovery_requests_are_refused_before_the_ledger() {
    for body in [
        r#"{"recovery":{"kind":"dismiss","action_kind":"agent_spawn"}}"#,
        r#"{"recovery":{"kind":"dismiss","action_kind":"spawn","id":1}}"#,
        r#"{"recovery":{"kind":"dismiss","action_kind":"agent_spawn","id":1,"outcome":"done"}}"#,
        r#"{"recovery":{"kind":"retry","id":1}}"#,
        r#"{"recovery":{"kind":"dismiss","action_kind":"agent_spawn","id":"1"}}"#,
    ] {
        assert_eq!(
            parse(body.as_bytes()).unwrap_err().0,
            "recovery_request_invalid",
            "{body}"
        );
    }
    let long = format!(
        r#"{{"recovery":{{"kind":"dismiss","action_kind":"agent_spawn","id":1}},"pad":"{}"}}"#,
        "x".repeat(REQUEST_BYTES_MAX)
    );
    assert_eq!(
        parse(long.as_bytes()).unwrap_err().0,
        "recovery_request_bounds"
    );
}

#[test]
fn recovery_codes_cover_every_refusal_of_the_protocol_and_stay_sorted() {
    let source = include_str!("recovery.rs");
    let found: Vec<&str> = ["failure(\"", "code: \""]
        .into_iter()
        .flat_map(|opening| source.split(opening).skip(1))
        .filter_map(|rest| rest.split('"').next())
        .collect();
    assert!(found.len() >= 7, "{found:?}");
    for code in found {
        assert!(
            RECOVERY_CODES.contains(&code),
            "{code} is not in RECOVERY_CODES"
        );
    }
    let mut sorted = RECOVERY_CODES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, RECOVERY_CODES);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dismissal_holds_the_owner_barrier_until_it_commits_and_a_revoked_client_dismisses_none()
{
    let running = Running::start().await;
    let held = ledger::open(&running.engine_root).unwrap();
    let first = notice(&held, 4);
    let second = notice(&held, 5);
    held.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut dismissal = Box::pin(
        running
            .client
            .recovery_dismiss(ActionKind::AgentSpawn, first.get()),
    );
    assert!(
        tokio::time::timeout(IN_FLIGHT, &mut dismissal)
            .await
            .is_err(),
        "the dismissal waits behind the held write"
    );
    assert!(
        running.runtime().engine.owner_egress.try_write().is_err(),
        "the dismissal holds the owner barrier while it waits to commit"
    );
    held.execute_batch("ROLLBACK").unwrap();
    dismissal
        .await
        .unwrap_or_else(|error| panic!("the held dismissal answered {}", error.0));

    let runtime = running.runtime();
    let revoked = runtime
        .control(OwnerControl {
            operation_id: RequestId::new("revoke-before-recovery").unwrap(),
            target: OwnerClientKind::Tui,
            expected_epoch: runtime.epoch(OwnerClientKind::Tui).unwrap(),
            decision: OwnerDecision::Revoke,
        })
        .await
        .unwrap();
    assert!(!revoked.active);
    assert!(
        running
            .client
            .recovery_dismiss(ActionKind::AgentSpawn, second.get())
            .await
            .is_err(),
        "a revoked client dismisses nothing"
    );
    let left = ledger::pending(&held, ActionKind::AgentSpawn).unwrap();
    assert_eq!(
        left.iter().map(|action| action.id).collect::<Vec<_>>(),
        vec![second]
    );
    drop(held);
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_stops_reading_holds_the_owner_barrier_no_longer_than_the_reply_bound() {
    let running = Running::start().await;
    let ledger = ledger::open(&running.engine_root).unwrap();
    let shown = notice(&ledger, 4);
    let runtime = running.runtime();
    let packet = running.packet();
    let admission = runtime.authority.authenticate(&packet).unwrap();
    let (mut server, client) = crate::spawn_gate::unix_pair_std().unwrap();
    for end in [&server, &client] {
        end.set_nonblocking(true).unwrap();
    }
    nix::sys::socket::setsockopt(&server, nix::sys::socket::sockopt::SndBuf, &1024usize).unwrap();
    nix::sys::socket::setsockopt(&client, nix::sys::socket::sockopt::RcvBuf, &1024usize).unwrap();
    let mut writes = 0;
    for chunk in [&[0; 1024][..], &[0; 1][..]] {
        loop {
            match std::io::Write::write(&mut server, chunk) {
                Ok(_) => writes += 1,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("filling the socket failed: {error}"),
            }
            assert!(writes < FILL_WRITES_MAX, "the socket never filled");
        }
    }
    assert!(writes > 0, "the client's socket took no bytes");
    let mut server = UnixStream::from_std(server).unwrap();
    let _client = UnixStream::from_std(client).unwrap();
    let body = serde_json::to_vec(&serde_json::json!({
        "recovery": {"kind": "dismiss", "action_kind": "agent_spawn", "id": shown.get()}
    }))
    .unwrap();

    let served = tokio::time::timeout(
        REPLY_WRITE_TIME + Duration::from_secs(5),
        serve(
            &mut server,
            &runtime.engine,
            &runtime.authority,
            &packet,
            &admission,
            &body,
        ),
    )
    .await
    .expect("a client that stops reading is dropped once the reply bound passes");
    assert_eq!(served.unwrap_err().0, "recovery_outcome_unknown");
    assert!(
        runtime.engine.owner_egress.try_write().is_ok(),
        "the owner barrier is free once the stalled reply is dropped"
    );
    assert!(
        ledger::pending(&ledger, ActionKind::AgentSpawn)
            .unwrap()
            .is_empty(),
        "the dismissal committed before its reply was written"
    );
    drop(server);
    drop(ledger);
    running.stop().await;
}
