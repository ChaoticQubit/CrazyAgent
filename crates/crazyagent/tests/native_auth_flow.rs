#![cfg(unix)]

use crazyagent::config::ModelConnection;
use crazyagent::engine::EngineError;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, ModelAccessRecord, NativeAuthAction, NativeAuthCancel, NativeAuthFact,
    NativeAuthLogout, NativeAuthMethod, NativeAuthPhase, NativeAuthQuery, NativeAuthReply,
    NativeAuthScope, NativeAuthStart, OwnerClient, OwnerClientKind, OwnerControl, OwnerData,
    OwnerDecision, OwnerOperation, OwnerRead, OwnerRequest, OwnerResponse,
};
use crazyagent::model_gateway::{Health, Quota};
use crazyagent::protocol::{CredentialRef, Cursor, PROTOCOL_VERSION, RequestId};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

const CATALOG: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-a", "account-z", "account-api", "account-local"]
[[model_access.connections]]
id = "connection-a"
provider = "provider-a"
harness = "codex"
protocol = "native"
account_ref = "account-a"
endpoint = { kind = "native" }
model = { id = "model-a", author = "author-a" }
auth = { method = "native_managed", profile_ref = "profile-a" }
[[model_access.connections]]
id = "connection-z"
provider = "provider-z"
harness = "claude"
protocol = "native"
account_ref = "account-z"
endpoint = { kind = "native" }
model = { id = "model-z", author = "author-z" }
auth = { method = "native_managed", profile_ref = "profile-z" }
[[model_access.connections]]
id = "connection-api"
provider = "provider-api"
harness = "rcli"
protocol = "openai"
account_ref = "account-api"
endpoint = { kind = "http", url = "https://api.example.invalid/v1" }
model = { id = "model-api", author = "author-api" }
auth = { method = "api_key", credential_ref = "api-key-reference" }
[[model_access.connections]]
id = "connection-local"
provider = "provider-local"
harness = "rcli"
protocol = "openai"
account_ref = "account-local"
endpoint = { kind = "http", url = "http://127.0.0.1:9/v1" }
model = { id = "model-local", author = "author-local" }
auth = { method = "none" }
[[model_access.pools]]
id = "configured-pool"
members = ["connection-a", "connection-z", "connection-api", "connection-local"]
projects = ["project-fixture"]
roles = ["leader"]
fallback_allowed = false
"#;

#[derive(Clone)]
struct Basis {
    scope: NativeAuthScope,
    connection: ModelConnection,
    engine_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredAuth {
    id: String,
    request: Vec<u8>,
    fact: Vec<u8>,
}

struct AuthCase {
    root: tempfile::TempDir,
    state: PathBuf,
    global: PathBuf,
    project: PathBuf,
    service: Mutex<Option<EngineService>>,
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn request_id(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}

fn private_directory(path: &Path) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

impl AuthCase {
    async fn new(catalog: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-na-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.path().join("s");
        let settings = root.path().join("c");
        private_directory(&state);
        private_directory(&settings);
        let global = settings.join("g.toml");
        private_file(&global, catalog.as_bytes());
        let case = Self {
            root,
            state,
            global,
            project: settings.join("p.toml"),
            service: Mutex::new(None),
        };
        *case.service.lock().await = Some(case.start_service().await);
        case
    }

    async fn start_service(&self) -> EngineService {
        let (_stop, shutdown) = tokio::sync::watch::channel(false);
        EngineService::start_with_settings_sources(
            &self.state,
            "e.sock",
            &self.global,
            &self.project,
            &shutdown,
        )
        .await
        .unwrap()
    }

    async fn client(&self, kind: OwnerClientKind) -> OwnerClient {
        OwnerClient::discover(&self.state, kind, Duration::from_secs(2))
            .await
            .unwrap()
            .0
    }

    async fn basis(&self, connection: &str) -> Basis {
        let (client, identity) =
            OwnerClient::discover(&self.state, OwnerClientKind::Tui, Duration::from_secs(2))
                .await
                .unwrap();
        let reply = client
            .request(OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: request_id("actual-connection-basis"),
                expected_revision: Cursor::new(0),
                operation: OwnerOperation::Read {
                    query: OwnerRead::ModelAccessRecord {
                        collection: ModelAccessCollection::Connections,
                        id: reference(connection),
                    },
                },
            })
            .await
            .unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("actual owner connection read required");
        };
        let OwnerData::ModelAccessRecord {
            source_hash,
            record: ModelAccessRecord::Connection { connection: actual },
            ..
        } = state.data
        else {
            panic!("actual selected connection required");
        };
        assert_eq!(actual.id.as_str(), connection);
        Basis {
            scope: NativeAuthScope {
                expected_database_instance: identity.database_instance().to_owned(),
                connection: actual.id.clone(),
                expected_config_id: state.config_id,
                expected_source_hash: source_hash,
            },
            connection: actual,
            engine_epoch: identity.engine_epoch().to_owned(),
        }
    }

    async fn restart(&self) {
        let mut service = self.service.lock().await.take().unwrap();
        service.shutdown().await.unwrap();
        *self.service.lock().await = Some(self.start_service().await);
    }

    async fn close(&self) -> Result<(), EngineError> {
        let service = self.service.lock().await.take();
        if let Some(mut service) = service {
            service.shutdown().await
        } else {
            Ok(())
        }
    }

    async fn control(&self, decision: OwnerDecision) {
        let service = self.service.lock().await;
        let host = service.as_ref().unwrap().owner_runtime().unwrap();
        let epoch = host.epoch(OwnerClientKind::Tui).unwrap();
        let reply = host
            .control(OwnerControl {
                operation_id: request_id(&format!("owner-control-{epoch}")),
                target: OwnerClientKind::Tui,
                expected_epoch: epoch,
                decision,
            })
            .await
            .unwrap();
        assert_eq!(reply.active, decision == OwnerDecision::Reactivate);
        assert!(reply.owner_epoch > epoch);
    }

    async fn prepare(&self, basis: &Basis) -> String {
        let service = self.service.lock().await;
        let receipt = service
            .as_ref()
            .unwrap()
            .owner_runtime()
            .unwrap()
            .prepare_native_profile(
                &basis.scope.connection,
                &basis.scope.expected_config_id,
                &basis.scope.expected_source_hash,
            )
            .await
            .unwrap();
        assert_eq!(receipt.binding().connection, basis.connection);
        receipt.binding().custody_id.clone()
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

    fn observer(&self) -> Connection {
        Connection::open_with_flags(
            self.state.join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn stored(&self) -> Vec<StoredAuth> {
        let observer = self.observer();
        let mut statement = observer
            .prepare("SELECT id,request,fact FROM native_auth_operations ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| {
                Ok(StoredAuth {
                    id: row.get(0)?,
                    request: row.get(1)?,
                    fact: row.get(2)?,
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn ordinary_counts(&self) -> Vec<i64> {
        let observer = self.observer();
        ["operations", "events", "projects", "tasks", "configs"]
            .iter()
            .map(|table| {
                observer
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap()
            })
            .collect()
    }

    fn profile(&self, profile: &str) -> PathBuf {
        let key = format!("{:x}", Sha256::digest(profile.as_bytes()));
        self.state.join("native-profiles").join(key)
    }
}

async fn run_case<F, T>(body: F)
where
    F: FnOnce(Arc<AuthCase>) -> T,
    T: Future<Output = ()> + Send + 'static,
{
    run_bounded(CATALOG, Duration::from_secs(20), body).await;
}

async fn run_bounded<F, T>(catalog: &str, deadline: Duration, body: F)
where
    F: FnOnce(Arc<AuthCase>) -> T,
    T: Future<Output = ()> + Send + 'static,
{
    let case = Arc::new(AuthCase::new(catalog).await);
    let mut task = tokio::spawn(body(Arc::clone(&case)));
    let result = tokio::time::timeout(deadline, &mut task).await;
    if result.is_err() {
        task.abort();
        let _ = task.await;
    }
    let cleanup = case.close().await;
    assert!(cleanup.is_ok(), "actual engine cleanup failed: {cleanup:?}");
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => std::panic::resume_unwind(error.into_panic()),
        Err(error) => panic!("bounded native auth test timed out: {error}"),
    }
    assert!(case.root.path().is_dir());
}

fn start(basis: &Basis, id: &str, method: NativeAuthMethod) -> NativeAuthStart {
    NativeAuthStart {
        request_id: request_id(id),
        scope: basis.scope.clone(),
        method,
    }
}

fn query(basis: &Basis, id: &str, operation: &str) -> NativeAuthQuery {
    NativeAuthQuery {
        request_id: request_id(id),
        scope: basis.scope.clone(),
        operation_id: request_id(operation),
    }
}

fn cancel(basis: &Basis, id: &str, operation: &str) -> NativeAuthCancel {
    NativeAuthCancel {
        request_id: request_id(id),
        scope: basis.scope.clone(),
        operation_id: request_id(operation),
    }
}

fn logout(basis: &Basis, id: &str) -> NativeAuthLogout {
    NativeAuthLogout {
        request_id: request_id(id),
        scope: basis.scope.clone(),
    }
}

fn assert_error(result: Result<NativeAuthReply, EngineError>, expected: &str) {
    assert_eq!(result.unwrap_err().0, expected);
}

async fn assert_unavailable(
    case: &AuthCase,
    basis: &Basis,
    reply: &NativeAuthReply,
    request: &str,
    operation: &str,
    action: NativeAuthAction,
) {
    assert_eq!(reply.request_id.as_str(), request);
    assert_eq!(
        reply.database_instance,
        basis.scope.expected_database_instance
    );
    assert_eq!(reply.engine_epoch, basis.engine_epoch);
    assert_eq!(reply.fact.operation_id.as_str(), operation);
    assert_eq!(reply.fact.action, action);
    assert_eq!(
        reply.fact.binding.database_instance,
        reply.database_instance
    );
    assert_eq!(reply.fact.binding.engine_epoch, basis.engine_epoch);
    assert_eq!(reply.fact.binding.connection, basis.connection);
    assert_eq!(reply.fact.binding.config_id, basis.scope.expected_config_id);
    assert_eq!(
        reply.fact.binding.source_hash,
        basis.scope.expected_source_hash
    );
    assert_eq!(reply.fact.binding.custody_id.len(), 64);
    assert!(reply.fact.generation > 0);
    assert_eq!(reply.fact.phase, NativeAuthPhase::Unavailable);
    assert_eq!(
        reply.fact.code.as_deref(),
        Some("native_auth_host_unavailable")
    );
    assert!(reply.challenge.is_none());
    assert!(reply.account.is_none());
    assert!(!format!("{reply:?}").contains("Ready"));
    case.assert_gateway_unknown(basis.scope.connection.as_str())
        .await;
}

fn assert_stored(rows: &[StoredAuth]) {
    for row in rows {
        assert!(row.request.len() <= 8192);
        assert!(row.fact.len() <= 16384);
        let request: Value = serde_json::from_slice(&row.request).unwrap();
        let fact: Value = serde_json::from_slice(&row.fact).unwrap();
        assert_eq!(request["request_id"].as_str(), Some(row.id.as_str()));
        assert_eq!(fact["operation_id"].as_str(), Some(row.id.as_str()));
        assert_eq!(fact["phase"], "unavailable");
        assert_eq!(fact["code"], "native_auth_host_unavailable");
        for forbidden in ["challenge", "account", "auth_url", "user_code", "token"] {
            assert!(fact.get(forbidden).is_none(), "durable field: {forbidden}");
            assert!(
                request.get(forbidden).is_none(),
                "request field: {forbidden}"
            );
        }
        assert!(!row.fact.windows(5).any(|bytes| bytes == b"Ready"));
    }
}

#[tokio::test]
async fn codex_browser_start_is_exact_owner_bound_and_explicitly_unavailable() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let reply = client
            .start_native_auth(start(&basis, "browser", NativeAuthMethod::CodexBrowser))
            .await
            .unwrap();
        assert_unavailable(
            &case,
            &basis,
            &reply,
            "browser",
            "browser",
            NativeAuthAction::SignIn {
                method: NativeAuthMethod::CodexBrowser,
            },
        )
        .await;
        let rows = case.stored();
        assert_eq!(rows.len(), 1);
        assert_stored(&rows);
    })
    .await;
}

#[tokio::test]
async fn codex_device_code_start_does_not_fabricate_a_challenge() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let reply = client
            .start_native_auth(start(&basis, "device", NativeAuthMethod::CodexDeviceCode))
            .await
            .unwrap();
        assert_unavailable(
            &case,
            &basis,
            &reply,
            "device",
            "device",
            NativeAuthAction::SignIn {
                method: NativeAuthMethod::CodexDeviceCode,
            },
        )
        .await;
    })
    .await;
}

#[tokio::test]
async fn claude_start_uses_the_selected_second_connection_without_codex_fallback() {
    run_case(|case| async move {
        let basis = case.basis("connection-z").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let reply = client
            .start_native_auth(start(&basis, "claude", NativeAuthMethod::ClaudeOfficial))
            .await
            .unwrap();
        assert_unavailable(
            &case,
            &basis,
            &reply,
            "claude",
            "claude",
            NativeAuthAction::SignIn {
                method: NativeAuthMethod::ClaudeOfficial,
            },
        )
        .await;
        assert_eq!(
            reply.fact.binding.connection.account_ref.as_str(),
            "account-z"
        );
        assert_eq!(reply.fact.binding.connection.harness.as_str(), "claude");
        assert!(case.profile("profile-z").is_dir());
        assert!(!case.profile("profile-a").exists());
    })
    .await;
}

#[tokio::test]
async fn desktop_owner_reads_the_actual_tui_operation_without_an_extra_receipt() {
    run_case(|case| async move {
        let basis = case.basis("connection-z").await;
        let tui = case.client(OwnerClientKind::Tui).await;
        let original = tui
            .start_native_auth(start(&basis, "shared", NativeAuthMethod::ClaudeOfficial))
            .await
            .unwrap();
        let before = case.stored();
        let desktop = case.client(OwnerClientKind::Desktop).await;
        let read = desktop
            .read_native_auth(query(&basis, "desktop-read", "shared"))
            .await
            .unwrap();
        assert_eq!(read.request_id.as_str(), "desktop-read");
        assert_eq!(read.fact, original.fact);
        assert!(read.challenge.is_none());
        assert!(read.account.is_none());
        assert_eq!(case.stored(), before);
    })
    .await;
}

#[tokio::test]
async fn exact_start_replay_preserves_the_original_fact_and_receipt_bytes() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let request = start(&basis, "replay", NativeAuthMethod::CodexBrowser);
        let first = client.start_native_auth(request.clone()).await.unwrap();
        let before = case.stored();
        let replay = client.start_native_auth(request).await.unwrap();
        assert_eq!(replay.fact, first.fact);
        assert_eq!(replay.request_id, first.request_id);
        assert_eq!(case.stored(), before);
    })
    .await;
}

#[tokio::test]
async fn known_start_id_with_a_different_method_is_a_conflict() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        client
            .start_native_auth(start(
                &basis,
                "method-conflict",
                NativeAuthMethod::CodexBrowser,
            ))
            .await
            .unwrap();
        let before = case.stored();
        assert_error(
            client
                .start_native_auth(start(
                    &basis,
                    "method-conflict",
                    NativeAuthMethod::CodexDeviceCode,
                ))
                .await,
            "native_auth_operation_conflict",
        );
        assert_eq!(case.stored(), before);
    })
    .await;
}

#[tokio::test]
async fn known_start_id_cannot_be_rebound_to_another_connection() {
    run_case(|case| async move {
        let a = case.basis("connection-a").await;
        let z = case.basis("connection-z").await;
        let client = case.client(OwnerClientKind::Tui).await;
        client
            .start_native_auth(start(
                &a,
                "connection-conflict",
                NativeAuthMethod::CodexBrowser,
            ))
            .await
            .unwrap();
        let before = case.stored();
        assert_error(
            client
                .start_native_auth(start(
                    &z,
                    "connection-conflict",
                    NativeAuthMethod::ClaudeOfficial,
                ))
                .await,
            "native_auth_operation_conflict",
        );
        assert!(!case.profile("profile-z").exists());
        assert_eq!(case.stored(), before);
    })
    .await;
}

#[tokio::test]
async fn start_and_logout_share_one_operation_identity_namespace() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        client
            .start_native_auth(start(
                &basis,
                "action-conflict",
                NativeAuthMethod::CodexBrowser,
            ))
            .await
            .unwrap();
        let before = case.stored();
        assert_error(
            client
                .logout_native_auth(logout(&basis, "action-conflict"))
                .await,
            "native_auth_operation_conflict",
        );
        assert_eq!(case.stored(), before);
    })
    .await;
}

#[tokio::test]
async fn logout_without_a_native_host_is_not_reported_as_successful_logout() {
    run_case(|case| async move {
        let basis = case.basis("connection-z").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let reply = client
            .logout_native_auth(logout(&basis, "logout"))
            .await
            .unwrap();
        assert_unavailable(
            &case,
            &basis,
            &reply,
            "logout",
            "logout",
            NativeAuthAction::Logout,
        )
        .await;
        assert_eq!(case.stored().len(), 1);
        assert_stored(&case.stored());
    })
    .await;
}

#[tokio::test]
async fn exact_logout_replay_does_not_allocate_or_change_its_fact() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let request = logout(&basis, "logout-replay");
        let first = client.logout_native_auth(request.clone()).await.unwrap();
        let before = case.stored();
        let replay = client.logout_native_auth(request).await.unwrap();
        assert_eq!(replay.fact, first.fact);
        assert_eq!(case.stored(), before);
    })
    .await;
}

#[tokio::test]
async fn unknown_operation_read_is_explicit_and_does_not_allocate() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        assert_error(
            client
                .read_native_auth(query(&basis, "read-missing", "missing"))
                .await,
            "native_auth_operation_missing",
        );
        assert!(case.stored().is_empty());
        assert!(!case.profile("profile-a").exists());
    })
    .await;
}

#[tokio::test]
async fn known_operation_read_requires_the_original_selected_connection() {
    run_case(|case| async move {
        let a = case.basis("connection-a").await;
        let z = case.basis("connection-z").await;
        let client = case.client(OwnerClientKind::Tui).await;
        client
            .start_native_auth(start(&a, "scoped", NativeAuthMethod::CodexBrowser))
            .await
            .unwrap();
        let before = case.stored();
        assert_error(
            client
                .read_native_auth(query(&z, "wrong-selection", "scoped"))
                .await,
            "native_auth_operation_scope_conflict",
        );
        assert_eq!(case.stored(), before);
        assert!(!case.profile("profile-z").exists());
    })
    .await;
}

#[tokio::test]
async fn cancelling_an_unknown_operation_does_not_fabricate_cancellation() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        assert_error(
            client
                .cancel_native_auth(cancel(&basis, "cancel-missing", "missing"))
                .await,
            "native_auth_operation_missing",
        );
        assert!(case.stored().is_empty());
        assert!(!case.profile("profile-a").exists());
    })
    .await;
}

#[tokio::test]
async fn cancelling_terminal_unavailable_preserves_the_original_fact() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let original = client
            .start_native_auth(start(&basis, "terminal", NativeAuthMethod::CodexBrowser))
            .await
            .unwrap();
        let before = case.stored();
        assert_error(
            client
                .cancel_native_auth(cancel(&basis, "cancel-terminal", "terminal"))
                .await,
            "native_auth_not_active",
        );
        let read = client
            .read_native_auth(query(&basis, "after-cancel", "terminal"))
            .await
            .unwrap();
        assert_eq!(read.fact, original.fact);
        assert_eq!(case.stored(), before);
    })
    .await;
}

#[tokio::test]
async fn cancelling_a_known_operation_under_a_different_connection_is_rejected() {
    run_case(|case| async move {
        let a = case.basis("connection-a").await;
        let z = case.basis("connection-z").await;
        let client = case.client(OwnerClientKind::Tui).await;
        client
            .start_native_auth(start(&a, "cancel-scope", NativeAuthMethod::CodexBrowser))
            .await
            .unwrap();
        let before = case.stored();
        assert_error(
            client
                .cancel_native_auth(cancel(&z, "wrong-cancel", "cancel-scope"))
                .await,
            "native_auth_operation_scope_conflict",
        );
        assert_eq!(case.stored(), before);
        assert!(!case.profile("profile-z").exists());
    })
    .await;
}

#[tokio::test]
async fn every_native_auth_command_rejects_a_different_expected_database() {
    run_case(|case| async move {
        let mut basis = case.basis("connection-a").await;
        basis.scope.expected_database_instance = "0".repeat(64);
        let client = case.client(OwnerClientKind::Tui).await;
        assert_error(
            client
                .start_native_auth(start(
                    &basis,
                    "wrong-db-start",
                    NativeAuthMethod::CodexBrowser,
                ))
                .await,
            "native_auth_database_mismatch",
        );
        assert_error(
            client
                .read_native_auth(query(&basis, "wrong-db-read", "missing"))
                .await,
            "native_auth_database_mismatch",
        );
        assert_error(
            client
                .cancel_native_auth(cancel(&basis, "wrong-db-cancel", "missing"))
                .await,
            "native_auth_database_mismatch",
        );
        assert_error(
            client
                .logout_native_auth(logout(&basis, "wrong-db-logout"))
                .await,
            "native_auth_database_mismatch",
        );
        assert!(case.stored().is_empty());
        assert!(!case.state.join("native-profiles").exists());
    })
    .await;
}

#[tokio::test]
async fn stale_config_rejects_before_profile_preparation_or_receipt() {
    run_case(|case| async move {
        let mut basis = case.basis("connection-z").await;
        basis.scope.expected_config_id =
            crazyagent::protocol::ConfigId::new("stale-config").unwrap();
        let client = case.client(OwnerClientKind::Tui).await;
        assert_error(
            client
                .start_native_auth(start(
                    &basis,
                    "stale-config",
                    NativeAuthMethod::ClaudeOfficial,
                ))
                .await,
            "native_profile_source_conflict",
        );
        assert_error(
            client
                .logout_native_auth(logout(&basis, "stale-config-out"))
                .await,
            "native_profile_source_conflict",
        );
        assert!(case.stored().is_empty());
        assert!(!case.state.join("native-profiles").exists());
    })
    .await;
}

#[tokio::test]
async fn stale_source_hash_rejects_all_four_commands_without_effects() {
    run_case(|case| async move {
        let mut basis = case.basis("connection-a").await;
        basis.scope.expected_source_hash = "f".repeat(64);
        let client = case.client(OwnerClientKind::Tui).await;
        assert_error(
            client
                .start_native_auth(start(&basis, "stale-start", NativeAuthMethod::CodexBrowser))
                .await,
            "native_profile_source_conflict",
        );
        assert_error(
            client
                .read_native_auth(query(&basis, "stale-read", "missing"))
                .await,
            "native_profile_source_conflict",
        );
        assert_error(
            client
                .cancel_native_auth(cancel(&basis, "stale-cancel", "missing"))
                .await,
            "native_profile_source_conflict",
        );
        assert_error(
            client.logout_native_auth(logout(&basis, "stale-out")).await,
            "native_profile_source_conflict",
        );
        assert!(case.stored().is_empty());
        assert!(!case.state.join("native-profiles").exists());
    })
    .await;
}

#[tokio::test]
async fn malformed_source_hash_is_not_silently_normalized() {
    run_case(|case| async move {
        let original = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        for (index, hash) in [
            String::new(),
            "a".repeat(63),
            "A".repeat(64),
            "g".repeat(64),
        ]
        .into_iter()
        .enumerate()
        {
            let mut basis = original.clone();
            basis.scope.expected_source_hash = hash;
            assert_error(
                client
                    .start_native_auth(start(
                        &basis,
                        &format!("bad-source-{index}"),
                        NativeAuthMethod::CodexBrowser,
                    ))
                    .await,
                "native_profile_source_invalid",
            );
        }
        assert!(case.stored().is_empty());
        assert!(!case.state.join("native-profiles").exists());
    })
    .await;
}

#[tokio::test]
async fn a_missing_connection_has_no_first_item_fallback() {
    run_case(|case| async move {
        let mut basis = case.basis("connection-a").await;
        basis.scope.connection = reference("connection-missing");
        let client = case.client(OwnerClientKind::Tui).await;
        assert_error(
            client
                .start_native_auth(start(
                    &basis,
                    "missing-connection",
                    NativeAuthMethod::CodexBrowser,
                ))
                .await,
            "native_profile_connection_missing",
        );
        assert!(case.stored().is_empty());
        assert!(!case.state.join("native-profiles").exists());
    })
    .await;
}

#[tokio::test]
async fn official_auth_method_must_match_the_selected_harness() {
    run_case(|case| async move {
        let a = case.basis("connection-a").await;
        let z = case.basis("connection-z").await;
        let client = case.client(OwnerClientKind::Tui).await;
        assert_error(
            client
                .start_native_auth(start(&a, "wrong-claude", NativeAuthMethod::ClaudeOfficial))
                .await,
            "native_auth_method_mismatch",
        );
        assert_error(
            client
                .start_native_auth(start(&z, "wrong-browser", NativeAuthMethod::CodexBrowser))
                .await,
            "native_auth_method_mismatch",
        );
        assert_error(
            client
                .start_native_auth(start(&z, "wrong-device", NativeAuthMethod::CodexDeviceCode))
                .await,
            "native_auth_method_mismatch",
        );
        assert!(case.stored().is_empty());
        assert!(!case.state.join("native-profiles").exists());
    })
    .await;
}

#[tokio::test]
async fn api_key_and_local_rcli_connections_cannot_use_subscription_sign_in() {
    run_case(|case| async move {
        let client = case.client(OwnerClientKind::Tui).await;
        for connection in ["connection-api", "connection-local"] {
            let basis = case.basis(connection).await;
            assert_error(
                client
                    .start_native_auth(start(
                        &basis,
                        &format!("not-native-{connection}"),
                        NativeAuthMethod::CodexBrowser,
                    ))
                    .await,
                "native_profile_not_managed",
            );
            assert_error(
                client
                    .logout_native_auth(logout(&basis, &format!("not-native-out-{connection}")))
                    .await,
                "native_profile_not_managed",
            );
        }
        assert!(case.stored().is_empty());
        assert!(!case.state.join("native-profiles").exists());
    })
    .await;
}

#[tokio::test]
async fn oversized_request_fails_with_compact_bounds_error_before_effects() {
    run_case(|case| async move {
        let mut basis = case.basis("connection-a").await;
        basis.scope.expected_database_instance = "bound-sentinel-".repeat(6000);
        let client = case.client(OwnerClientKind::Tui).await;
        let error = client
            .start_native_auth(start(&basis, "oversized", NativeAuthMethod::CodexBrowser))
            .await
            .unwrap_err();
        assert_eq!(error.0, "native_auth_request_bounds");
        assert!(!error.0.contains("bound-sentinel"));
        assert!(case.stored().is_empty());
        assert!(!case.state.join("native-profiles").exists());
    })
    .await;
}

#[tokio::test]
async fn start_request_rejects_caller_paths_argv_tokens_challenges_and_ready() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let valid = serde_json::to_value(start(
            &basis,
            "closed-request",
            NativeAuthMethod::CodexBrowser,
        ))
        .unwrap();
        for key in ["argv", "home", "token", "challenge", "ready"] {
            let mut top = valid.clone();
            top.as_object_mut()
                .unwrap()
                .insert(key.into(), json!("caller-sentinel"));
            assert!(
                serde_json::from_value::<NativeAuthStart>(top).is_err(),
                "top-level {key}"
            );
            let mut nested = valid.clone();
            nested["scope"]
                .as_object_mut()
                .unwrap()
                .insert(key.into(), json!("caller-sentinel"));
            assert!(
                serde_json::from_value::<NativeAuthStart>(nested).is_err(),
                "scope {key}"
            );
        }
        assert!(case.stored().is_empty());
        assert!(!case.state.join("native-profiles").exists());
    })
    .await;
}

#[tokio::test]
async fn unsupported_external_token_api_and_rcli_methods_are_not_accepted() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let valid = serde_json::to_value(start(
            &basis,
            "closed-method",
            NativeAuthMethod::CodexBrowser,
        ))
        .unwrap();
        for method in [
            "external_token",
            "oauth_token",
            "api_key",
            "rcli",
            "ready",
            "CodexBrowser",
        ] {
            let mut value = valid.clone();
            value["method"] = json!(method);
            assert!(
                serde_json::from_value::<NativeAuthStart>(value).is_err(),
                "method {method}"
            );
        }
        assert!(case.stored().is_empty());
    })
    .await;
}

#[tokio::test]
async fn revoked_owner_cannot_start_read_cancel_or_logout() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        client
            .start_native_auth(start(
                &basis,
                "before-revoke",
                NativeAuthMethod::CodexBrowser,
            ))
            .await
            .unwrap();
        case.control(OwnerDecision::Revoke).await;
        let before = case.stored();
        assert!(
            client
                .start_native_auth(start(
                    &basis,
                    "revoked-start",
                    NativeAuthMethod::CodexBrowser
                ))
                .await
                .is_err()
        );
        assert!(
            client
                .read_native_auth(query(&basis, "revoked-read", "before-revoke"))
                .await
                .is_err()
        );
        assert!(
            client
                .cancel_native_auth(cancel(&basis, "revoked-cancel", "before-revoke"))
                .await
                .is_err()
        );
        assert!(
            client
                .logout_native_auth(logout(&basis, "revoked-out"))
                .await
                .is_err()
        );
        assert_eq!(case.stored(), before);
        case.assert_gateway_unknown("connection-a").await;
    })
    .await;
}

#[tokio::test]
async fn reactivated_owner_reads_existing_fact_but_old_capability_stays_revoked() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let old = case.client(OwnerClientKind::Tui).await;
        let original = old
            .start_native_auth(start(&basis, "reactivate", NativeAuthMethod::CodexBrowser))
            .await
            .unwrap();
        case.control(OwnerDecision::Revoke).await;
        case.control(OwnerDecision::Reactivate).await;
        let before = case.stored();
        assert!(
            old.read_native_auth(query(&basis, "old-grant", "reactivate"))
                .await
                .is_err()
        );
        let fresh = case.client(OwnerClientKind::Tui).await;
        let read = fresh
            .read_native_auth(query(&basis, "fresh-grant", "reactivate"))
            .await
            .unwrap();
        assert_eq!(read.fact, original.fact);
        assert_eq!(case.stored(), before);
    })
    .await;
}

#[tokio::test]
async fn restart_preserves_terminal_unavailability_and_original_observation_epoch() {
    run_case(|case| async move {
        let original_basis = case.basis("connection-z").await;
        let old = case.client(OwnerClientKind::Tui).await;
        let original = old
            .start_native_auth(start(
                &original_basis,
                "restart",
                NativeAuthMethod::ClaudeOfficial,
            ))
            .await
            .unwrap();
        let before = case.stored();
        case.restart().await;
        let basis = case.basis("connection-z").await;
        assert_eq!(basis.scope, original_basis.scope);
        assert_ne!(basis.engine_epoch, original_basis.engine_epoch);
        assert!(
            old.read_native_auth(query(&basis, "old-epoch", "restart"))
                .await
                .is_err()
        );
        let client = case.client(OwnerClientKind::Tui).await;
        let read = client
            .read_native_auth(query(&basis, "restart-read", "restart"))
            .await
            .unwrap();
        assert_eq!(
            read.database_instance,
            basis.scope.expected_database_instance
        );
        assert_eq!(read.engine_epoch, basis.engine_epoch);
        assert_eq!(read.fact, original.fact);
        assert_eq!(read.fact.phase, NativeAuthPhase::Unavailable);
        assert_ne!(read.fact.binding.engine_epoch, read.engine_epoch);
        assert!(read.challenge.is_none());
        assert!(read.account.is_none());
        assert_eq!(case.stored(), before);
        case.assert_gateway_unknown("connection-z").await;
    })
    .await;
}

#[tokio::test]
async fn exact_start_replay_after_restart_does_not_relaunch_or_rewrite() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let request = start(&basis, "restart-replay", NativeAuthMethod::CodexDeviceCode);
        let original = client.start_native_auth(request.clone()).await.unwrap();
        let before = case.stored();
        case.restart().await;
        let client = case.client(OwnerClientKind::Tui).await;
        let replay = client.start_native_auth(request).await.unwrap();
        assert_eq!(replay.fact, original.fact);
        assert_eq!(case.stored(), before);
        assert!(replay.challenge.is_none());
        assert!(replay.account.is_none());
    })
    .await;
}

#[tokio::test]
async fn restart_never_invents_a_missing_operation_or_authenticated_account() {
    run_case(|case| async move {
        case.restart().await;
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        assert_error(
            client
                .read_native_auth(query(&basis, "restart-missing", "never-started"))
                .await,
            "native_auth_operation_missing",
        );
        assert!(case.stored().is_empty());
        assert!(!case.state.join("native-profiles").exists());
        case.assert_gateway_unknown("connection-a").await;
    })
    .await;
}

#[tokio::test]
async fn changed_real_settings_bytes_reject_the_previously_valid_source() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let before = case.ordinary_counts();
        private_file(&case.global, format!("{CATALOG}\n").as_bytes());
        assert_error(
            client
                .start_native_auth(start(
                    &basis,
                    "changed-source",
                    NativeAuthMethod::CodexBrowser,
                ))
                .await,
            "native_profile_source_conflict",
        );
        assert!(case.stored().is_empty());
        assert!(!case.state.join("native-profiles").exists());
        assert_eq!(case.ordinary_counts(), before);
    })
    .await;
}

#[tokio::test]
async fn reused_populated_home_and_sentinel_remain_unchanged_across_auth_and_restart() {
    run_case(|case| async move {
        let basis = case.basis("connection-z").await;
        let custody = case.prepare(&basis).await;
        let sentinel = case.profile("profile-z").join("home/native-owned");
        private_file(&sentinel, b"unread native-owned fixture sentinel\n");
        std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o000)).unwrap();
        let before = std::fs::symlink_metadata(&sentinel).unwrap();
        let client = case.client(OwnerClientKind::Tui).await;
        let first = client
            .start_native_auth(start(&basis, "populated", NativeAuthMethod::ClaudeOfficial))
            .await
            .unwrap();
        assert_eq!(first.fact.binding.custody_id, custody);
        case.restart().await;
        let basis = case.basis("connection-z").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let second = client
            .logout_native_auth(logout(&basis, "populated-out"))
            .await
            .unwrap();
        assert_eq!(second.fact.binding.custody_id, custody);
        let after = std::fs::symlink_metadata(&sentinel).unwrap();
        assert_eq!(
            (after.dev(), after.ino(), after.len(), after.mode()),
            (before.dev(), before.ino(), before.len(), before.mode())
        );
        assert_eq!(
            (after.mtime(), after.mtime_nsec()),
            (before.mtime(), before.mtime_nsec())
        );
        assert_eq!(first.fact.phase, NativeAuthPhase::Unavailable);
        assert_eq!(second.fact.phase, NativeAuthPhase::Unavailable);
        assert!(first.account.is_none());
        assert!(second.account.is_none());
    })
    .await;
}

#[tokio::test]
async fn unsafe_profile_permissions_are_rejected_instead_of_using_ambient_home() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        case.prepare(&basis).await;
        let profile = case.profile("profile-a");
        std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o755)).unwrap();
        let client = case.client(OwnerClientKind::Tui).await;
        let error = client
            .start_native_auth(start(
                &basis,
                "unsafe-profile",
                NativeAuthMethod::CodexBrowser,
            ))
            .await
            .unwrap_err();
        assert!(error.0.starts_with("native_profile_"), "{error:?}");
        assert!(case.stored().is_empty());
        assert_eq!(
            std::fs::symlink_metadata(&profile).unwrap().mode() & 0o777,
            0o755
        );
    })
    .await;
}

#[tokio::test]
async fn replaced_home_symlink_is_rejected_without_touching_its_target() {
    run_case(|case| async move {
        let basis = case.basis("connection-z").await;
        case.prepare(&basis).await;
        let home = case.profile("profile-z").join("home");
        std::fs::rename(&home, case.profile("profile-z").join("retained-home")).unwrap();
        let outside = case.root.path().join("outside");
        private_directory(&outside);
        let marker = outside.join("sentinel");
        private_file(&marker, b"unrelated fixture directory\n");
        let before = std::fs::symlink_metadata(&marker).unwrap();
        symlink(&outside, &home).unwrap();
        let client = case.client(OwnerClientKind::Tui).await;
        let error = client
            .start_native_auth(start(
                &basis,
                "symlink-home",
                NativeAuthMethod::ClaudeOfficial,
            ))
            .await
            .unwrap_err();
        assert!(error.0.starts_with("native_profile_"), "{error:?}");
        let after = std::fs::symlink_metadata(&marker).unwrap();
        assert_eq!(
            (after.ino(), after.len(), after.mtime(), after.mtime_nsec()),
            (
                before.ino(),
                before.len(),
                before.mtime(),
                before.mtime_nsec()
            )
        );
        assert!(
            std::fs::symlink_metadata(&home)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(case.stored().is_empty());
    })
    .await;
}

#[tokio::test]
async fn unavailable_auth_does_not_create_tasks_events_models_or_dispatch_effects() {
    run_case(|case| async move {
        let a = case.basis("connection-a").await;
        let z = case.basis("connection-z").await;
        let before = case.ordinary_counts();
        let original_config = std::fs::read(&case.global).unwrap();
        let client = case.client(OwnerClientKind::Tui).await;
        client
            .start_native_auth(start(&a, "no-effects-a", NativeAuthMethod::CodexBrowser))
            .await
            .unwrap();
        client
            .start_native_auth(start(&z, "no-effects-z", NativeAuthMethod::ClaudeOfficial))
            .await
            .unwrap();
        client
            .logout_native_auth(logout(&a, "no-effects-out"))
            .await
            .unwrap();
        assert_eq!(case.ordinary_counts(), before);
        assert_eq!(std::fs::read(&case.global).unwrap(), original_config);
        assert!(!case.project.exists());
        assert_eq!(case.stored().len(), 3);
        assert_stored(&case.stored());
        case.assert_gateway_unknown("connection-a").await;
        case.assert_gateway_unknown("connection-z").await;
    })
    .await;
}

#[tokio::test]
async fn repeated_status_reads_do_not_extend_the_receipt_ledger_or_generation() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let original = client
            .start_native_auth(start(
                &basis,
                "bounded-read",
                NativeAuthMethod::CodexBrowser,
            ))
            .await
            .unwrap();
        let before = case.stored();
        for index in 0..32 {
            let read = client
                .read_native_auth(query(&basis, &format!("status-{index}"), "bounded-read"))
                .await
                .unwrap();
            assert_eq!(read.fact, original.fact);
            assert!(read.challenge.is_none());
            assert!(read.account.is_none());
        }
        assert_eq!(case.stored(), before);
        assert_stored(&before);
    })
    .await;
}

async fn fill_capacity(client: &OwnerClient, basis: &Basis) -> (NativeAuthFact, NativeAuthFact) {
    let first = client
        .start_native_auth(start(basis, "capacity-0", NativeAuthMethod::CodexBrowser))
        .await
        .unwrap()
        .fact;
    let second = client
        .logout_native_auth(logout(basis, "capacity-1"))
        .await
        .unwrap()
        .fact;
    for index in 2..1024 {
        let id = format!("capacity-{index}");
        let reply = if index % 2 == 0 {
            client
                .start_native_auth(start(basis, &id, NativeAuthMethod::CodexBrowser))
                .await
        } else {
            client.logout_native_auth(logout(basis, &id)).await
        }
        .unwrap();
        assert_eq!(reply.fact.operation_id.as_str(), id);
        assert_eq!(reply.fact.phase, NativeAuthPhase::Unavailable);
        assert!(reply.challenge.is_none());
        assert!(reply.account.is_none());
    }
    (first, second)
}

async fn assert_capacity_rejections(case: &AuthCase, client: &OwnerClient, basis: &Basis) {
    let before = case.stored();
    assert_error(
        client
            .start_native_auth(start(
                basis,
                "capacity-new-start",
                NativeAuthMethod::ClaudeOfficial,
            ))
            .await,
        "native_auth_operation_capacity",
    );
    assert_error(
        client
            .logout_native_auth(logout(basis, "capacity-new-logout"))
            .await,
        "native_auth_operation_capacity",
    );
    assert_eq!(case.stored(), before);
    assert!(!case.profile("profile-z").exists());
}

#[tokio::test]
async fn capacity_1024_preserves_known_replay_conflict_and_nonallocating_reads() {
    run_bounded(CATALOG, Duration::from_secs(90), |case| async move {
        let a = case.basis("connection-a").await;
        let z = case.basis("connection-z").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let ordinary = case.ordinary_counts();
        let (first, second) = fill_capacity(&client, &a).await;
        let before = case.stored();
        assert_eq!(before.len(), 1024);
        assert_stored(&before);
        assert!(!case.profile("profile-z").exists());
        let replay = client
            .start_native_auth(start(&a, "capacity-0", NativeAuthMethod::CodexBrowser))
            .await
            .unwrap();
        assert_eq!(replay.fact, first);
        let replay = client
            .logout_native_auth(logout(&a, "capacity-1"))
            .await
            .unwrap();
        assert_eq!(replay.fact, second);
        assert_error(
            client
                .start_native_auth(start(&a, "capacity-0", NativeAuthMethod::CodexDeviceCode))
                .await,
            "native_auth_operation_conflict",
        );
        assert_error(
            client.logout_native_auth(logout(&a, "capacity-0")).await,
            "native_auth_operation_conflict",
        );
        assert_error(
            client
                .start_native_auth(start(&z, "capacity-0", NativeAuthMethod::ClaudeOfficial))
                .await,
            "native_auth_operation_conflict",
        );
        let read = client
            .read_native_auth(query(&a, "capacity-read", "capacity-0"))
            .await
            .unwrap();
        assert_eq!(read.fact, first);
        assert_error(
            client
                .cancel_native_auth(cancel(&a, "capacity-cancel", "capacity-0"))
                .await,
            "native_auth_not_active",
        );
        assert_capacity_rejections(&case, &client, &z).await;
        assert_eq!(case.stored(), before);
        assert_eq!(case.ordinary_counts(), ordinary);
        case.assert_gateway_unknown("connection-a").await;
    })
    .await;
}

#[tokio::test]
async fn new_operation_after_terminal_unavailability_gets_a_distinct_generation() {
    run_case(|case| async move {
        let basis = case.basis("connection-a").await;
        let client = case.client(OwnerClientKind::Tui).await;
        let first = client
            .start_native_auth(start(
                &basis,
                "generation-a",
                NativeAuthMethod::CodexBrowser,
            ))
            .await
            .unwrap();
        let second = client
            .start_native_auth(start(
                &basis,
                "generation-b",
                NativeAuthMethod::CodexDeviceCode,
            ))
            .await
            .unwrap();
        assert!(second.fact.generation > first.fact.generation);
        assert_eq!(
            first.fact.binding.custody_id,
            second.fact.binding.custody_id
        );
        let old = client
            .read_native_auth(query(&basis, "generation-read", "generation-a"))
            .await
            .unwrap();
        assert_eq!(old.fact, first.fact);
        assert_eq!(case.stored().len(), 2);
    })
    .await;
}

#[tokio::test]
async fn incompatible_profile_aliases_are_rejected_without_allocating_auth_receipts() {
    let catalog = CATALOG.replace("profile_ref = \"profile-z\"", "profile_ref = \"profile-a\"");
    run_bounded(&catalog, Duration::from_secs(20), |case| async move {
        let basis = case.basis("connection-z").await;
        let client = case.client(OwnerClientKind::Tui).await;
        assert_error(
            client
                .start_native_auth(start(
                    &basis,
                    "alias-conflict",
                    NativeAuthMethod::ClaudeOfficial,
                ))
                .await,
            "native_profile_binding_conflict",
        );
        assert!(case.stored().is_empty());
        assert!(!case.state.join("native-profiles").exists());
    })
    .await;
}

#[test]
fn three_official_methods_have_exact_closed_wire_names() {
    assert_eq!(
        serde_json::to_value(NativeAuthMethod::CodexBrowser).unwrap(),
        "codex_browser"
    );
    assert_eq!(
        serde_json::to_value(NativeAuthMethod::CodexDeviceCode).unwrap(),
        "codex_device_code"
    );
    assert_eq!(
        serde_json::to_value(NativeAuthMethod::ClaudeOfficial).unwrap(),
        "claude_official"
    );
}
