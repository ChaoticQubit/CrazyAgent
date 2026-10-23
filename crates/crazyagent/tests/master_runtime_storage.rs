use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, OwnerClient, OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation,
    OwnerRead, OwnerReadReply, OwnerRequest, OwnerResponse, OwnerResult,
};
use crazyagent::master_session::{
    MasterBackend, MasterOwnerAction, MasterSelection, MasterSessionView, MasterTurnPage,
    MasterTurnPhase, MasterTurnView,
};
use crazyagent::protocol::{
    CredentialRef, Cursor, PROTOCOL_VERSION, ProjectId, ProjectSpec, RequestId,
};
use crazyagent::routing::{ResourceBudget, RouteRequest};
use rusqlite::{Connection, OpenFlags, params};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tokio::sync::watch;

const CONFIG: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = ["master-profile"]
[routing]
approved_accounts = ["configured-account"]
global_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
project_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
task_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
[model_access]
accounts = ["configured-account"]
[[model_access.connections]]
id = "configured-codex"
provider = "openai"
harness = "codex"
protocol = "native"
account_ref = "configured-account"
endpoint = { kind = "native" }
model = { id = "configured-model", author = "openai" }
auth = { method = "none" }
[[model_access.pools]]
id = "master-pool"
members = ["configured-codex"]
projects = ["runtime-project"]
roles = ["project_leader"]
fallback_allowed = false
"#;

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn project() -> ProjectId {
    ProjectId::new("runtime-project").unwrap()
}

async fn read(owner: &OwnerClient, query: OwnerRead) -> OwnerReadReply {
    let reply = owner
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("runtime-storage-read").unwrap(),
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Read { query },
        })
        .await
        .unwrap();
    let OwnerResponse::Read { state } = reply.response else {
        panic!("actual owner read failed: {reply:?}");
    };
    state
}

async fn apply(owner: &OwnerClient, id: &str, mutation: OwnerMutation) {
    let basis = read(owner, OwnerRead::Capabilities).await;
    let reply = owner
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(id).unwrap(),
            expected_revision: basis.revision.clone(),
            operation: OwnerOperation::Apply { mutation },
        })
        .await
        .unwrap();
    let OwnerResponse::Applied { operation } = reply.response else {
        panic!("actual owner mutation failed: {reply:?}");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    assert!(operation.current_revision.value() > basis.revision.value());
}

struct Fixture {
    root: tempfile::TempDir,
    service: EngineService,
    owner: OwnerClient,
    _stop: watch::Sender<bool>,
}

impl Fixture {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-mrs-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, CONFIG).unwrap();
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
        let owner = OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(3))
            .await
            .unwrap()
            .0;
        assert!(root.path().join("engine.db").is_file());
        assert!(service.owner_runtime().is_some());
        Self {
            root,
            service,
            owner,
            _stop: stop,
        }
    }

    async fn queued(&self) -> (MasterSessionView, MasterTurnView) {
        apply(
            &self.owner,
            "create-runtime-project",
            OwnerMutation::CreateProject {
                project_id: project(),
                spec: ProjectSpec {
                    name: "Runtime storage".into(),
                    repositories: vec![],
                },
            },
        )
        .await;
        let basis = read(
            &self.owner,
            OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Pools,
                cursor: None,
                limit: 1,
            },
        )
        .await;
        let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
            panic!("actual configured source basis missing");
        };
        apply(
            &self.owner,
            "open-runtime-master",
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Open {
                    project_id: project(),
                    expected_config_id: basis.config_id,
                    expected_source_hash: source_hash,
                    selection: MasterSelection {
                        backend: MasterBackend::Codex,
                        pool_ref: reference("master-pool"),
                        target_profile_ref: reference("master-profile"),
                        route: RouteRequest {
                            preferred_target: Some(reference("master-profile")),
                            required_target: Some(reference("master-profile")),
                            required_capabilities: vec!["planning".into()],
                            account_ref: reference("configured-account"),
                            budget_ref: reference("planning-budget"),
                            resources: ResourceBudget::unit(),
                        },
                    },
                },
            },
        )
        .await;
        self.submit().await
    }

    async fn session(&self) -> MasterSessionView {
        let state = read(
            &self.owner,
            OwnerRead::ProjectMaster {
                project_id: project(),
            },
        )
        .await;
        let OwnerData::ProjectMaster {
            session: Some(session),
        } = state.data
        else {
            panic!("actual committed master session missing");
        };
        assert_eq!(session.project_id, project());
        assert_eq!(session.version, state.revision);
        session
    }

    async fn page(&self, session: &MasterSessionView) -> MasterTurnPage {
        let state = read(
            &self.owner,
            OwnerRead::ProjectMasterTurns {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
                cursor: None,
                limit: 1,
            },
        )
        .await;
        let OwnerData::ProjectMasterTurns { page } = state.data else {
            panic!("actual committed master turn missing");
        };
        assert_eq!(page.project_id, session.project_id);
        assert_eq!(page.session_id, session.session_id);
        page
    }

    async fn submit(&self) -> (MasterSessionView, MasterTurnView) {
        let session = self.session().await;
        apply(
            &self.owner,
            "submit-runtime-turn",
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Submit {
                    project_id: session.project_id.clone(),
                    session_id: session.session_id.clone(),
                    text: "Persist this owner input without claiming model readiness.".into(),
                },
            },
        )
        .await;
        let session = self.session().await;
        let page = self.page(&session).await;
        assert_eq!(page.turns.len(), 1);
        let turn = page.turns.into_iter().next().unwrap();
        assert_eq!(turn.phase, MasterTurnPhase::Queued);
        assert_eq!(turn.reason, None);
        assert_eq!(turn.attempt, None);
        assert_eq!(turn.runtime, None);
        assert_eq!(turn.reply, None);
        assert_eq!(session.current_turn.as_ref(), Some(&turn.id));
        assert_eq!(turn.input_cursor, session.version);
        (session, turn)
    }
}

fn probe_phase(
    db: &mut Connection,
    session: &MasterSessionView,
    turn: &MasterTurnView,
    phase: &str,
) -> Result<(), String> {
    let tx = db.transaction().unwrap();
    let result = match tx.execute(
        "UPDATE master_turns SET phase=?1 WHERE session=?2 AND id=?3",
        params![phase, session.session_id.as_str(), turn.id.as_str()],
    ) {
        Ok(changed) => {
            assert_eq!(changed, 1);
            let stored: String = tx
                .query_row(
                    "SELECT phase FROM master_turns WHERE session=?1 AND id=?2",
                    params![session.session_id.as_str(), turn.id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(stored, phase);
            Ok(())
        }
        Err(error) => Err(format!("actual SQLite rejected master phase {phase}: {error}")),
    };
    tx.rollback().unwrap();
    assert!(db.is_autocommit());
    result
}

#[tokio::test]
async fn real_sqlite_master_turn_schema_accepts_runtime_lifecycle_phases() {
    let mut fixture = Fixture::start().await;
    let (session, turn) = fixture.queued().await;
    let mut db = Connection::open_with_flags(
        fixture.root.path().join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    db.busy_timeout(Duration::from_millis(250)).unwrap();
    assert_eq!(turn.session_id, session.session_id);
    assert_eq!(turn.id.as_str(), "submit-runtime-turn");
    let mut rejected = Vec::new();
    for phase in [
        "prepared",
        "running",
        "succeeded",
        "failed",
        "cancelled",
        "uncertain",
        "blocked",
    ] {
        if let Err(error) = probe_phase(&mut db, &session, &turn, phase) {
            rejected.push(error);
        }
    }
    assert_eq!(fixture.session().await, session);
    assert_eq!(fixture.page(&session).await.turns, vec![turn]);
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM conversation_entries WHERE kind='message'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    drop(db);
    fixture.service.shutdown().await.unwrap();
    assert!(
        rejected.is_empty(),
        "actual initialized SQLite cannot store the master lifecycle: {rejected:?}"
    );
}
