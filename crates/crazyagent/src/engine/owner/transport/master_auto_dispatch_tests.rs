use super::*;
use crate::engine::bootstrap::EngineService;
use crate::master_session::{
    MasterBackend, MasterOwnerAction, MasterReason, MasterSelection, MasterSessionView,
    MasterTurnPhase, MasterTurnView,
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

fn receipt(reply: &OwnerReply) -> &OwnerApplied {
    let OwnerResponse::Applied { operation } = &reply.response else {
        panic!("actual owner receipt missing: {reply:?}");
    };
    operation
}

struct AutoDispatchCase {
    root: tempfile::TempDir,
    service: EngineService,
    owner: OwnerClient,
    _stop: watch::Sender<bool>,
}

impl AutoDispatchCase {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-mad-")
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

    async fn read(&self, query: OwnerRead) -> OwnerReadReply {
        let reply = self
            .owner
            .request(request("auto-read", 0, OwnerOperation::Read { query }))
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

    async fn action_request(&self, id: &str, action: MasterOwnerAction) -> OwnerRequest {
        request(
            id,
            self.revision().await,
            OwnerOperation::Apply {
                mutation: OwnerMutation::ProjectMaster { action },
            },
        )
    }

    async fn open(&self) -> MasterSessionView {
        let create = request(
            "auto-project",
            self.revision().await,
            OwnerOperation::Apply {
                mutation: OwnerMutation::CreateProject {
                    project_id: ProjectId::new("p").unwrap(),
                    spec: ProjectSpec {
                        name: "automatic dispatch project".into(),
                        repositories: vec![],
                    },
                },
            },
        );
        let created = self.owner.request(create).await.unwrap();
        assert_eq!(receipt(&created).result, OwnerResult::Applied);
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
        let target = CredentialRef::new("sandbox-profile").unwrap();
        let open = self
            .action_request(
                "auto-open",
                MasterOwnerAction::Open {
                    project_id: ProjectId::new("p").unwrap(),
                    expected_config_id: basis.config_id,
                    expected_source_hash: source_hash,
                    selection: MasterSelection {
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
                    },
                },
            )
            .await;
        let opened = self.owner.request(open).await.unwrap();
        assert_eq!(receipt(&opened).result, OwnerResult::Applied);
        self.session().await
    }

    async fn session(&self) -> MasterSessionView {
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
        session
    }

    async fn submit_request(&self, session: &MasterSessionView, id: &str) -> OwnerRequest {
        self.action_request(
            id,
            MasterOwnerAction::Submit {
                project_id: session.project_id.clone(),
                session_id: session.session_id.clone(),
                text: "Real synthetic input for the unsupported configured backend.".into(),
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

    async fn await_blocked(
        &self,
        session: &MasterSessionView,
    ) -> Result<MasterTurnView, tokio::time::error::Elapsed> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let turn = self.turn(session).await;
                if turn.phase == MasterTurnPhase::Blocked {
                    return turn;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
    }

    fn database(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.root.path().join("engine.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn counts(&self) -> (i64, i64, i64) {
        let db = self.database();
        let count = |table: &str| {
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
                .unwrap()
        };
        (
            count("master_turns"),
            count("master_preparations"),
            count("model_gateway_attempts"),
        )
    }

    async fn reconcile(&self, submit: &OwnerRequest) -> OwnerApplied {
        let reply = self
            .owner
            .request(request(
                "auto-reconcile",
                0,
                OwnerOperation::Reconcile {
                    operation_id: submit.request_id.clone(),
                    expected_hash: Some(submit.mutation_hash().unwrap()),
                },
            ))
            .await
            .unwrap();
        let OwnerResponse::Committed { operation } = reply.response else {
            panic!("actual committed operation not reconciled: {reply:?}");
        };
        operation
    }

    async fn stop(&mut self) -> Result<(), EngineError> {
        tokio::time::timeout(Duration::from_secs(5), self.service.shutdown())
            .await
            .expect("actual owner shutdown exceeded five seconds")
    }
}

fn assert_blocked(turn: &MasterTurnView, submit: &OwnerRequest, session: &MasterSessionView) {
    assert_eq!(turn.id, submit.request_id);
    assert_eq!(turn.session_id, session.session_id);
    assert_eq!(turn.phase, MasterTurnPhase::Blocked);
    assert_eq!(turn.reason, Some(MasterReason::UnsupportedCapability));
    assert!(turn.attempt.is_none());
    assert!(turn.runtime.is_none());
    assert!(turn.reply.is_none());
}

#[tokio::test]
async fn master_auto_dispatch_fresh_socket_submit_is_readably_blocked_before_shutdown() {
    let mut case = AutoDispatchCase::start().await;
    let session = case.open().await;
    let submit = case.submit_request(&session, "auto-turn").await;
    let submitted = case.owner.request(submit.clone()).await.unwrap();
    let reached = case.await_blocked(&session).await;
    let visible = case.turn(&session).await;
    let current = case.session().await;
    let counts = case.counts();
    let stopped = case.stop().await;
    stopped.unwrap();
    assert_eq!(receipt(&submitted).result, OwnerResult::Applied);
    assert!(!receipt(&submitted).historical_replay);
    assert!(
        reached.is_ok(),
        "fresh socket Submit stayed {:?}",
        visible.phase
    );
    assert_blocked(&visible, &submit, &session);
    assert!(current.current_turn.is_none());
    assert_eq!(counts, (1, 0, 0));
}

#[tokio::test]
async fn master_auto_dispatch_rejected_submit_and_actual_replays_have_no_second_effect() {
    let mut case = AutoDispatchCase::start().await;
    let session = case.open().await;
    let empty = case.counts();
    let before_rejected = case.revision().await;
    let mut stale = case.submit_request(&session, "auto-stale").await;
    stale.expected_revision = Cursor::new(0);
    let rejected = case.owner.request(stale.clone()).await.unwrap();
    let rejected_replay = case.owner.request(stale).await.unwrap();
    let after_rejected = case.revision().await;
    let rejected_counts = case.counts();
    let submit = case.submit_request(&session, "auto-turn").await;
    let submitted = case.owner.request(submit.clone()).await.unwrap();
    let reached = case.await_blocked(&session).await;
    let before_replays = case.revision().await;
    let turn_before = case.turn(&session).await;
    let replay = case.owner.request(submit.clone()).await.unwrap();
    let duplicate = case.owner.request(submit.clone()).await.unwrap();
    let reconciled = case.reconcile(&submit).await;
    let after_replays = case.revision().await;
    let turn_after = case.turn(&session).await;
    let counts = case.counts();
    let stopped = case.stop().await;
    stopped.unwrap();
    assert_eq!(empty, (0, 0, 0));
    assert_eq!(
        receipt(&rejected).result,
        OwnerResult::Rejected {
            code: "stale_revision".into()
        }
    );
    assert!(!receipt(&rejected).historical_replay);
    assert!(receipt(&rejected).provenance.event_cursor.is_none());
    assert!(receipt(&rejected_replay).historical_replay);
    assert_eq!(
        receipt(&rejected_replay).provenance,
        receipt(&rejected).provenance
    );
    assert_eq!(before_rejected, after_rejected);
    assert_eq!(rejected_counts, (0, 0, 0));
    assert!(reached.is_ok(), "fresh socket Submit never became Blocked");
    assert_eq!(receipt(&submitted).result, OwnerResult::Applied);
    for actual in [receipt(&replay), receipt(&duplicate)] {
        assert!(actual.historical_replay);
        assert_eq!(actual.provenance, receipt(&submitted).provenance);
    }
    assert_eq!(reconciled.provenance, receipt(&submitted).provenance);
    assert_eq!(before_replays, after_replays);
    assert_eq!(turn_before, turn_after);
    assert_blocked(&turn_after, &submit, &session);
    assert_eq!(counts, (1, 0, 0));
}

#[tokio::test]
async fn master_auto_dispatch_closed_actor_reply_still_reconciles_actual_commit() {
    let mut case = AutoDispatchCase::start().await;
    let session = case.open().await;
    let submit = case.submit_request(&session, "auto-lost-reply").await;
    {
        let runtime = case.service.owner_runtime().unwrap();
        let mut auth = [0; 37];
        auth[..4].copy_from_slice(b"CAO1");
        auth[4] = 0;
        auth[5..].copy_from_slice(&case.owner.token);
        let admission = runtime.authority.authenticate(&auth).unwrap();
        let _barrier = runtime.engine.owner_egress.write().await;
        let (reply, receive) = oneshot::channel();
        drop(receive);
        runtime
            .engine
            .storage
            .sender
            .try_send(Job::OwnerApply(admission, submit.clone(), reply))
            .unwrap_or_else(|_| panic!("real actor queue rejected the test job"));
    }
    let reconciled = case.reconcile(&submit).await;
    let counts = case.counts();
    let stopped = case.stop().await;
    stopped.unwrap();
    assert_eq!(reconciled.result, OwnerResult::Applied);
    assert_eq!(reconciled.provenance.operation_id, submit.request_id);
    assert_eq!(
        reconciled.provenance.request_hash,
        submit.mutation_hash().unwrap()
    );
    assert_eq!(
        reconciled.provenance.revision_before,
        submit.expected_revision
    );
    assert_eq!(
        reconciled.provenance.revision_after.value(),
        submit.expected_revision.value() + 1
    );
    assert_eq!(
        reconciled.provenance.event_cursor.as_ref(),
        Some(&reconciled.provenance.revision_after)
    );
    assert_eq!(counts, (1, 0, 0));
}

#[tokio::test]
async fn master_auto_dispatch_acknowledged_socket_submit_is_drained_on_owner_shutdown() {
    let mut case = AutoDispatchCase::start().await;
    let session = case.open().await;
    let submit = case.submit_request(&session, "auto-stop-turn").await;
    let submitted = case.owner.request(submit.clone()).await.unwrap();
    let stopped = case.stop().await;
    stopped.unwrap();
    let db = case.database();
    let (phase, body): (String, String) = db
        .query_row(
            "SELECT phase,body FROM master_turns WHERE session=?1 AND id=?2",
            rusqlite::params![session.session_id.as_str(), submit.request_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let stored: MasterTurnView = serde_json::from_str(&body).unwrap();
    assert_eq!(receipt(&submitted).result, OwnerResult::Applied);
    assert_eq!(phase, "blocked");
    assert_blocked(&stored, &submit, &session);
    assert_eq!(case.counts(), (1, 0, 0));
}
