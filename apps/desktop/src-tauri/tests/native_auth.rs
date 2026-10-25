use crazyagent::config::ModelAccessEdit;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, ModelAccessRecord, NativeAuthAction, NativeAuthMethod, NativeAuthPhase,
    NativeAuthQuery, NativeAuthReply, NativeAuthScope, OwnerClient, OwnerClientKind, OwnerControl,
    OwnerDecision,
};
use crazyagent::model_gateway::{Health, Quota};
use crazyagent::protocol::{ConfigId, CredentialRef, RequestId};
use crazyagent_desktop::bridge::{ActionOutcome, ModelAccessDetail, NativeBridge};
use serde_json::Value;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

const CATALOG: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-a", "account-z", "account-api"]
[[model_access.connections]]
id = "connection-a"
provider = "provider-a"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "model-a", author = "author-a" }
auth = { method = "native_managed", profile_ref = "credential-profile-a" }
[[model_access.connections]]
id = "connection-z"
provider = "provider-z"
harness = "claude"
protocol = "native"
account_ref = "account-z"
endpoint = { kind = "native" }
model = { id = "model-z", author = "author-z" }
auth = { method = "native_managed", profile_ref = "credential-profile-z" }
[[model_access.connections]]
id = "connection-api"
provider = "provider-api"
harness = "rcli"
protocol = "openai"
account_ref = "account-api"
endpoint = { kind = "http", url = "https://api.example.invalid/v1" }
model = { id = "model-api", author = "author-api" }
auth = { method = "api_key", credential_ref = "stored-key-reference" }
[[model_access.pools]]
id = "pool"
members = ["connection-a", "connection-z", "connection-api"]
projects = ["project-fixture"]
roles = ["leader"]
fallback_allowed = false
"#;

struct Fixture {
    root: tempfile::TempDir,
    service: Mutex<Option<EngineService>>,
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn request_id(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-dna-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.path().join("s");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, CATALOG).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let fixture = Self {
            root,
            service: Mutex::new(None),
        };
        *fixture.service.lock().await = Some(fixture.start().await);
        fixture
    }

    async fn start(&self) -> EngineService {
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        tokio::time::timeout(
            Duration::from_secs(5),
            EngineService::start_with_settings_sources(
                &self.root.path().join("s"),
                "e.sock",
                &self.root.path().join("global.toml"),
                &self.root.path().join("project.toml"),
                &stopped,
            ),
        )
        .await
        .unwrap()
        .unwrap()
    }

    async fn bridge(&self) -> NativeBridge {
        let mut bridge = NativeBridge::new(Some(self.root.path().join("s/e.sock")));
        assert!(bridge.connect().await.unwrap().connected);
        bridge
    }

    async fn stop(&self) {
        if let Some(mut service) = self.service.lock().await.take() {
            tokio::time::timeout(Duration::from_secs(5), service.shutdown())
                .await
                .unwrap()
                .unwrap();
        }
    }

    async fn replace_owner(&self) {
        self.stop().await;
        std::fs::rename(self.root.path().join("s"), self.root.path().join("retired")).unwrap();
        let state = self.root.path().join("s");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        *self.service.lock().await = Some(self.start().await);
    }

    async fn control(&self, decision: OwnerDecision) {
        let service = self.service.lock().await;
        let runtime = service.as_ref().unwrap().owner_runtime().unwrap();
        let epoch = runtime.epoch(OwnerClientKind::Desktop).unwrap();
        let result = runtime
            .control(OwnerControl {
                operation_id: request_id(&format!("desktop-grant-{epoch}")),
                target: OwnerClientKind::Desktop,
                expected_epoch: epoch,
                decision,
            })
            .await
            .unwrap();
        assert_eq!(result.active, decision == OwnerDecision::Reactivate);
        assert!(result.owner_epoch > epoch);
    }

    async fn owner_fact(&self, view: &Value) -> NativeAuthReply {
        let (client, identity) = OwnerClient::discover(
            &self.root.path().join("s"),
            OwnerClientKind::Desktop,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(view["database_instance"], identity.database_instance());
        assert_eq!(view["engine_epoch"], identity.engine_epoch());
        client
            .read_native_auth(NativeAuthQuery {
                request_id: request_id("independent-desktop-observation"),
                scope: scope(view),
                operation_id: request_id(view["operation_id"].as_str().unwrap()),
            })
            .await
            .unwrap()
    }

    async fn assert_gateway_unknown(&self, connection: &str) {
        let service = self.service.lock().await;
        let state = service
            .as_ref()
            .unwrap()
            .owner_runtime()
            .unwrap()
            .model_connection_state(&reference(connection))
            .await
            .unwrap();
        assert_eq!(state.health, Health::Unknown);
        assert_eq!(state.quota, Quota::Unknown);
        assert!(state.observed_at_ms.is_none());
        assert!(state.capabilities.is_empty());
    }
}

async fn run_case<F, T>(body: F)
where
    F: FnOnce(Arc<Fixture>) -> T,
    T: Future<Output = ()> + Send + 'static,
{
    let fixture = Arc::new(Fixture::new().await);
    let mut task = tokio::spawn(body(Arc::clone(&fixture)));
    let result = tokio::time::timeout(Duration::from_secs(20), &mut task).await;
    if result.is_err() {
        task.abort();
        let _ = task.await;
    }
    fixture.stop().await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => std::panic::resume_unwind(error.into_panic()),
        Err(_) => panic!("desktop native-auth case exceeded 20 seconds"),
    }
    assert!(fixture.root.path().join("s/engine.db").is_file());
}

fn scope(view: &Value) -> NativeAuthScope {
    NativeAuthScope {
        expected_database_instance: view["database_instance"].as_str().unwrap().into(),
        connection: reference(view["connection"].as_str().unwrap()),
        expected_config_id: ConfigId::new(view["config_id"].as_str().unwrap()).unwrap(),
        expected_source_hash: view["source_hash"].as_str().unwrap().into(),
    }
}

async fn inspect(bridge: &mut NativeBridge, id: &str) -> ModelAccessDetail {
    bridge
        .inspect_model_access(ModelAccessCollection::Connections, id)
        .await
        .unwrap()
}

async fn assert_actual_unavailable(
    fixture: &Fixture,
    detail: &ModelAccessDetail,
    view: &Value,
    action: NativeAuthAction,
) {
    let actual = fixture.owner_fact(view).await;
    assert_eq!(view["ticket"], detail.ticket);
    assert_eq!(view["connection"], detail.record.id().as_str());
    assert_eq!(view["config_id"], detail.config_id);
    assert_eq!(view["source_hash"], detail.source_hash);
    assert_eq!(view["operation_id"], actual.fact.operation_id.as_str());
    assert_eq!(view["action"], serde_json::to_value(&action).unwrap());
    assert_eq!(actual.fact.action, action);
    assert_eq!(view["generation"], actual.fact.generation.to_string());
    assert_eq!(actual.fact.phase, NativeAuthPhase::Unavailable);
    assert_eq!(view["phase"], "unavailable");
    assert_eq!(view["code"], "native_auth_host_unavailable");
    assert_eq!(view["challenge"], Value::Null);
    assert_eq!(view["account"], Value::Null);
    assert_eq!(
        actual.fact.binding.database_instance,
        actual.database_instance
    );
    assert_eq!(actual.fact.binding.engine_epoch, actual.engine_epoch);
    assert_eq!(actual.fact.binding.config_id.as_str(), detail.config_id);
    assert_eq!(actual.fact.binding.source_hash, detail.source_hash);
    let ModelAccessRecord::Connection { connection } = &detail.record else {
        panic!("actual inspected connection required");
    };
    assert_eq!(&actual.fact.binding.connection, connection);
    assert!(actual.fact.generation > 0);
    assert!(actual.challenge.is_none());
    assert!(actual.account.is_none());
    fixture
        .assert_gateway_unknown(detail.record.id().as_str())
        .await;
}

#[tokio::test]
async fn desktop_native_auth_unconfigured_and_disconnected_fail_closed() {
    let mut bridge = NativeBridge::new(None);
    assert_eq!(bridge.connect().await.err().unwrap().code, "not_configured");
    assert_eq!(
        bridge
            .start_native_auth("1", NativeAuthMethod::CodexBrowser)
            .await
            .err()
            .unwrap()
            .code,
        "not_connected"
    );
    assert_eq!(
        bridge.read_native_auth("1").await.err().unwrap().code,
        "not_connected"
    );
    assert_eq!(
        bridge.cancel_native_auth("1").await.err().unwrap().code,
        "not_connected"
    );
    assert_eq!(
        bridge.logout_native_auth("1").await.err().unwrap().code,
        "not_connected"
    );
    assert!(!bridge.status().connected);
    assert!(!bridge.status().pending);
}

#[tokio::test]
async fn desktop_native_auth_real_owner_reports_unavailable_for_each_official_method() {
    run_case(|fixture| async move {
        let mut bridge = fixture.bridge().await;
        let before = std::fs::read(fixture.root.path().join("global.toml")).unwrap();
        let mut operations = Vec::new();
        for (connection, method) in [
            ("connection-a", NativeAuthMethod::CodexBrowser),
            ("connection-a", NativeAuthMethod::CodexDeviceCode),
            ("connection-z", NativeAuthMethod::ClaudeOfficial),
        ] {
            let detail = inspect(&mut bridge, connection).await;
            let view = serde_json::to_value(
                bridge
                    .start_native_auth(&detail.ticket, method)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(view["request_id"], view["operation_id"]);
            assert!(!operations.contains(&view["operation_id"]));
            assert_actual_unavailable(
                &fixture,
                &detail,
                &view,
                NativeAuthAction::SignIn { method },
            )
            .await;
            operations.push(view["operation_id"].clone());
            assert!(bridge.status().connected);
            assert!(!bridge.status().pending);
            assert!(bridge.pending_intent().unwrap().is_none());
        }
        assert_eq!(
            std::fs::read(fixture.root.path().join("global.toml")).unwrap(),
            before
        );
        assert!(!fixture.root.path().join("project.toml").exists());
    })
    .await;
}

#[tokio::test]
async fn desktop_native_auth_read_cancel_logout_preserve_actual_operation_identity() {
    run_case(|fixture| async move {
        let mut bridge = fixture.bridge().await;
        let detail = inspect(&mut bridge, "connection-a").await;
        assert_eq!(
            bridge
                .read_native_auth(&detail.ticket)
                .await
                .err()
                .unwrap()
                .code,
            "native_auth_operation_missing"
        );
        let original = serde_json::to_value(
            bridge
                .start_native_auth(&detail.ticket, NativeAuthMethod::CodexBrowser)
                .await
                .unwrap(),
        )
        .unwrap();
        let fact = fixture.owner_fact(&original).await.fact;
        let read =
            serde_json::to_value(bridge.read_native_auth(&detail.ticket).await.unwrap()).unwrap();
        assert_eq!(read["operation_id"], original["operation_id"]);
        assert_ne!(read["request_id"], original["request_id"]);
        assert_eq!(fixture.owner_fact(&read).await.fact, fact);
        assert_eq!(
            bridge
                .cancel_native_auth(&detail.ticket)
                .await
                .err()
                .unwrap()
                .code,
            "native_auth_not_active"
        );
        assert!(bridge.status().connected);
        let after =
            serde_json::to_value(bridge.read_native_auth(&detail.ticket).await.unwrap()).unwrap();
        assert_eq!(after["operation_id"], original["operation_id"]);
        assert_eq!(fixture.owner_fact(&after).await.fact, fact);
        let logout =
            serde_json::to_value(bridge.logout_native_auth(&detail.ticket).await.unwrap()).unwrap();
        assert_ne!(logout["operation_id"], original["operation_id"]);
        assert_eq!(logout["request_id"], logout["operation_id"]);
        assert_actual_unavailable(&fixture, &detail, &logout, NativeAuthAction::Logout).await;
        let read_logout =
            serde_json::to_value(bridge.read_native_auth(&detail.ticket).await.unwrap()).unwrap();
        assert_eq!(read_logout["operation_id"], logout["operation_id"]);
        assert_eq!(fixture.owner_fact(&original).await.fact, fact);
        assert!(!bridge.status().pending);
    })
    .await;
}

#[tokio::test]
async fn desktop_native_auth_requires_supported_inspected_connection() {
    run_case(|fixture| async move {
        let mut bridge = fixture.bridge().await;
        let page = bridge
            .model_access_page(ModelAccessCollection::Connections, false)
            .await
            .unwrap();
        assert_eq!(
            bridge
                .start_native_auth(&page.ticket, NativeAuthMethod::CodexBrowser)
                .await
                .err()
                .unwrap()
                .code,
            "stale_model_access"
        );
        for (collection, id) in [
            (ModelAccessCollection::Accounts, "account-a"),
            (ModelAccessCollection::Pools, "pool"),
        ] {
            let detail = bridge.inspect_model_access(collection, id).await.unwrap();
            assert_eq!(
                bridge
                    .start_native_auth(&detail.ticket, NativeAuthMethod::CodexBrowser)
                    .await
                    .err()
                    .unwrap()
                    .code,
                "stale_model_access"
            );
        }
        let api = inspect(&mut bridge, "connection-api").await;
        assert_eq!(
            bridge
                .start_native_auth(&api.ticket, NativeAuthMethod::CodexBrowser)
                .await
                .err()
                .unwrap()
                .code,
            "native_profile_not_managed"
        );
        assert_eq!(
            bridge
                .logout_native_auth(&api.ticket)
                .await
                .err()
                .unwrap()
                .code,
            "native_profile_not_managed"
        );
        let codex = inspect(&mut bridge, "connection-a").await;
        assert_eq!(
            bridge
                .start_native_auth(&codex.ticket, NativeAuthMethod::ClaudeOfficial)
                .await
                .err()
                .unwrap()
                .code,
            "native_auth_method_mismatch"
        );
        let claude = inspect(&mut bridge, "connection-z").await;
        assert_eq!(
            bridge
                .start_native_auth(&claude.ticket, NativeAuthMethod::CodexDeviceCode)
                .await
                .err()
                .unwrap()
                .code,
            "native_auth_method_mismatch"
        );
        assert!(!fixture.root.path().join("s/native-profiles").exists());
        assert!(bridge.pending_intent().unwrap().is_none());
        fixture.assert_gateway_unknown("connection-api").await;
    })
    .await;
}

#[tokio::test]
async fn desktop_native_auth_selection_and_reconnect_do_not_retarget_original_operation() {
    run_case(|fixture| async move {
        let mut bridge = fixture.bridge().await;
        let old = inspect(&mut bridge, "connection-a").await;
        let current = inspect(&mut bridge, "connection-a").await;
        assert_ne!(old.ticket, current.ticket);
        assert_eq!(
            bridge
                .start_native_auth(&old.ticket, NativeAuthMethod::CodexBrowser)
                .await
                .err()
                .unwrap()
                .code,
            "stale_model_access"
        );
        let original = serde_json::to_value(
            bridge
                .start_native_auth(&current.ticket, NativeAuthMethod::CodexBrowser)
                .await
                .unwrap(),
        )
        .unwrap();
        let fact = fixture.owner_fact(&original).await.fact;
        let other = inspect(&mut bridge, "connection-z").await;
        assert_eq!(
            bridge
                .read_native_auth(&current.ticket)
                .await
                .err()
                .unwrap()
                .code,
            "stale_model_access"
        );
        assert_eq!(
            bridge
                .read_native_auth(&other.ticket)
                .await
                .err()
                .unwrap()
                .code,
            "native_auth_operation_scope_conflict"
        );
        assert_eq!(
            bridge
                .cancel_native_auth(&other.ticket)
                .await
                .err()
                .unwrap()
                .code,
            "native_auth_operation_scope_conflict"
        );
        bridge.connect().await.unwrap();
        assert_eq!(
            bridge
                .start_native_auth(&current.ticket, NativeAuthMethod::CodexBrowser)
                .await
                .err()
                .unwrap()
                .code,
            "stale_model_access"
        );
        let refreshed = inspect(&mut bridge, "connection-a").await;
        let read = serde_json::to_value(bridge.read_native_auth(&refreshed.ticket).await.unwrap())
            .unwrap();
        assert_eq!(read["operation_id"], original["operation_id"]);
        assert_eq!(read["ticket"], refreshed.ticket);
        assert_eq!(fixture.owner_fact(&read).await.fact, fact);
        assert!(bridge.pending_intent().unwrap().is_none());
    })
    .await;
}

#[tokio::test]
async fn desktop_native_auth_source_conflict_preserves_original_operation() {
    run_case(|fixture| async move {
        let mut bridge = fixture.bridge().await;
        let detail = inspect(&mut bridge, "connection-a").await;
        let original = serde_json::to_value(
            bridge
                .start_native_auth(&detail.ticket, NativeAuthMethod::CodexBrowser)
                .await
                .unwrap(),
        )
        .unwrap();
        let fact = fixture.owner_fact(&original).await.fact;
        let global = fixture.root.path().join("global.toml");
        let changed = format!("{CATALOG}\n");
        std::fs::write(&global, &changed).unwrap();
        assert_eq!(
            bridge
                .read_native_auth(&detail.ticket)
                .await
                .err()
                .unwrap()
                .code,
            "native_profile_source_conflict"
        );
        assert_eq!(std::fs::read_to_string(&global).unwrap(), changed);
        assert!(bridge.pending_intent().unwrap().is_none());
        std::fs::write(&global, CATALOG).unwrap();
        bridge.connect().await.unwrap();
        let refreshed = inspect(&mut bridge, "connection-a").await;
        let read = serde_json::to_value(bridge.read_native_auth(&refreshed.ticket).await.unwrap())
            .unwrap();
        assert_eq!(read["operation_id"], original["operation_id"]);
        assert_eq!(fixture.owner_fact(&read).await.fact, fact);
        fixture.assert_gateway_unknown("connection-a").await;
    })
    .await;
}

#[tokio::test]
async fn desktop_native_auth_revocation_blocks_all_four_commands() {
    run_case(|fixture| async move {
        let mut bridge = fixture.bridge().await;
        let detail = inspect(&mut bridge, "connection-a").await;
        let original = serde_json::to_value(
            bridge
                .start_native_auth(&detail.ticket, NativeAuthMethod::CodexBrowser)
                .await
                .unwrap(),
        )
        .unwrap();
        let fact = fixture.owner_fact(&original).await.fact;
        fixture.control(OwnerDecision::Revoke).await;
        assert!(
            bridge
                .start_native_auth(&detail.ticket, NativeAuthMethod::CodexBrowser)
                .await
                .is_err()
        );
        assert!(bridge.read_native_auth(&detail.ticket).await.is_err());
        assert!(bridge.cancel_native_auth(&detail.ticket).await.is_err());
        assert!(bridge.logout_native_auth(&detail.ticket).await.is_err());
        assert!(bridge.pending_intent().unwrap().is_none());
        fixture.control(OwnerDecision::Reactivate).await;
        assert_eq!(fixture.owner_fact(&original).await.fact, fact);
        fixture.assert_gateway_unknown("connection-a").await;
    })
    .await;
}

#[tokio::test]
async fn desktop_native_auth_owner_replacement_does_not_adopt_retained_operation() {
    run_case(|fixture| async move {
        let mut bridge = fixture.bridge().await;
        let detail = inspect(&mut bridge, "connection-a").await;
        let original = serde_json::to_value(
            bridge
                .start_native_auth(&detail.ticket, NativeAuthMethod::CodexBrowser)
                .await
                .unwrap(),
        )
        .unwrap();
        fixture.replace_owner().await;
        bridge.connect().await.unwrap();
        let refreshed = inspect(&mut bridge, "connection-a").await;
        assert_ne!(detail.ticket, refreshed.ticket);
        assert!(bridge.read_native_auth(&refreshed.ticket).await.is_err());
        assert!(bridge.cancel_native_auth(&refreshed.ticket).await.is_err());
        let (client, identity) = OwnerClient::discover(
            &fixture.root.path().join("s"),
            OwnerClientKind::Desktop,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_ne!(original["database_instance"], identity.database_instance());
        let mut current_scope = scope(&original);
        current_scope.expected_database_instance = identity.database_instance().into();
        assert_eq!(
            client
                .read_native_auth(NativeAuthQuery {
                    request_id: request_id("new-owner-operation-absence"),
                    scope: current_scope,
                    operation_id: request_id(original["operation_id"].as_str().unwrap()),
                })
                .await
                .err()
                .unwrap()
                .0,
            "native_auth_operation_missing"
        );
        assert!(!fixture.root.path().join("s/native-profiles").exists());
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("global.toml")).unwrap(),
            CATALOG
        );
        assert!(bridge.pending_intent().unwrap().is_none());
    })
    .await;
}

fn custodian() -> std::path::PathBuf {
    let path = std::env::var_os("CRAZYAGENT_TEST_NATIVE_AUTH_HOST")
        .map(std::path::PathBuf::from)
        .expect("the test run supplies the built crazyagent custodian executable path");
    assert!(path.is_absolute());
    path
}

impl Fixture {
    async fn register_claude(&self, login: &str) {
        let script = self.root.path().join("claude");
        std::fs::write(&script, format!("#!/bin/sh\n{login}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut service = self.service.lock().await;
        let service = service.as_mut().unwrap();
        service.register_native_auth_host(&custodian()).unwrap();
        service.register_claude_executable(&script).unwrap();
    }
}

async fn settle(bridge: &mut NativeBridge, phase: &str) -> Value {
    let mut seen = Value::Null;
    for _ in 0..120 {
        let detail = inspect(bridge, "connection-z").await;
        seen =
            serde_json::to_value(bridge.read_native_auth(&detail.ticket).await.unwrap()).unwrap();
        if seen["phase"] == phase {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "the real Claude sign-in never reached {phase}: last {} {}",
        seen["phase"], seen["code"]
    );
}

async fn add_account_ref(bridge: &mut NativeBridge, id: &str, generation: u64) {
    let page = bridge
        .model_access_page(ModelAccessCollection::Accounts, false)
        .await
        .unwrap();
    let receipt = bridge
        .edit_model_access(
            &page.ticket,
            ModelAccessEdit::AddAccount {
                account_ref: reference(id),
            },
            generation,
        )
        .await
        .unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::Applied);
    assert!(!receipt.pending);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_native_auth_failed_sign_in_releases_settings_and_sign_in_again() {
    run_case(|fixture| async move {
        fixture.register_claude("exit 3").await;
        let mut bridge = fixture.bridge().await;
        let detail = inspect(&mut bridge, "connection-z").await;
        let started = serde_json::to_value(
            bridge
                .start_native_auth(&detail.ticket, NativeAuthMethod::ClaudeOfficial)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_ne!(started["phase"], "unavailable");
        let failed = settle(&mut bridge, "failed").await;
        assert_eq!(failed["operation_id"], started["operation_id"]);
        assert_eq!(
            fixture.owner_fact(&failed).await.fact.phase,
            NativeAuthPhase::Failed
        );
        add_account_ref(&mut bridge, "account-after-failed", 1).await;
        let again = inspect(&mut bridge, "connection-z").await;
        let retried = serde_json::to_value(
            bridge
                .start_native_auth(&again.ticket, NativeAuthMethod::ClaudeOfficial)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_ne!(retried["operation_id"], started["operation_id"]);
        settle(&mut bridge, "failed").await;
    })
    .await;
}

async fn sleeping_login(fixture: &Fixture) -> std::path::PathBuf {
    let running = fixture.root.path().join("login-running");
    fixture
        .register_claude(&format!(
            "echo $$ > '{}'\nexec /bin/sleep 30",
            running.display()
        ))
        .await;
    running
}

async fn login_pid(running: &std::path::Path) -> String {
    for _ in 0..100 {
        let pid = std::fs::read_to_string(running).unwrap_or_default();
        if pid.ends_with('\n') {
            return pid.trim().to_owned();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the real login child never started");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_native_auth_cancelled_sign_in_releases_settings_changes() {
    run_case(|fixture| async move {
        let running = sleeping_login(&fixture).await;
        let mut bridge = fixture.bridge().await;
        let detail = inspect(&mut bridge, "connection-z").await;
        let started = serde_json::to_value(
            bridge
                .start_native_auth(&detail.ticket, NativeAuthMethod::ClaudeOfficial)
                .await
                .unwrap(),
        )
        .unwrap();
        let pid = login_pid(&running).await;
        let current = inspect(&mut bridge, "connection-z").await;
        let cancelling =
            serde_json::to_value(bridge.cancel_native_auth(&current.ticket).await.unwrap())
                .unwrap();
        assert_eq!(cancelling["operation_id"], started["operation_id"]);
        let cancelled = settle(&mut bridge, "cancelled").await;
        assert_eq!(
            fixture.owner_fact(&cancelled).await.fact.phase,
            NativeAuthPhase::Cancelled
        );
        let alive = crazyagent::spawn_gate::status(
            std::process::Command::new("/bin/kill")
                .args(["-0", &pid])
                .stderr(std::process::Stdio::null()),
        )
        .unwrap();
        assert!(!alive.success(), "cancel stops the real login child");
        add_account_ref(&mut bridge, "account-after-cancel", 1).await;
    })
    .await;
}
