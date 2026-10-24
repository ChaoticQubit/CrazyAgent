#![cfg(unix)]

use crazyagent::config::resolve_config;
use crazyagent::conversation::{ConversationContent, ConversationMutation, ConversationPage};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::*;
use crazyagent::master_session::{
    MasterBackend, MasterCursor, MasterOwnerAction, MasterReason, MasterSelection,
    MasterSessionPhase, MasterSessionView, MasterTurnPage, MasterTurnPhase, MasterTurnView,
};
use crazyagent::protocol::*;
use crazyagent::routing::{ResourceBudget, RouteRequest};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

const CATALOG: &str = r#"
version = 1
timeout_ms = 2000
allowed_destinations = ["sandbox-profile"]
[routing]
approved_accounts = ["api-account", "codex-account", "claude-account"]
global_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
project_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
task_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
[model_access]
accounts = ["api-account", "codex-account", "claude-account"]
[[model_access.connections]]
id = "api-connection"
provider = "configured-api"
harness = "rcli"
protocol = "openai-compatible"
account_ref = "api-account"
endpoint = { kind="http", url="https://model.example.invalid/v1" }
model = { id="configured-model", author="configured-author" }
auth = { method="none" }
[[model_access.connections]]
id = "codex-connection"
provider = "openai"
harness = "codex"
protocol = "native"
account_ref = "codex-account"
endpoint = { kind="native" }
model = { id="configured-codex-model", author="openai" }
auth = { method="native_managed", profile_ref="codex-profile" }
[[model_access.connections]]
id = "claude-connection"
provider = "anthropic"
harness = "claude-code"
protocol = "native"
account_ref = "claude-account"
endpoint = { kind="native" }
model = { id="configured-claude-model", author="anthropic" }
auth = { method="native_managed", profile_ref="claude-profile" }
[[model_access.pools]]
id = "master-pool"
members = ["api-connection", "codex-connection", "claude-connection"]
projects = ["p", "q"]
roles = ["project_leader"]
fallback_allowed = false
"#;

fn project(name: &str) -> ProjectId {
    ProjectId::new(name).unwrap()
}

fn reference(name: &str) -> CredentialRef {
    CredentialRef::new(name).unwrap()
}

fn selection(backend: MasterBackend) -> MasterSelection {
    let account = match backend {
        MasterBackend::Rcli => "api-account",
        MasterBackend::Codex => "codex-account",
        MasterBackend::Claude => "claude-account",
    };
    MasterSelection {
        backend,
        pool_ref: reference("master-pool"),
        target_profile_ref: reference("sandbox-profile"),
        route: RouteRequest {
            preferred_target: Some(reference("sandbox-profile")),
            required_target: Some(reference("sandbox-profile")),
            required_capabilities: vec!["planning".into(), "engine-tools".into()],
            account_ref: reference(account),
            budget_ref: reference("planning-budget"),
            resources: ResourceBudget::unit(),
        },
    }
}

fn request(id: &str, revision: u64, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        operation,
    }
}

fn mutation(id: &str, revision: u64, action: MasterOwnerAction) -> OwnerRequest {
    request(
        id,
        revision,
        OwnerOperation::Apply {
            mutation: OwnerMutation::ProjectMaster { action },
        },
    )
}

fn applied(reply: OwnerReply) -> OwnerApplied {
    let OwnerResponse::Applied { operation } = reply.response else {
        panic!("real owner mutation did not apply: {reply:?}");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    operation
}

fn rejected(reply: OwnerReply) {
    match reply.response {
        OwnerResponse::Rejected { code } => assert!(!code.is_empty()),
        OwnerResponse::Applied { operation } => {
            assert!(matches!(operation.result, OwnerResult::Rejected { .. }));
        }
        response => panic!("expected real rejection, received {response:?}"),
    }
}

fn queued(turn: &MasterTurnView, session: &MasterSessionView, id: &str) {
    assert_eq!(turn.id, RequestId::new(id).unwrap());
    assert_eq!(turn.session_id, session.session_id);
    assert_eq!(turn.phase, MasterTurnPhase::Queued);
    assert_eq!(turn.reason, None);
    assert_eq!(turn.attempt, None);
    assert_eq!(turn.runtime, None);
    assert_eq!(turn.reply, None);
}

fn database(root: &Path, writable: bool) -> Connection {
    Connection::open_with_flags(
        root.join("engine.db"),
        if writable {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        },
    )
    .unwrap()
}

fn counts(root: &Path) -> (i64, i64, i64, i64) {
    let db = database(root, false);
    let count = |table: &str| -> i64 {
        db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    };
    (
        count("conversation_entries"),
        count("owner_operations"),
        count("events"),
        db.query_row("SELECT revision FROM state WHERE id=1", [], |r| r.get(0))
            .unwrap(),
    )
}

struct Fixture {
    root: tempfile::TempDir,
    service: EngineService,
    stop: watch::Sender<bool>,
}

impl Fixture {
    async fn start(catalog: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-ms-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, catalog).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = watch::channel(false);
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
            stop,
        }
    }

    async fn owner(&self, kind: OwnerClientKind) -> OwnerClient {
        OwnerClient::discover(self.root.path(), kind, Duration::from_secs(3))
            .await
            .unwrap()
            .0
    }

    async fn read(&self, owner: &OwnerClient, query: OwnerRead) -> OwnerReadReply {
        let reply = owner
            .request(request("master-read", 0, OwnerOperation::Read { query }))
            .await
            .unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("actual owner read failed: {reply:?}");
        };
        state
    }

    async fn revision(&self, owner: &OwnerClient) -> u64 {
        self.read(owner, OwnerRead::Capabilities)
            .await
            .revision
            .value()
    }

    async fn basis(&self, owner: &OwnerClient) -> (ConfigId, String) {
        let state = self
            .read(
                owner,
                OwnerRead::ModelAccessPage {
                    collection: ModelAccessCollection::Pools,
                    cursor: None,
                    limit: 16,
                },
            )
            .await;
        let OwnerData::ModelAccessPage { source_hash, .. } = state.data else {
            panic!("actual configured source basis missing");
        };
        (state.config_id, source_hash)
    }

    async fn create(&self, owner: &OwnerClient, name: &str) {
        let reply = owner
            .request(request(
                &format!("create-{name}"),
                self.revision(owner).await,
                OwnerOperation::Apply {
                    mutation: OwnerMutation::CreateProject {
                        project_id: project(name),
                        spec: ProjectSpec {
                            name: name.into(),
                            repositories: vec![],
                        },
                    },
                },
            ))
            .await
            .unwrap();
        applied(reply);
    }

    async fn open_request(
        &self,
        owner: &OwnerClient,
        id: &str,
        name: &str,
        selection: MasterSelection,
    ) -> OwnerRequest {
        let (expected_config_id, expected_source_hash) = self.basis(owner).await;
        mutation(
            id,
            self.revision(owner).await,
            MasterOwnerAction::Open {
                project_id: project(name),
                expected_config_id,
                expected_source_hash,
                selection: Box::new(selection),
            },
        )
    }

    async fn open(
        &self,
        owner: &OwnerClient,
        id: &str,
        name: &str,
        selection: MasterSelection,
    ) -> (OwnerApplied, MasterSessionView) {
        let exact = self.open_request(owner, id, name, selection).await;
        let operation = applied(owner.request(exact).await.unwrap());
        let session = self.session(owner, name).await.unwrap();
        (operation, session)
    }

    async fn session(&self, owner: &OwnerClient, name: &str) -> Option<MasterSessionView> {
        let state = self
            .read(
                owner,
                OwnerRead::ProjectMaster {
                    project_id: project(name),
                },
            )
            .await;
        let OwnerData::ProjectMaster { session } = state.data else {
            panic!("actual master view missing");
        };
        session.map(|session| *session)
    }

    async fn turns(
        &self,
        owner: &OwnerClient,
        session: &MasterSessionView,
        cursor: Option<MasterCursor>,
        limit: u8,
    ) -> OwnerReadReply {
        self.read(
            owner,
            OwnerRead::ProjectMasterTurns {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
                cursor,
                limit,
            },
        )
        .await
    }

    async fn page(&self, owner: &OwnerClient, session: &MasterSessionView) -> MasterTurnPage {
        let state = self.turns(owner, session, None, 32).await;
        let OwnerData::ProjectMasterTurns { page } = state.data else {
            panic!("actual turn page missing");
        };
        page
    }

    async fn settled(
        &self,
        owner: &OwnerClient,
        session: &MasterSessionView,
        id: &str,
    ) -> MasterTurnView {
        let id = RequestId::new(id).unwrap();
        for _ in 0..200 {
            let page = self.page(owner, session).await;
            let turn = page.turns.into_iter().find(|turn| turn.id == id).unwrap();
            if turn.phase != MasterTurnPhase::Queued {
                return turn;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("actual master dispatch left turn {} queued", id.as_str());
    }

    async fn history(&self, owner: &OwnerClient, name: &str) -> ConversationPage {
        let state = self
            .read(
                owner,
                OwnerRead::ConversationHistory {
                    project_id: project(name),
                    cursor: None,
                    limit: 32,
                },
            )
            .await;
        let OwnerData::ConversationHistory { page } = state.data else {
            panic!("actual owner input history missing");
        };
        page
    }

    async fn apply(
        &self,
        owner: &OwnerClient,
        id: &str,
        action: MasterOwnerAction,
    ) -> OwnerApplied {
        applied(
            owner
                .request(mutation(id, self.revision(owner).await, action))
                .await
                .unwrap(),
        )
    }

    async fn submit(
        &self,
        owner: &OwnerClient,
        id: &str,
        session: &MasterSessionView,
        text: &str,
    ) -> OwnerApplied {
        self.apply(
            owner,
            id,
            MasterOwnerAction::Submit {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
                text: text.into(),
            },
        )
        .await
    }

    async fn cancel(&self, owner: &OwnerClient, id: &str, session: &MasterSessionView, turn: &str) {
        self.apply(
            owner,
            id,
            MasterOwnerAction::Cancel {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
                turn_id: RequestId::new(turn).unwrap(),
            },
        )
        .await;
    }

    async fn close(&self, owner: &OwnerClient, id: &str, session: &MasterSessionView) {
        self.apply(
            owner,
            id,
            MasterOwnerAction::Close {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
            },
        )
        .await;
    }

    async fn restart(&mut self) {
        self.service.shutdown().await.unwrap();
        let (stop, stopped) = watch::channel(false);
        self.service = EngineService::start_with_settings_sources(
            self.root.path(),
            "e.sock",
            &self.root.path().join("global.toml"),
            &self.root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        self.stop = stop;
    }

    async fn stop(mut self) {
        self.service.shutdown().await.unwrap();
    }
}

#[test]
fn master_selection_defaults_only_backend_and_preserves_every_route_dimension() {
    let selected = selection(MasterBackend::Rcli);
    let mut wire = serde_json::to_value(&selected).unwrap();
    wire.as_object_mut().unwrap().remove("backend");
    let decoded: MasterSelection = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(decoded, selected);
    assert_eq!(decoded.route.resources, ResourceBudget::unit());
    for required in ["pool_ref", "target_profile_ref", "route"] {
        let mut missing = wire.clone();
        missing.as_object_mut().unwrap().remove(required);
        assert!(serde_json::from_value::<MasterSelection>(missing).is_err());
    }
    for invalid in [Value::Null, json!("worker"), json!("Rcli")] {
        let mut unknown = wire.clone();
        unknown["backend"] = invalid;
        assert!(serde_json::from_value::<MasterSelection>(unknown).is_err());
    }
    wire["admitted_account"] = json!("api-account");
    assert!(serde_json::from_value::<MasterSelection>(wire).is_err());
}

#[test]
fn master_wire_types_are_closed_snake_case_without_producer_actions() {
    for (backend, name) in [
        (MasterBackend::Rcli, "rcli"),
        (MasterBackend::Codex, "codex"),
        (MasterBackend::Claude, "claude"),
    ] {
        assert_eq!(serde_json::to_value(backend).unwrap(), json!(name));
    }
    for phase in ["configured", "closing", "closed"] {
        let value: MasterSessionPhase = serde_json::from_value(json!(phase)).unwrap();
        assert_eq!(serde_json::to_value(value).unwrap(), json!(phase));
    }
    for phase in [
        "queued",
        "blocked",
        "prepared",
        "delivering",
        "running",
        "waiting_approval",
        "succeeded",
        "failed",
        "cancelled",
        "uncertain",
    ] {
        let value: MasterTurnPhase = serde_json::from_value(json!(phase)).unwrap();
        assert_eq!(serde_json::to_value(value).unwrap(), json!(phase));
    }
    for reason in [
        "readiness_unknown",
        "readiness_stale",
        "invalid_authentication",
        "quota_unknown",
        "quota_exhausted",
        "rate_limited",
        "cooldown_active",
        "connection_changed",
        "scope_revoked",
        "capacity_unavailable",
        "budget_exhausted",
        "profile_busy",
        "approval_required",
        "runtime_unavailable",
        "capture_incomplete",
        "output_limit",
        "unsupported_capability",
        "cancelled",
        "unknown",
    ] {
        let value: MasterReason = serde_json::from_value(json!(reason)).unwrap();
        assert_eq!(serde_json::to_value(value).unwrap(), json!(reason));
    }
    assert!(serde_json::from_value::<MasterTurnPhase>(json!("ready")).is_err());
    assert!(serde_json::from_value::<MasterReason>(json!("authenticated")).is_err());
    for kind in ["prepare", "report", "deliver", "observe", "complete"] {
        assert!(serde_json::from_value::<MasterOwnerAction>(json!({"kind":kind})).is_err());
    }
}

#[tokio::test]
async fn master_open_and_submit_precede_development_plan_with_real_applied_input() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    let reader = f.owner(OwnerClientKind::Desktop).await;
    f.create(&owner, "p").await;
    assert_eq!(f.session(&reader, "p").await, None);
    let (opened, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    assert_eq!(session.phase, MasterSessionPhase::Configured);
    assert_eq!(session.selection, selection(MasterBackend::Rcli));
    assert_eq!(session.current_turn, None);
    assert_eq!(session.owner, opened.provenance);
    assert_eq!(session.version, opened.provenance.revision_after);
    let db = database(f.root.path(), false);
    for table in [
        "verification_baselines",
        "coordination_plans",
        "model_gateway_attempts",
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "bootstrap invented {table}");
    }
    drop(db);
    let sent = f
        .submit(
            &owner,
            "requirements",
            &session,
            "Help me clarify the project requirements.",
        )
        .await;
    let page = f.page(&reader, &session).await;
    assert_eq!(page.turns.len(), 1);
    let turn = &page.turns[0];
    queued(turn, &session, "requirements");
    assert_eq!(turn.ordinal, 1);
    let history = f.history(&reader, "p").await;
    assert_eq!(history.entries.len(), 1);
    assert_eq!(
        history.entries[0].text,
        "Help me clarify the project requirements."
    );
    assert_eq!(history.entries[0].content, ConversationContent::Message);
    assert_eq!(history.entries[0].event_cursor, turn.input_cursor);
    assert_eq!(history.entries[0].provenance, sent.provenance);
    assert_eq!(sent.provenance.origin, "native_owner");
    assert_eq!(sent.provenance.operation_id, turn.id);
    assert_eq!(
        sent.provenance.event_cursor,
        Some(turn.input_cursor.clone())
    );
    assert_eq!(
        f.session(&reader, "p").await.unwrap().current_turn,
        Some(turn.id.clone())
    );
    f.stop().await;
}

#[tokio::test]
async fn master_native_backends_store_configured_metadata_without_auth_or_execution() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Desktop).await;
    for (name, backend) in [("p", MasterBackend::Codex), ("q", MasterBackend::Claude)] {
        f.create(&owner, name).await;
        let selected = selection(backend);
        let (_, session) = f
            .open(&owner, &format!("open-{name}"), name, selected.clone())
            .await;
        assert_eq!(session.selection, selected);
        f.submit(&owner, &format!("turn-{name}"), &session, "Plan the work.")
            .await;
        let turn = f.settled(&owner, &session, &format!("turn-{name}")).await;
        assert_eq!(turn.session_id, session.session_id);
        assert_eq!(turn.phase, MasterTurnPhase::Blocked);
        assert_eq!(turn.reason, Some(MasterReason::ConnectionChanged));
        assert_eq!(turn.attempt, None);
        assert_eq!(turn.runtime, None);
        assert_eq!(turn.reply, None);
    }
    let db = database(f.root.path(), false);
    for table in ["model_gateway_observations", "model_gateway_attempts"] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    f.stop().await;
}

#[tokio::test]
async fn master_configured_allowances_do_not_consume_the_single_active_capacity_slot() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    for name in ["p", "q"] {
        f.create(&owner, name).await;
        let (_, session) = f
            .open(
                &owner,
                &format!("open-{name}"),
                name,
                selection(MasterBackend::Rcli),
            )
            .await;
        f.submit(
            &owner,
            &format!("turn-{name}"),
            &session,
            "Owner-authorized planning input.",
        )
        .await;
        queued(
            &f.page(&owner, &session).await.turns[0],
            &session,
            &format!("turn-{name}"),
        );
    }
    let config = resolve_config(
        &f.root.path().join("global.toml"),
        &f.root.path().join("project.toml"),
    )
    .unwrap();
    assert_eq!(config.settings.routing.global_capacity.slots, 1);
    let db = database(f.root.path(), false);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM model_gateway_attempts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    f.stop().await;
}

#[tokio::test]
async fn master_open_and_submit_exact_replay_preserve_one_session_input_and_turn() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    let open = f
        .open_request(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    let first = applied(owner.request(open.clone()).await.unwrap());
    let session = f.session(&owner, "p").await.unwrap();
    let replay = applied(owner.request(open).await.unwrap());
    assert!(replay.historical_replay);
    assert_eq!(replay.provenance, first.provenance);
    assert_eq!(f.session(&owner, "p").await.unwrap(), session);
    let submit = mutation(
        "turn",
        f.revision(&owner).await,
        MasterOwnerAction::Submit {
            project_id: project("p"),
            session_id: session.session_id.clone(),
            text: "Clarify scope.".into(),
        },
    );
    let first = applied(owner.request(submit.clone()).await.unwrap());
    let before = counts(f.root.path());
    let replay = applied(owner.request(submit).await.unwrap());
    assert!(replay.historical_replay);
    assert_eq!(replay.provenance, first.provenance);
    assert_eq!(counts(f.root.path()), before);
    assert_eq!(f.page(&owner, &session).await.turns.len(), 1);
    assert_eq!(f.history(&owner, "p").await.entries.len(), 1);
    f.stop().await;
}

#[tokio::test]
async fn master_conflicting_operation_id_cannot_replace_the_committed_owner_input() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    let exact = mutation(
        "turn",
        f.revision(&owner).await,
        MasterOwnerAction::Submit {
            project_id: project("p"),
            session_id: session.session_id.clone(),
            text: "Original requirement.".into(),
        },
    );
    applied(owner.request(exact.clone()).await.unwrap());
    let mut conflicting = exact;
    let OwnerOperation::Apply {
        mutation:
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Submit { text, .. },
            },
    } = &mut conflicting.operation
    else {
        unreachable!()
    };
    *text = "Conflicting replacement.".into();
    let before = counts(f.root.path());
    rejected(owner.request(conflicting).await.unwrap());
    assert_eq!(counts(f.root.path()), before);
    assert_eq!(
        f.history(&owner, "p").await.entries[0].text,
        "Original requirement."
    );
    assert_eq!(f.page(&owner, &session).await.turns.len(), 1);
    f.stop().await;
}

#[tokio::test]
async fn master_submit_sql_failure_rolls_back_owner_message_turn_and_receipt() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    let db = database(f.root.path(), true);
    db.execute_batch("CREATE TRIGGER fail_master_receipt BEFORE INSERT ON owner_operations WHEN NEW.operation_id='rollback-turn' BEGIN SELECT RAISE(ABORT,'master receipt fault'); END;").unwrap();
    let exact = mutation(
        "rollback-turn",
        f.revision(&owner).await,
        MasterOwnerAction::Submit {
            project_id: project("p"),
            session_id: session.session_id.clone(),
            text: "Must commit atomically.".into(),
        },
    );
    let before = counts(f.root.path());
    rejected(owner.request(exact.clone()).await.unwrap());
    assert_eq!(counts(f.root.path()), before);
    assert!(f.page(&owner, &session).await.turns.is_empty());
    assert!(f.history(&owner, "p").await.entries.is_empty());
    assert_eq!(f.session(&owner, "p").await.unwrap().current_turn, None);
    db.execute_batch("DROP TRIGGER fail_master_receipt")
        .unwrap();
    applied(owner.request(exact).await.unwrap());
    assert_eq!(f.page(&owner, &session).await.turns.len(), 1);
    assert_eq!(f.history(&owner, "p").await.entries.len(), 1);
    f.stop().await;
}

#[tokio::test]
async fn master_one_outstanding_turn_survives_competing_owner_requests() {
    let f = Fixture::start(CATALOG).await;
    let tui = f.owner(OwnerClientKind::Tui).await;
    let desktop = f.owner(OwnerClientKind::Desktop).await;
    f.create(&tui, "p").await;
    let (_, session) = f
        .open(&tui, "open", "p", selection(MasterBackend::Rcli))
        .await;
    let revision = f.revision(&tui).await;
    let action = |text: &str| MasterOwnerAction::Submit {
        project_id: project("p"),
        session_id: session.session_id.clone(),
        text: text.into(),
    };
    let (a, b) = tokio::join!(
        tui.request(mutation("tui-turn", revision, action("TUI requirement."))),
        desktop.request(mutation(
            "desktop-turn",
            revision,
            action("Desktop requirement.")
        )),
    );
    let replies = [a.unwrap(), b.unwrap()];
    assert_eq!(replies.iter().filter(|r| matches!(&r.response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied)).count(), 1);
    for reply in replies {
        if !matches!(&reply.response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied)
        {
            rejected(reply);
        }
    }
    let page = f.page(&tui, &session).await;
    assert_eq!(page.turns.len(), 1);
    assert_eq!(f.history(&desktop, "p").await.entries.len(), 1);
    let second = mutation(
        "second-turn",
        f.revision(&tui).await,
        action("Still outstanding."),
    );
    rejected(tui.request(second).await.unwrap());
    assert_eq!(f.page(&tui, &session).await.turns, page.turns);
    assert_eq!(f.history(&desktop, "p").await.entries.len(), 1);
    f.stop().await;
}

#[tokio::test]
async fn master_only_one_nonclosed_session_exists_per_project() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    let duplicate = f
        .open_request(
            &owner,
            "duplicate-open",
            "p",
            selection(MasterBackend::Claude),
        )
        .await;
    rejected(owner.request(duplicate).await.unwrap());
    assert_eq!(f.session(&owner, "p").await.unwrap(), session);
    f.close(&owner, "close", &session).await;
    let (_, next) = f
        .open(&owner, "next-open", "p", selection(MasterBackend::Claude))
        .await;
    assert_ne!(next.session_id, session.session_id);
    assert_ne!(next.bootstrap_task_id, session.bootstrap_task_id);
    assert_eq!(next.phase, MasterSessionPhase::Configured);
    f.stop().await;
}

#[tokio::test]
async fn master_cancel_before_delivery_is_durable_and_allows_a_new_planning_turn() {
    let mut f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    f.submit(&owner, "turn", &session, "Cancel this planning input.")
        .await;
    f.cancel(&owner, "cancel", &session, "turn").await;
    let cancelled = f.page(&owner, &session).await.turns.remove(0);
    assert_eq!(cancelled.phase, MasterTurnPhase::Cancelled);
    assert_eq!(cancelled.attempt, None);
    assert_eq!(cancelled.runtime, None);
    assert_eq!(cancelled.reply, None);
    assert_eq!(f.session(&owner, "p").await.unwrap().current_turn, None);
    f.restart().await;
    let reader = f.owner(OwnerClientKind::Desktop).await;
    assert_eq!(f.page(&reader, &session).await.turns, vec![cancelled]);
    f.submit(
        &reader,
        "next-turn",
        &session,
        "Use the revised requirement.",
    )
    .await;
    let next = f.settled(&reader, &session, "next-turn").await;
    assert_eq!(next.session_id, session.session_id);
    assert_eq!(next.phase, MasterTurnPhase::Blocked);
    assert_eq!(next.reason, Some(MasterReason::ConnectionChanged));
    assert_eq!(next.attempt, None);
    assert_eq!(next.runtime, None);
    assert_eq!(next.reply, None);
    assert_eq!(next.ordinal, 2);
    assert_eq!(f.page(&reader, &session).await.turns.len(), 2);
    assert_eq!(f.session(&reader, "p").await.unwrap().current_turn, None);
    assert_eq!(f.history(&reader, "p").await.entries.len(), 2);
    f.stop().await;
}

#[tokio::test]
async fn master_close_before_delivery_cancels_queue_without_fabricating_a_reply() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Desktop).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    f.submit(
        &owner,
        "turn",
        &session,
        "Close before genuine preparation.",
    )
    .await;
    f.close(&owner, "close", &session).await;
    let closed = f.session(&owner, "p").await.unwrap();
    assert_eq!(closed.phase, MasterSessionPhase::Closed);
    assert_eq!(closed.current_turn, None);
    let turn = f.page(&owner, &session).await.turns.remove(0);
    assert_eq!(turn.phase, MasterTurnPhase::Cancelled);
    assert_eq!(turn.attempt, None);
    assert_eq!(turn.runtime, None);
    assert_eq!(turn.reply, None);
    rejected(
        owner
            .request(mutation(
                "late-turn",
                f.revision(&owner).await,
                MasterOwnerAction::Submit {
                    project_id: project("p"),
                    session_id: session.session_id.clone(),
                    text: "Must not reopen.".into(),
                },
            ))
            .await
            .unwrap(),
    );
    assert_eq!(f.history(&owner, "p").await.entries.len(), 1);
    f.stop().await;
}

#[tokio::test]
async fn master_restart_retains_queued_input_and_reconciles_the_original_receipt() {
    let mut f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    let exact = mutation(
        "turn",
        f.revision(&owner).await,
        MasterOwnerAction::Submit {
            project_id: project("p"),
            session_id: session.session_id.clone(),
            text: "Durable planning requirement.".into(),
        },
    );
    let first = applied(owner.request(exact.clone()).await.unwrap());
    let saved = f.page(&owner, &session).await;
    f.restart().await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    let replay = applied(owner.request(exact.clone()).await.unwrap());
    assert!(replay.historical_replay);
    assert_eq!(replay.provenance, first.provenance);
    assert_eq!(f.page(&owner, &session).await, saved);
    queued(&saved.turns[0], &session, "turn");
    let reply = owner
        .request(request(
            "reconcile",
            0,
            OwnerOperation::Reconcile {
                operation_id: exact.request_id.clone(),
                expected_hash: Some(exact.mutation_hash().unwrap()),
            },
        ))
        .await
        .unwrap();
    let OwnerResponse::Committed { operation } = reply.response else {
        panic!("real durable reconciliation missing");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    assert_eq!(operation.provenance, first.provenance);
    assert_eq!(
        f.history(&owner, "p").await.entries[0].provenance,
        first.provenance
    );
    f.stop().await;
}

#[tokio::test]
async fn master_open_rejects_missing_project_wrong_config_and_changed_source_basis() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    let missing = f
        .open_request(&owner, "missing", "p", selection(MasterBackend::Rcli))
        .await;
    rejected(owner.request(missing).await.unwrap());
    f.create(&owner, "p").await;
    let mut stale = f
        .open_request(&owner, "stale-config", "p", selection(MasterBackend::Rcli))
        .await;
    let OwnerOperation::Apply {
        mutation:
            OwnerMutation::ProjectMaster {
                action:
                    MasterOwnerAction::Open {
                        expected_config_id, ..
                    },
            },
    } = &mut stale.operation
    else {
        unreachable!()
    };
    *expected_config_id = ConfigId::new("0".repeat(64)).unwrap();
    rejected(owner.request(stale).await.unwrap());
    let exact = f
        .open_request(
            &owner,
            "changed-source",
            "p",
            selection(MasterBackend::Rcli),
        )
        .await;
    let global = f.root.path().join("global.toml");
    let before = resolve_config(&global, &f.root.path().join("project.toml")).unwrap();
    std::fs::write(&global, format!("{CATALOG}\n\n")).unwrap();
    assert_eq!(
        resolve_config(&global, &f.root.path().join("project.toml"))
            .unwrap()
            .id,
        before.id
    );
    rejected(owner.request(exact).await.unwrap());
    std::fs::write(&global, CATALOG).unwrap();
    assert_eq!(f.session(&owner, "p").await, None);
    assert!(f.history(&owner, "p").await.entries.is_empty());
    f.stop().await;
}

#[tokio::test]
async fn master_submit_rechecks_source_even_when_configuration_values_are_unchanged() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    let global = f.root.path().join("global.toml");
    std::fs::write(&global, format!("{CATALOG}\n")).unwrap();
    rejected(
        owner
            .request(mutation(
                "stale-turn",
                f.revision(&owner).await,
                MasterOwnerAction::Submit {
                    project_id: project("p"),
                    session_id: session.session_id.clone(),
                    text: "Changed source must be checked.".into(),
                },
            ))
            .await
            .unwrap(),
    );
    std::fs::write(&global, CATALOG).unwrap();
    assert!(f.page(&owner, &session).await.turns.is_empty());
    assert!(f.history(&owner, "p").await.entries.is_empty());
    f.stop().await;
}

#[tokio::test]
async fn master_worker_only_or_wrong_project_pool_cannot_authorize_a_leader() {
    for catalog in [
        CATALOG.replace("roles = [\"project_leader\"]", "roles = [\"worker\"]"),
        CATALOG.replace("projects = [\"p\", \"q\"]", "projects = [\"q\"]"),
    ] {
        let f = Fixture::start(&catalog).await;
        let owner = f.owner(OwnerClientKind::Tui).await;
        f.create(&owner, "p").await;
        let exact = f
            .open_request(&owner, "denied-open", "p", selection(MasterBackend::Rcli))
            .await;
        rejected(owner.request(exact).await.unwrap());
        assert_eq!(f.session(&owner, "p").await, None);
        assert!(f.history(&owner, "p").await.entries.is_empty());
        f.stop().await;
    }
}

#[tokio::test]
async fn master_submit_rechecks_current_config_and_pool_role_after_an_owner_settings_edit() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    let state = f
        .read(
            &owner,
            OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Pools,
                id: reference("master-pool"),
            },
        )
        .await;
    let OwnerData::ModelAccessRecord {
        source_hash,
        record: ModelAccessRecord::Pool { mut pool },
        ..
    } = state.data
    else {
        panic!("actual owner settings pool missing");
    };
    pool.roles = vec![reference("worker")];
    applied(
        owner
            .request(request(
                "revoke-leader-role",
                f.revision(&owner).await,
                OwnerOperation::Apply {
                    mutation: OwnerMutation::ModelAccess {
                        expected_config_id: state.config_id,
                        expected_source_hash: source_hash,
                        edit: crazyagent::config::ModelAccessEdit::ReplacePool { pool },
                    },
                },
            ))
            .await
            .unwrap(),
    );
    let (current_config, current_source) = f.basis(&owner).await;
    assert_ne!(current_config, session.config_id);
    assert_ne!(current_source, session.source_hash);
    rejected(
        owner
            .request(mutation(
                "revoked-turn",
                f.revision(&owner).await,
                MasterOwnerAction::Submit {
                    project_id: project("p"),
                    session_id: session.session_id.clone(),
                    text: "An old session cannot preserve revoked leader scope.".into(),
                },
            ))
            .await
            .unwrap(),
    );
    let db = database(f.root.path(), false);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM conversation_entries", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM model_gateway_attempts", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    f.stop().await;
}

#[tokio::test]
async fn master_selection_cannot_cross_backend_account_profile_or_approved_pool() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Desktop).await;
    f.create(&owner, "p").await;
    let mut selections = Vec::new();
    let mut value = selection(MasterBackend::Codex);
    value.route.account_ref = reference("api-account");
    selections.push(value);
    let mut value = selection(MasterBackend::Rcli);
    value.pool_ref = reference("unapproved-pool");
    selections.push(value);
    let mut value = selection(MasterBackend::Rcli);
    value.target_profile_ref = reference("unapproved-profile");
    value.route.preferred_target = Some(value.target_profile_ref.clone());
    value.route.required_target = Some(value.target_profile_ref.clone());
    selections.push(value);
    let mut value = selection(MasterBackend::Rcli);
    value.route.resources.slots = 2;
    selections.push(value);
    for (index, selected) in selections.into_iter().enumerate() {
        let exact = f
            .open_request(&owner, &format!("bad-selection-{index}"), "p", selected)
            .await;
        rejected(owner.request(exact).await.unwrap());
        assert_eq!(f.session(&owner, "p").await, None);
    }
    f.stop().await;
}

#[tokio::test]
async fn master_session_actions_cannot_cross_project_or_cancel_another_turn() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    f.create(&owner, "q").await;
    let (_, p) = f
        .open(&owner, "open-p", "p", selection(MasterBackend::Rcli))
        .await;
    let (_, q) = f
        .open(&owner, "open-q", "q", selection(MasterBackend::Rcli))
        .await;
    f.submit(&owner, "p-turn", &p, "Project P requirement.")
        .await;
    f.submit(&owner, "q-turn", &q, "Project Q requirement.")
        .await;
    for (index, action) in [
        MasterOwnerAction::Submit {
            project_id: project("q"),
            session_id: p.session_id.clone(),
            text: "Cross project.".into(),
        },
        MasterOwnerAction::Cancel {
            project_id: project("p"),
            session_id: p.session_id.clone(),
            turn_id: RequestId::new("q-turn").unwrap(),
        },
        MasterOwnerAction::Close {
            project_id: project("q"),
            session_id: p.session_id.clone(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        rejected(
            owner
                .request(mutation(
                    &format!("cross-{index}"),
                    f.revision(&owner).await,
                    action,
                ))
                .await
                .unwrap(),
        );
    }
    queued(&f.page(&owner, &p).await.turns[0], &p, "p-turn");
    queued(&f.page(&owner, &q).await.turns[0], &q, "q-turn");
    assert_eq!(f.history(&owner, "p").await.entries.len(), 1);
    assert_eq!(f.history(&owner, "q").await.entries.len(), 1);
    f.stop().await;
}

#[test]
fn master_submit_text_and_page_limits_validate_utf8_bytes_before_admission() {
    for text in ["".into(), " \t\n".into(), "a\0b".into(), "é".repeat(4097)] {
        let exact = mutation(
            "invalid",
            0,
            MasterOwnerAction::Submit {
                project_id: project("p"),
                session_id: SessionId::new("session").unwrap(),
                text,
            },
        );
        assert!(exact.validate().is_err());
    }
    mutation(
        "maximum",
        0,
        MasterOwnerAction::Submit {
            project_id: project("p"),
            session_id: SessionId::new("session").unwrap(),
            text: "é".repeat(4096),
        },
    )
    .validate()
    .unwrap();
    for limit in [0, 33, u8::MAX] {
        assert!(
            request(
                "invalid-page",
                0,
                OwnerOperation::Read {
                    query: OwnerRead::ProjectMasterTurns {
                        project_id: project("p"),
                        session_id: SessionId::new("session").unwrap(),
                        cursor: None,
                        limit
                    }
                }
            )
            .validate()
            .is_err()
        );
    }
}

#[tokio::test]
async fn master_maximum_utf8_owner_input_is_not_a_model_reply_or_storage_only_send() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Desktop).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    let text = "é".repeat(4096);
    f.submit(&owner, "maximum", &session, &text).await;
    assert_eq!(f.history(&owner, "p").await.entries[0].text, text);
    queued(
        &f.page(&owner, &session).await.turns[0],
        &session,
        "maximum",
    );
    f.cancel(&owner, "cancel", &session, "maximum").await;
    applied(
        owner
            .request(request(
                "append-only",
                f.revision(&owner).await,
                OwnerOperation::Apply {
                    mutation: OwnerMutation::Conversation {
                        mutation: ConversationMutation::AppendMessage {
                            project_id: project("p"),
                            text: "Existing storage-only note.".into(),
                        },
                    },
                },
            ))
            .await
            .unwrap(),
    );
    assert_eq!(f.history(&owner, "p").await.entries.len(), 2);
    assert_eq!(f.page(&owner, &session).await.turns.len(), 1);
    assert_eq!(f.session(&owner, "p").await.unwrap().current_turn, None);
    f.stop().await;
}

#[tokio::test]
async fn master_turn_pages_are_ordered_complete_bounded_and_resync_after_mutation() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    for index in 0..33 {
        let turn = format!("turn-{index}");
        f.submit(&owner, &turn, &session, "Planning page input.")
            .await;
        f.cancel(&owner, &format!("cancel-{index}"), &session, &turn)
            .await;
    }
    let first = f.page(&owner, &session).await;
    assert_eq!(first.project_id, project("p"));
    assert_eq!(first.session_id, session.session_id);
    assert!(!first.turns.is_empty());
    assert!(first.turns.len() <= 32);
    assert!(serde_json::to_vec(&first).unwrap().len() <= 32768);
    let cursor = first
        .next
        .clone()
        .expect("33 turns require another bounded page");
    assert_eq!(cursor.after_ordinal, first.turns.last().unwrap().ordinal);
    let mut turns = first.turns;
    let mut next = Some(cursor.clone());
    while let Some(cursor) = next {
        let state = f.turns(&owner, &session, Some(cursor), 32).await;
        let OwnerData::ProjectMasterTurns { page } = state.data else {
            panic!("same-basis continuation failed");
        };
        assert!(!page.turns.is_empty());
        assert!(page.turns.len() <= 32);
        assert!(serde_json::to_vec(&page).unwrap().len() <= 32768);
        next = page.next;
        turns.extend(page.turns);
        assert!(turns.len() <= 33);
    }
    assert_eq!(turns.len(), 33);
    for (index, turn) in turns.iter().enumerate() {
        assert_eq!(turn.ordinal, u64::try_from(index + 1).unwrap());
        assert_eq!(turn.id, RequestId::new(format!("turn-{index}")).unwrap());
        assert_eq!(turn.phase, MasterTurnPhase::Cancelled);
        assert_eq!(turn.reply, None);
    }
    f.submit(
        &owner,
        "new-input",
        &session,
        "Invalidate the old page basis.",
    )
    .await;
    assert!(matches!(
        f.turns(&owner, &session, Some(cursor), 32).await.data,
        OwnerData::ResyncRequired
    ));
    f.stop().await;
}

#[tokio::test]
async fn master_cursor_binds_database_owner_epochs_project_session_config_source_and_version() {
    let mut f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    let desktop = f.owner(OwnerClientKind::Desktop).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    for index in 0..2 {
        f.submit(&owner, &format!("turn-{index}"), &session, "Cursor basis.")
            .await;
        f.cancel(
            &owner,
            &format!("cancel-{index}"),
            &session,
            &format!("turn-{index}"),
        )
        .await;
    }
    let OwnerData::ProjectMasterTurns { page } = f.turns(&owner, &session, None, 1).await.data
    else {
        panic!("first bounded page missing");
    };
    let cursor = page.next.unwrap();
    let mut variants = Vec::new();
    let mut c = cursor.clone();
    c.db_instance = "0".repeat(64);
    variants.push(c);
    let mut c = cursor.clone();
    c.owner_id = "0".repeat(64);
    variants.push(c);
    let mut c = cursor.clone();
    c.owner_epoch += 1;
    variants.push(c);
    let mut c = cursor.clone();
    c.engine_epoch = "0".repeat(64);
    variants.push(c);
    let mut c = cursor.clone();
    c.project_id = project("q");
    variants.push(c);
    let mut c = cursor.clone();
    c.session_id = SessionId::new("another-session").unwrap();
    variants.push(c);
    let mut c = cursor.clone();
    c.config_id = ConfigId::new("0".repeat(64)).unwrap();
    variants.push(c);
    let mut c = cursor.clone();
    c.source_hash = "0".repeat(64);
    variants.push(c);
    let mut c = cursor.clone();
    c.version = Cursor::new(cursor.version.value() + 1);
    variants.push(c);
    for cursor in variants {
        assert!(matches!(
            f.turns(&owner, &session, Some(cursor), 1).await.data,
            OwnerData::ResyncRequired
        ));
    }
    assert!(matches!(
        f.turns(&desktop, &session, Some(cursor.clone()), 1)
            .await
            .data,
        OwnerData::ResyncRequired
    ));
    f.restart().await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    assert!(matches!(
        f.turns(&owner, &session, Some(cursor), 1).await.data,
        OwnerData::ResyncRequired
    ));
    f.stop().await;
}

#[tokio::test]
async fn master_cursor_rejects_unbounded_ordinal_and_unknown_serialized_fields() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    for index in 0..2 {
        f.submit(
            &owner,
            &format!("turn-{index}"),
            &session,
            "Bounded ordinal.",
        )
        .await;
        f.cancel(
            &owner,
            &format!("cancel-{index}"),
            &session,
            &format!("turn-{index}"),
        )
        .await;
    }
    let OwnerData::ProjectMasterTurns { page } = f.turns(&owner, &session, None, 1).await.data
    else {
        panic!("cursor missing");
    };
    let mut cursor = page.next.unwrap();
    cursor.after_ordinal = u64::MAX;
    assert!(
        request(
            "overflow",
            0,
            OwnerOperation::Read {
                query: OwnerRead::ProjectMasterTurns {
                    project_id: project("p"),
                    session_id: session.session_id.clone(),
                    cursor: Some(cursor.clone()),
                    limit: 1
                }
            }
        )
        .validate()
        .is_err()
    );
    let mut wire = serde_json::to_value(cursor).unwrap();
    wire["worker_binding"] = json!("forged");
    assert!(serde_json::from_value::<MasterCursor>(wire).is_err());
    let mut wire = serde_json::to_value(&session).unwrap();
    wire["approved_plan_id"] = json!("not-a-bootstrap-prerequisite");
    assert!(serde_json::from_value::<MasterSessionView>(wire).is_err());
    let mut wire = serde_json::to_value(&f.page(&owner, &session).await.turns[0]).unwrap();
    wire["ready"] = json!(true);
    assert!(serde_json::from_value::<MasterTurnView>(wire).is_err());
    f.stop().await;
}

#[tokio::test]
async fn master_retained_session_bound_does_not_evict_closed_history() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    let mut first = None;
    for index in 0..64 {
        let (_, session) = f
            .open(
                &owner,
                &format!("open-{index}"),
                "p",
                selection(MasterBackend::Rcli),
            )
            .await;
        if index == 0 {
            first = Some(session.clone());
        }
        f.close(&owner, &format!("close-{index}"), &session).await;
    }
    let overflow = f
        .open_request(&owner, "overflow-open", "p", selection(MasterBackend::Rcli))
        .await;
    rejected(owner.request(overflow).await.unwrap());
    let current = f.session(&owner, "p").await.unwrap();
    assert_eq!(current.phase, MasterSessionPhase::Closed);
    let first = first.unwrap();
    let page = f.page(&owner, &first).await;
    assert_eq!(page.session_id, first.session_id);
    assert!(page.turns.is_empty());
    f.stop().await;
}

#[tokio::test]
async fn master_retained_turn_bound_rejects_1025_without_losing_the_original_input() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    for index in 0..1024 {
        let turn = format!("turn-{index}");
        f.submit(&owner, &turn, &session, "Retained planning input.")
            .await;
        f.cancel(&owner, &format!("cancel-{index}"), &session, &turn)
            .await;
    }
    let before = counts(f.root.path());
    rejected(
        owner
            .request(mutation(
                "overflow-turn",
                f.revision(&owner).await,
                MasterOwnerAction::Submit {
                    project_id: project("p"),
                    session_id: session.session_id.clone(),
                    text: "Must not evict.".into(),
                },
            ))
            .await
            .unwrap(),
    );
    let after = counts(f.root.path());
    assert_eq!(after.0, before.0);
    assert_eq!(after.2, before.2);
    assert_eq!(after.3, before.3);
    let first = f.page(&owner, &session).await;
    assert_eq!(first.turns[0].id, RequestId::new("turn-0").unwrap());
    assert_eq!(first.turns[0].ordinal, 1);
    let db = database(f.root.path(), false);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM conversation_entries", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1024
    );
    assert_eq!(f.session(&owner, "p").await.unwrap().current_turn, None);
    f.stop().await;
}

#[tokio::test]
async fn master_generic_worker_wire_cannot_submit_open_cancel_close_or_internal_reports() {
    let f = Fixture::start(CATALOG).await;
    let owner = f.owner(OwnerClientKind::Tui).await;
    f.create(&owner, "p").await;
    let (_, session) = f
        .open(&owner, "open", "p", selection(MasterBackend::Rcli))
        .await;
    let before = counts(f.root.path());
    let actions = [
        json!({"kind":"open","project_id":"p","expected_config_id":session.config_id,"expected_source_hash":session.source_hash,"selection":session.selection}),
        json!({"kind":"submit","project_id":"p","session_id":session.session_id,"text":"Worker forged owner input."}),
        json!({"kind":"cancel","project_id":"p","session_id":session.session_id,"turn_id":"forged"}),
        json!({"kind":"close","project_id":"p","session_id":session.session_id}),
        json!({"kind":"report","project_id":"p","session_id":session.session_id,"phase":"succeeded"}),
    ];
    for (index, action) in actions.into_iter().enumerate() {
        for kind in ["OwnerProjectMaster", "ProjectMaster"] {
            let wire = json!({"protocol_version":PROTOCOL_VERSION,"client_id":"worker","request_id":format!("forged-{index}"),"expected_revision":before.3.to_string(),"command":{"kind":kind,"action":action}});
            assert!(serde_json::from_value::<WireRequest>(wire.clone()).is_err());
            let bytes = serde_json::to_vec(&wire).unwrap();
            let mut socket = crazyagent::spawn_gate::connect_unix(f.root.path().join("e.sock"))
                .await
                .unwrap();
            socket
                .write_all(&u32::try_from(bytes.len()).unwrap().to_be_bytes())
                .await
                .unwrap();
            socket.write_all(&bytes).await.unwrap();
            socket.shutdown().await.unwrap();
            let mut response = Vec::new();
            let mut bounded = socket.take(65537);
            tokio::time::timeout(Duration::from_secs(3), bounded.read_to_end(&mut response))
                .await
                .unwrap()
                .unwrap();
            assert!(response.len() <= 65536);
            assert_eq!(counts(f.root.path()), before);
        }
    }
    assert!(f.history(&owner, "p").await.entries.is_empty());
    assert!(f.page(&owner, &session).await.turns.is_empty());
    assert_eq!(f.session(&owner, "p").await.unwrap(), session);
    f.stop().await;
}
