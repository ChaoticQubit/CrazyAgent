use super::super::{ActionOutcome, NativeBridge};
use crazyagent::client_controller::OwnerController;
use crazyagent::config::ModelAccessEdit;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, NativeAuthMethod, OwnerClientKind, OwnerData, OwnerMutation, OwnerRead,
    OwnerResponse, OwnerResult,
};
use crazyagent::protocol::{ConfigId, CredentialRef};
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
