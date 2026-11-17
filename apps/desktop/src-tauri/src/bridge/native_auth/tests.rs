use super::super::{ActionOutcome, NativeBridge};
use crazyagent::client_controller::OwnerController;
use crazyagent::config::ModelAccessEdit;
use crazyagent::engine::EngineError;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, NativeAuthFact, NativeAuthMethod, NativeAuthPhase, NativeAuthReply,
    NativeProfileBinding, OwnerClientKind, OwnerData, OwnerMutation, OwnerRead, OwnerResponse,
    OwnerResult,
};
use crazyagent::protocol::{ConfigId, CredentialRef, Cursor};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

const CATALOG: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-claude"]
[[model_access.connections]]
id = "b-claude"
provider = "provider-claude"
harness = "claude"
protocol = "native"
account_ref = "account-claude"
endpoint = { kind = "native" }
model = { id = "model-claude", author = "author-claude" }
auth = { method = "native_managed", profile_ref = "profile-claude" }
"#;

struct Case {
    root: tempfile::TempDir,
    service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
}

impl Case {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-stuck-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, CATALOG).unwrap();
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

    /// Leaves a bridge holding a sign-in the engine itself answered as outcome unknown, then
    /// has another owner client remove its connection so the bridge cannot select it again.
    /// A real write lock on engine.db makes the engine's own storage refuse the start; no
    /// provider and no sign-in program take part. Output: the bridge, detached as the
    /// unknown outcome leaves it.
    async fn stuck(&self) -> NativeBridge {
        let root = self.root.path();
        let mut bridge = NativeBridge::new(Some(root.join("e.sock")));
        bridge.connect().await.unwrap();
        let ticket = bridge
            .inspect_model_access(ModelAccessCollection::Connections, "b-claude")
            .await
            .unwrap()
            .ticket;
        let lock = crazyagent::landing::open(root).unwrap();
        lock.execute_batch("BEGIN IMMEDIATE").unwrap();
        let started = bridge
            .start_native_auth(&ticket, NativeAuthMethod::ClaudeOfficial)
            .await;
        lock.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            started.err().map(|error| error.code),
            Some("native_auth_outcome_unknown_read_same_operation")
        );
        let mut other = OwnerController::attach(root, OwnerClientKind::Tui, Duration::from_secs(2))
            .await
            .unwrap();
        let id = CredentialRef::new("b-claude").unwrap();
        let read = other
            .read(OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Connections,
                id: id.clone(),
            })
            .await
            .unwrap();
        let OwnerData::ModelAccessRecord { source_hash, .. } = read.data else {
            panic!("actual connection record required");
        };
        other
            .begin_mutation(
                OwnerMutation::ModelAccess {
                    expected_config_id: read.config_id,
                    expected_source_hash: source_hash,
                    edit: ModelAccessEdit::RemoveConnection { id },
                },
                1,
            )
            .unwrap();
        let removed = other.send_pending().await.unwrap();
        assert!(
            matches!(removed, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied)
        );
        bridge
    }

    async fn stop(mut self) {
        self.service.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn a_stuck_sign_in_blocks_changes_to_its_own_account_only() {
    let case = Case::new().await;
    let mut bridge = case.stuck().await;
    bridge.connect().await.unwrap();
    assert_eq!(
        bridge
            .inspect_model_access(ModelAccessCollection::Connections, "b-claude")
            .await
            .err()
            .map(|error| error.code),
        Some("model_access_record_not_found")
    );
    let ticket = bridge
        .model_access_page(ModelAccessCollection::Accounts, false)
        .await
        .unwrap()
        .ticket;
    let added = bridge
        .edit_model_access(
            &ticket,
            ModelAccessEdit::AddAccount {
                account_ref: CredentialRef::new("account-spare").unwrap(),
            },
            1,
        )
        .await;
    assert_eq!(added.as_ref().err().map(|error| error.code), None);
    assert!(matches!(added.unwrap().outcome, ActionOutcome::Applied));
    let own = bridge
        .rename_account("b-claude", "Renamed".into())
        .await
        .err()
        .map(|error| error.code);
    assert_eq!(own, Some("native_auth_recovery_required"));
    case.stop().await;
}

#[tokio::test]
async fn clear_stuck_sign_in_ends_it_only_once_the_engine_answers() {
    let case = Case::new().await;
    let mut bridge = case.stuck().await;
    assert_eq!(bridge.status().stuck_sign_in.as_deref(), Some("b-claude"));
    assert_eq!(
        bridge
            .clear_native_auth()
            .await
            .err()
            .map(|error| error.code),
        Some("not_connected")
    );
    assert_eq!(bridge.status().stuck_sign_in.as_deref(), Some("b-claude"));
    bridge.connect().await.unwrap();
    assert_eq!(bridge.status().stuck_sign_in.as_deref(), Some("b-claude"));
    assert_eq!(
        bridge.clear_native_auth().await.map_err(|error| error.code),
        Ok(true)
    );
    assert_eq!(bridge.status().stuck_sign_in, None);
    let own = bridge
        .rename_account("b-claude", "Renamed".into())
        .await
        .err()
        .map(|error| error.code);
    assert_eq!(own, Some("account_not_found"));
    case.stop().await;
}

/// A bridge holding a sign-in it believes still runs, whose reads fail because the unknown
/// outcome left its controller detached, and a config that sign-in did not start with.
/// Input: the case. Output: the bridge and the config.
async fn running_unreadable(case: &Case) -> (NativeBridge, ConfigId) {
    let mut bridge = case.stuck().await;
    let operation = bridge.native_auth.as_mut().unwrap();
    operation.running = true;
    operation.uncertain = false;
    (bridge, ConfigId::new("d".repeat(64)).unwrap())
}

#[tokio::test]
async fn a_recheck_whose_read_failed_is_tried_again_at_the_same_config() {
    let case = Case::new().await;
    let (mut bridge, moved) = running_unreadable(&case).await;
    bridge.recheck_auth(&moved).await;
    assert_eq!(bridge.native_auth.as_ref().unwrap().checked, None);
    drop(bridge);
    case.stop().await;
}

#[tokio::test]
async fn a_recheck_reads_the_engine_once_per_config() {
    let case = Case::new().await;
    let (mut bridge, moved) = running_unreadable(&case).await;
    bridge.native_auth.as_mut().unwrap().checked = Some(moved.clone());
    let request = |bridge: &NativeBridge| bridge.native_auth.as_ref().unwrap().request_id.clone();
    let before = request(&bridge);
    bridge.recheck_auth(&moved).await;
    assert_eq!(request(&bridge), before);
    bridge
        .recheck_auth(&ConfigId::new("e".repeat(64)).unwrap())
        .await;
    assert_ne!(request(&bridge), before);
    drop(bridge);
    case.stop().await;
}

/// PROTO-3-LOWS (rv-code r1 L1): a busy engine admitted no request, so the outcome is known.
/// A sign-in the bridge holds as running stays running and certain, and the connection
/// stays, so the next recheck retries instead of the sign-in turning stuck.
#[tokio::test]
async fn a_busy_engine_leaves_a_running_sign_in_running_and_connected() {
    let case = Case::new().await;
    let mut bridge = case.stuck().await;
    bridge.connect().await.unwrap();
    let operation = bridge.native_auth.as_mut().unwrap();
    operation.running = true;
    operation.uncertain = false;
    let busy = crazyagent::engine::EngineError("owner_busy".to_owned());
    assert_eq!(
        bridge.settle_auth(Err(busy)).map_err(|error| error.code),
        Err("owner_busy")
    );
    let operation = bridge.native_auth.as_ref().unwrap();
    assert!(operation.running, "a busy engine ended the held sign-in");
    assert!(
        !operation.uncertain,
        "a busy engine made the sign-in's outcome unknown"
    );
    assert!(
        bridge.status().connected,
        "a busy engine dropped the connection"
    );
    drop(bridge);
    case.stop().await;
}

/// A connected bridge holding a sign-in it last read as running and certain, as a recheck
/// finds one. Input: the case. Output: the bridge.
async fn held_running(case: &Case) -> NativeBridge {
    let mut bridge = case.stuck().await;
    bridge.connect().await.unwrap();
    let operation = bridge.native_auth.as_mut().unwrap();
    operation.running = true;
    operation.uncertain = false;
    assert!(bridge.stuck_auth().is_none());
    bridge
}

/// An answer about the bridge's held sign-in, in the given phase, that matches it in every
/// field the bridge checks. Inputs: the bridge and the phase. Output: the reply.
fn matching(bridge: &NativeBridge, phase: NativeAuthPhase) -> NativeAuthReply {
    let operation = bridge.native_auth.as_ref().unwrap();
    let identity = bridge.controller.as_ref().unwrap().identity();
    let scope = operation.original.scope();
    NativeAuthReply {
        request_id: operation.request_id.clone(),
        database_instance: identity.database_instance().to_owned(),
        engine_epoch: identity.engine_epoch().to_owned(),
        fact: NativeAuthFact {
            operation_id: operation.original.id().clone(),
            action: operation.original.action(),
            binding: NativeProfileBinding {
                database_instance: scope.expected_database_instance.clone(),
                engine_epoch: identity.engine_epoch().to_owned(),
                config_id: scope.expected_config_id.clone(),
                source_hash: scope.expected_source_hash.clone(),
                at_revision: Cursor::new(1),
                connection: operation.connection.clone(),
                custody_id: "custody-row".into(),
            },
            generation: operation.generation.unwrap_or(1),
            phase,
            code: None,
        },
        challenge: None,
        claude_challenge: None,
        account: None,
    }
}

const ENDED: [NativeAuthPhase; 6] = [
    NativeAuthPhase::AuthenticatedObserved,
    NativeAuthPhase::UnauthenticatedObserved,
    NativeAuthPhase::Failed,
    NativeAuthPhase::Cancelled,
    NativeAuthPhase::Unavailable,
    NativeAuthPhase::Uncertain,
];

/// PROTO-3-SEC F2: on the recheck route, an ended answer the bridge cannot match, here one
/// bound to settings the sign-in did not start with, is refused as an unknown outcome and
/// leaves the sign-in stuck, as any other answer that fails the check does, instead of
/// holding it as running.
#[tokio::test]
async fn a_recheck_never_keeps_an_ended_answer_that_fails_the_check() {
    let case = Case::new().await;
    let mut bridge = held_running(&case).await;
    let mut other = matching(&bridge, NativeAuthPhase::AuthenticatedObserved);
    other.fact.binding.config_id = ConfigId::new("e".repeat(64)).unwrap();
    assert_eq!(
        bridge
            .running_answer(Ok(other), true)
            .map_err(|error| error.code),
        Err("native_auth_outcome_unknown_read_same_operation")
    );
    assert_eq!(
        bridge.status().stuck_sign_in.as_deref(),
        Some("b-claude"),
        "an ended answer the bridge cannot match leaves the sign-in stuck"
    );
    drop(bridge);
    case.stop().await;
}

/// PROTO-3-SEC F1: on the recheck route a matching answer in every ended phase, finished,
/// failed and cancelled alike, keeps the sign-in held and running for the drawer's own read,
/// and the next read that settles drops it.
#[tokio::test]
async fn a_recheck_keeps_each_matching_end_until_a_settling_read() {
    let case = Case::new().await;
    let mut bridge = held_running(&case).await;
    for phase in ENDED {
        assert!(!phase.unfinished());
        let reply = matching(&bridge, phase);
        let kept = bridge.running_answer(Ok(reply), true);
        assert_eq!(kept.map_err(|error| error.code), Ok(false), "{phase:?}");
        let operation = bridge.native_auth.as_ref().unwrap();
        assert!(operation.unresolved && operation.running, "{phase:?}");
        assert_eq!(bridge.stuck_auth(), None, "{phase:?}");
    }
    let reply = matching(&bridge, NativeAuthPhase::Failed);
    let settled = bridge.running_answer(Ok(reply), false);
    assert_eq!(settled.map_err(|error| error.code), Ok(true));
    assert!(bridge.native_auth.is_none());
    drop(bridge);
    case.stop().await;
}

/// PROTO-3-SEC F1: on the recheck route a refusal that ends the sign-in (`ends_it`'s
/// refusal arm) keeps it held and running for the drawer's own read, and the next read that
/// settles drops it.
#[tokio::test]
async fn a_recheck_keeps_a_sign_in_the_engine_refused_as_ended_until_a_settling_read() {
    let case = Case::new().await;
    let mut bridge = held_running(&case).await;
    let ended = || -> Result<NativeAuthReply, EngineError> {
        Err(EngineError("native_auth_not_active".to_owned()))
    };
    assert!(crazyagent::engine::owner::ends_operation(
        "native_auth_not_active"
    ));
    let kept = bridge.running_answer(ended(), true);
    assert_eq!(kept.map_err(|error| error.code), Ok(false));
    let operation = bridge.native_auth.as_ref().unwrap();
    assert!(operation.unresolved && operation.running);
    let settled = bridge.running_answer(ended(), false);
    assert_eq!(settled.map_err(|error| error.code), Ok(true));
    assert!(bridge.native_auth.is_none());
    drop(bridge);
    case.stop().await;
}
