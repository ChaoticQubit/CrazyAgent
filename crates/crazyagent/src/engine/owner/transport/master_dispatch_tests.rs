use super::master_dispatch::MasterDispatch;
use super::*;
use crate::engine::bootstrap::EngineService;
use crate::master_session::{
    MasterBackend, MasterOwnerAction, MasterSelection, MasterSessionView, MasterTurnPhase,
    MasterTurnView,
};
use crate::routing::{ResourceBudget, RouteRequest};
use std::os::unix::fs::PermissionsExt;
use tokio::sync::watch;

const CATALOG: &str = r#"
version = 1
timeout_ms = 2000
allowed_destinations = ["sandbox-profile"]
[routing]
approved_accounts = ["claude-account"]
[routing.global_capacity]
slots = 1
cpu_millis = 1000
memory_mib = 512
processes = 4
output_bytes = 1048576
wall_time_ms = 60000
spend_microunits = 0
[routing.project_capacity]
slots = 1
cpu_millis = 1000
memory_mib = 512
processes = 4
output_bytes = 1048576
wall_time_ms = 60000
spend_microunits = 0
[routing.task_capacity]
slots = 1
cpu_millis = 1000
memory_mib = 512
processes = 4
output_bytes = 1048576
wall_time_ms = 60000
spend_microunits = 0
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
[[model_access.pools]]
id = "master-pool"
members = ["claude-connection"]
projects = ["p"]
roles = ["project_leader"]
fallback_allowed = false
"#;

fn request(id: &str, revision: u64, operation: OwnerOperation) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        operation,
    }
}

fn selection() -> MasterSelection {
    let target = CredentialRef::new("sandbox-profile").unwrap();
    MasterSelection {
        backend: MasterBackend::Claude,
        pool_ref: CredentialRef::new("master-pool").unwrap(),
        target_profile_ref: target.clone(),
        route: RouteRequest {
            preferred_target: Some(target.clone()),
            required_target: Some(target),
            required_capabilities: vec!["planning".into(), "engine-tools".into()],
            account_ref: CredentialRef::new("claude-account").unwrap(),
            budget_ref: CredentialRef::new("planning-budget").unwrap(),
            resources: ResourceBudget::unit(),
        },
    }
}

fn operation(reply: &OwnerReply) -> &OwnerApplied {
    let OwnerResponse::Applied { operation } = &reply.response else {
        panic!("actual committed operation missing: {reply:?}");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    operation
}

struct DispatchFixture {
    root: tempfile::TempDir,
    service: EngineService,
    owner: OwnerClient,
    _stop: watch::Sender<bool>,
}

impl DispatchFixture {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-mdb-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        std::fs::write(&global, CATALOG).unwrap();
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
        Self {
            root,
            service,
            owner,
            _stop: stop,
        }
    }

    fn runtime(&self) -> OwnerRuntimeHandle {
        let retained = self.service.owner_runtime().unwrap();
        OwnerRuntimeHandle {
            engine: Arc::clone(&retained.engine),
            authority: Arc::clone(&retained.authority),
        }
    }

    async fn read(&self, query: OwnerRead) -> OwnerReadReply {
        let reply = self
            .owner
            .request(request("dispatch-read", 0, OwnerOperation::Read { query }))
            .await
            .unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("actual owner read failed: {reply:?}");
        };
        state
    }

    async fn revision(&self) -> u64 {
        self.read(OwnerRead::Capabilities).await.revision.value()
    }

    async fn commit(&self, request: &OwnerRequest) -> OwnerReply {
        let runtime = self.runtime();
        let mut auth = [0; 37];
        auth[..4].copy_from_slice(b"CAO1");
        auth[4] = 0;
        auth[5..].copy_from_slice(&self.owner.token);
        let admission = runtime.authority.authenticate(&auth).unwrap();
        let _barrier = runtime.engine.owner_egress.write().await;
        runtime
            .engine
            .dispatch_owner(admission, request.clone(), None)
            .await
            .unwrap()
    }

    async fn action(&self, id: &str, action: MasterOwnerAction) -> (OwnerRequest, OwnerReply) {
        let request = request(
            id,
            self.revision().await,
            OwnerOperation::Apply {
                mutation: OwnerMutation::ProjectMaster { action },
            },
        );
        let reply = self.commit(&request).await;
        (request, reply)
    }

    async fn create(&self) -> (OwnerRequest, OwnerReply) {
        let request = request(
            "dispatch-project",
            self.revision().await,
            OwnerOperation::Apply {
                mutation: OwnerMutation::CreateProject {
                    project_id: ProjectId::new("p").unwrap(),
                    spec: ProjectSpec {
                        name: "dispatch project".into(),
                        repositories: vec![],
                    },
                },
            },
        );
        let reply = self.owner.request(request.clone()).await.unwrap();
        operation(&reply);
        (request, reply)
    }

    async fn open(&self) -> (OwnerRequest, OwnerReply, MasterSessionView) {
        let basis = self
            .read(OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Pools,
                cursor: None,
                limit: 16,
            })
            .await;
        let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
            panic!("actual model source missing");
        };
        let (request, reply) = self
            .action(
                "dispatch-open",
                MasterOwnerAction::Open {
                    project_id: ProjectId::new("p").unwrap(),
                    expected_config_id: basis.config_id,
                    expected_source_hash: source_hash,
                    selection: selection(),
                },
            )
            .await;
        operation(&reply);
        let state = self
            .read(OwnerRead::ProjectMaster {
                project_id: ProjectId::new("p").unwrap(),
            })
            .await;
        let OwnerData::ProjectMaster {
            session: Some(session),
        } = state.data
        else {
            panic!("actual configured master session missing");
        };
        (request, reply, session)
    }

    async fn submit(&self, session: &MasterSessionView, id: &str) -> (OwnerRequest, OwnerReply) {
        self.action(
            id,
            MasterOwnerAction::Submit {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
                text: "Actual committed bounded input, with no admitted native profile.".into(),
            },
        )
        .await
    }

    async fn turn(&self, session: &MasterSessionView) -> MasterTurnView {
        let state = self
            .read(OwnerRead::ProjectMasterTurns {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
                cursor: None,
                limit: 16,
            })
            .await;
        let OwnerData::ProjectMasterTurns { mut page } = state.data else {
            panic!("actual turn page missing");
        };
        assert_eq!(page.turns.len(), 1);
        page.turns.remove(0)
    }

    async fn await_blocked(&self, session: &MasterSessionView) -> MasterTurnView {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let turn = self.turn(session).await;
                if turn.phase == MasterTurnPhase::Blocked {
                    return turn;
                }
                assert_eq!(turn.phase, MasterTurnPhase::Queued);
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("real driver never reached the unadmitted Core result")
    }

    fn counts(&self) -> (i64, i64) {
        let database = rusqlite::Connection::open_with_flags(
            self.root.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let count = |table: &str| {
            database
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
                .unwrap()
        };
        (count("master_turns"), count("model_gateway_attempts"))
    }

    async fn finish(mut self) {
        self.service.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn master_dispatch_open_and_unrelated_commit_launch_no_turn() {
    let fixture = DispatchFixture::start().await;
    let mut dispatcher = MasterDispatch::new();
    let (create, created) = fixture.create().await;
    assert!(!dispatcher.committed(fixture.runtime(), &create, &created).unwrap());
    let (open, opened, session) = fixture.open().await;
    assert!(session.current_turn.is_none());
    assert!(!dispatcher.committed(fixture.runtime(), &open, &opened).unwrap());
    dispatcher.shutdown().await.unwrap();
    assert_eq!(fixture.counts(), (0, 0));
    fixture.finish().await;
}

#[tokio::test]
async fn master_dispatch_fresh_submit_invokes_real_driver_and_preserves_unknown_admission() {
    let fixture = DispatchFixture::start().await;
    fixture.create().await;
    let (_, _, session) = fixture.open().await;
    let (submit, submitted) = fixture.submit(&session, "dispatch-turn").await;
    operation(&submitted);
    assert_eq!(fixture.turn(&session).await.phase, MasterTurnPhase::Queued);
    let mut dispatcher = MasterDispatch::new();
    assert!(dispatcher.committed(fixture.runtime(), &submit, &submitted).unwrap());
    let turn = fixture.await_blocked(&session).await;
    assert_eq!(turn.id, submit.request_id);
    assert_eq!(turn.session_id, session.session_id);
    assert!(turn.reason.is_some());
    assert!(turn.attempt.is_none());
    assert!(turn.runtime.is_none());
    assert!(turn.reply.is_none());
    dispatcher.shutdown().await.unwrap();
    assert_eq!(fixture.counts(), (1, 0));
    fixture.finish().await;
}

#[tokio::test]
async fn master_dispatch_duplicate_and_actual_replay_never_invoke_a_second_driver() {
    let fixture = DispatchFixture::start().await;
    fixture.create().await;
    let (_, _, session) = fixture.open().await;
    let (submit, submitted) = fixture.submit(&session, "dispatch-turn").await;
    operation(&submitted);
    let mut dispatcher = MasterDispatch::new();
    assert!(dispatcher.committed(fixture.runtime(), &submit, &submitted).unwrap());
    assert!(!dispatcher.committed(fixture.runtime(), &submit, &submitted).unwrap());
    fixture.await_blocked(&session).await;
    let replay = fixture.commit(&submit).await;
    assert!(operation(&replay).historical_replay);
    assert!(!dispatcher.committed(fixture.runtime(), &submit, &replay).unwrap());
    assert!(!dispatcher.committed(fixture.runtime(), &submit, &submitted).unwrap());
    dispatcher.shutdown().await.unwrap();
    assert_eq!(fixture.counts(), (1, 0));
    fixture.finish().await;
}

#[tokio::test]
async fn master_dispatch_actual_rejected_submit_and_read_launch_no_driver() {
    let fixture = DispatchFixture::start().await;
    fixture.create().await;
    let (_, _, session) = fixture.open().await;
    let (_, accepted) = fixture.submit(&session, "dispatch-turn").await;
    operation(&accepted);
    let (busy, rejected) = fixture.submit(&session, "dispatch-busy").await;
    let OwnerResponse::Applied { operation } = &rejected.response else {
        panic!("actual rejection operation missing: {rejected:?}");
    };
    assert!(matches!(operation.result, OwnerResult::Rejected { .. }));
    let mut dispatcher = MasterDispatch::new();
    assert!(!dispatcher.committed(fixture.runtime(), &busy, &rejected).unwrap());
    let read = request(
        "dispatch-capabilities",
        0,
        OwnerOperation::Read {
            query: OwnerRead::Capabilities,
        },
    );
    let read_reply = fixture.owner.request(read.clone()).await.unwrap();
    assert!(matches!(read_reply.response, OwnerResponse::Read { .. }));
    assert!(!dispatcher.committed(fixture.runtime(), &read, &read_reply).unwrap());
    dispatcher.shutdown().await.unwrap();
    assert_eq!(fixture.turn(&session).await.phase, MasterTurnPhase::Queued);
    assert_eq!(fixture.counts(), (1, 0));
    fixture.finish().await;
}

#[tokio::test]
async fn master_dispatch_rejects_a_real_reply_from_another_operation() {
    let fixture = DispatchFixture::start().await;
    fixture.create().await;
    let (_, opened, session) = fixture.open().await;
    let (submit, submitted) = fixture.submit(&session, "dispatch-turn").await;
    operation(&submitted);
    let mut dispatcher = MasterDispatch::new();
    assert!(dispatcher.committed(fixture.runtime(), &submit, &opened).is_err());
    assert_eq!(fixture.turn(&session).await.phase, MasterTurnPhase::Queued);
    assert!(dispatcher.committed(fixture.runtime(), &submit, &submitted).unwrap());
    fixture.await_blocked(&session).await;
    dispatcher.shutdown().await.unwrap();
    assert_eq!(fixture.counts(), (1, 0));
    fixture.finish().await;
}

#[tokio::test]
async fn master_dispatch_rejects_an_actual_committed_reply_from_a_foreign_owner_host() {
    let fixture = DispatchFixture::start().await;
    let foreign = DispatchFixture::start().await;
    foreign.create().await;
    let (_, _, session) = foreign.open().await;
    let (submit, submitted) = foreign.submit(&session, "dispatch-turn").await;
    operation(&submitted);
    let mut dispatcher = MasterDispatch::new();
    assert!(dispatcher.committed(fixture.runtime(), &submit, &submitted).is_err());
    dispatcher.shutdown().await.unwrap();
    assert_eq!(fixture.counts(), (0, 0));
    assert_eq!(foreign.turn(&session).await.phase, MasterTurnPhase::Queued);
    assert_eq!(foreign.counts(), (1, 0));
    foreign.finish().await;
    fixture.finish().await;
}

#[tokio::test]
async fn master_dispatch_actual_cancel_before_driver_start_remains_cancelled() {
    let fixture = DispatchFixture::start().await;
    fixture.create().await;
    let (_, _, session) = fixture.open().await;
    let (submit, submitted) = fixture.submit(&session, "dispatch-turn").await;
    operation(&submitted);
    let (cancel, cancelled) = fixture
        .action(
            "dispatch-cancel",
            MasterOwnerAction::Cancel {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
                turn_id: submit.request_id.clone(),
            },
        )
        .await;
    operation(&cancelled);
    let mut dispatcher = MasterDispatch::new();
    assert!(!dispatcher.committed(fixture.runtime(), &cancel, &cancelled).unwrap());
    assert!(dispatcher.committed(fixture.runtime(), &submit, &submitted).unwrap());
    dispatcher.shutdown().await.unwrap();
    let turn = fixture.turn(&session).await;
    assert_eq!(turn.phase, MasterTurnPhase::Cancelled);
    assert!(turn.attempt.is_none());
    assert!(turn.runtime.is_none());
    assert!(turn.reply.is_none());
    assert_eq!(fixture.counts(), (1, 0));
    fixture.finish().await;
}

#[tokio::test]
async fn master_dispatch_shutdown_joins_a_blocked_driver_and_reports_cleanup_uncertainty() {
    let fixture = DispatchFixture::start().await;
    fixture.create().await;
    let (_, _, session) = fixture.open().await;
    let (submit, submitted) = fixture.submit(&session, "dispatch-turn").await;
    operation(&submitted);
    let runtime = fixture.runtime();
    let engine = Arc::downgrade(&runtime.engine);
    let barrier = runtime.engine.owner_egress.write().await;
    let mut dispatcher = MasterDispatch::new();
    assert!(dispatcher.committed(fixture.runtime(), &submit, &submitted).unwrap());
    let result = tokio::time::timeout(Duration::from_secs(5), dispatcher.shutdown())
        .await
        .expect("retained driver shutdown exceeded the test bound");
    assert_eq!(result.unwrap_err().0, "master_dispatch_cleanup_uncertain");
    drop(barrier);
    assert_eq!(fixture.turn(&session).await.phase, MasterTurnPhase::Queued);
    assert_eq!(fixture.counts(), (1, 0));
    drop(dispatcher);
    drop(runtime);
    fixture.finish().await;
    assert!(engine.upgrade().is_none());
}

#[tokio::test]
async fn master_dispatch_shutdown_prevents_a_later_committed_turn_launch() {
    let fixture = DispatchFixture::start().await;
    fixture.create().await;
    let (_, _, session) = fixture.open().await;
    let (submit, submitted) = fixture.submit(&session, "dispatch-turn").await;
    operation(&submitted);
    let mut dispatcher = MasterDispatch::new();
    dispatcher.shutdown().await.unwrap();
    assert!(dispatcher.committed(fixture.runtime(), &submit, &submitted).is_err());
    assert_eq!(fixture.turn(&session).await.phase, MasterTurnPhase::Queued);
    assert_eq!(fixture.counts(), (1, 0));
    fixture.finish().await;
}

#[tokio::test]
async fn master_dispatch_drop_aborts_owned_pending_work_instead_of_detaching_it() {
    let fixture = DispatchFixture::start().await;
    fixture.create().await;
    let (_, _, session) = fixture.open().await;
    let (submit, submitted) = fixture.submit(&session, "dispatch-turn").await;
    operation(&submitted);
    let runtime = fixture.runtime();
    let engine = Arc::downgrade(&runtime.engine);
    let barrier = runtime.engine.owner_egress.write().await;
    let mut dispatcher = MasterDispatch::new();
    assert!(dispatcher.committed(fixture.runtime(), &submit, &submitted).unwrap());
    tokio::time::sleep(Duration::from_millis(20)).await;
    drop(dispatcher);
    drop(barrier);
    assert_eq!(fixture.turn(&session).await.phase, MasterTurnPhase::Queued);
    assert_eq!(fixture.counts(), (1, 0));
    drop(runtime);
    fixture.finish().await;
    assert!(engine.upgrade().is_none());
}
