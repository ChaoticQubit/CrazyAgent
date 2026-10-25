use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{
    ModelAccessCollection, OwnerClient, OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation,
    OwnerRead, OwnerReadReply, OwnerRequest, OwnerResponse, OwnerResult,
};
use crate::master_session::{
    MasterBackend, MasterOwnerAction, MasterSelection, MasterSessionPhase, MasterSessionView,
    SESSIONS_MAX,
};
use crate::protocol::{CredentialRef, Cursor, PROTOCOL_VERSION, ProjectId, ProjectSpec, RequestId};
use crate::routing::{ResourceBudget, RouteRequest};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tokio::sync::watch;

const CATALOG: &str = r#"version = 1
timeout_ms = 5000
allowed_destinations = ["cap-profile"]
[routing]
approved_accounts = ["cap-account"]
[model_access]
accounts = ["cap-account"]
[[model_access.connections]]
id = "cap-connection"
provider = "local-provider"
harness = "rcli"
protocol = "openai-chat"
account_ref = "cap-account"
endpoint = { kind="http", url="http://127.0.0.1:11434/v1" }
model = { id="local-model", author="local" }
auth = { method="none" }
[[model_access.pools]]
id = "cap-pool"
members = ["cap-connection"]
projects = ["cap-project"]
roles = ["project_leader"]
fallback_allowed = false
"#;

async fn read(client: &OwnerClient, query: OwnerRead) -> OwnerReadReply {
    let reply = client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("master-capacity-read").unwrap(),
            expected_revision: Cursor::new(0),
            operation: OwnerOperation::Read { query },
        })
        .await
        .unwrap();
    let OwnerResponse::Read { state } = reply.response else {
        panic!("actual typed owner read required: {reply:?}");
    };
    state
}

async fn apply(client: &OwnerClient, id: &str, mutation: OwnerMutation) -> OwnerResult {
    let basis = read(client, OwnerRead::Capabilities).await;
    let reply = client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(id).unwrap(),
            expected_revision: basis.revision,
            operation: OwnerOperation::Apply { mutation },
        })
        .await
        .unwrap();
    let OwnerResponse::Applied { operation } = reply.response else {
        panic!("actual durable owner mutation required: {reply:?}");
    };
    assert!(!operation.historical_replay);
    operation.result
}

async fn open(client: &OwnerClient, project: &ProjectId, id: &str) -> OwnerResult {
    let basis = read(
        client,
        OwnerRead::ModelAccessRecord {
            collection: ModelAccessCollection::Pools,
            id: CredentialRef::new("cap-pool").unwrap(),
        },
    )
    .await;
    let OwnerData::ModelAccessRecord { source_hash, .. } = basis.data else {
        panic!("actual current pool/source basis required");
    };
    let target = CredentialRef::new("cap-profile").unwrap();
    apply(
        client,
        id,
        OwnerMutation::ProjectMaster {
            action: MasterOwnerAction::Open {
                project_id: project.clone(),
                expected_config_id: basis.config_id,
                expected_source_hash: source_hash,
                selection: Box::new(MasterSelection {
                    backend: MasterBackend::Rcli,
                    pool_ref: CredentialRef::new("cap-pool").unwrap(),
                    target_profile_ref: target.clone(),
                    route: RouteRequest {
                        preferred_target: Some(target.clone()),
                        required_target: Some(target),
                        required_capabilities: vec![],
                        account_ref: CredentialRef::new("cap-account").unwrap(),
                        budget_ref: CredentialRef::new("cap-budget").unwrap(),
                        resources: ResourceBudget::unit(),
                    },
                }),
            },
        },
    )
    .await
}

async fn current(client: &OwnerClient, project: &ProjectId) -> Option<MasterSessionView> {
    let state = read(
        client,
        OwnerRead::ProjectMaster {
            project_id: project.clone(),
        },
    )
    .await;
    let OwnerData::ProjectMaster { session } = state.data else {
        panic!("actual project master read required");
    };
    session.map(|session| *session)
}

#[tokio::test]
async fn a_project_opens_its_master_again_after_more_closed_sessions_than_the_live_cap() {
    let root = tempfile::Builder::new()
        .prefix("ca-cap-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let repository = root.path().join("repository");
    std::fs::create_dir(&repository).unwrap();
    std::fs::write(repository.join("README.md"), "Master capacity repository\n").unwrap();
    let global = root.path().join("global.toml");
    std::fs::write(&global, CATALOG).unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_shutdown, stopped) = watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("project.toml"),
        &stopped,
    )
    .await
    .unwrap();
    let (client, _) =
        OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(5))
            .await
            .unwrap();
    let project = ProjectId::new("cap-project").unwrap();
    let created = apply(
        &client,
        "create-capacity-project",
        OwnerMutation::CreateProject {
            project_id: project.clone(),
            spec: ProjectSpec {
                name: "Master capacity project".into(),
                repositories: vec![repository.to_str().unwrap().into()],
            },
        },
    )
    .await;
    assert_eq!(created, OwnerResult::Applied);

    let cycles = SESSIONS_MAX + 1;
    for cycle in 0..cycles {
        let opened = open(&client, &project, &format!("open-capacity-{cycle}")).await;
        assert_eq!(opened, OwnerResult::Applied, "open {cycle}");
        let session = current(&client, &project).await.unwrap();
        assert_eq!(session.phase, MasterSessionPhase::Configured);
        let closed = apply(
            &client,
            &format!("close-capacity-{cycle}"),
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Close {
                    project_id: project.clone(),
                    session_id: session.session_id.clone(),
                },
            },
        )
        .await;
        assert_eq!(closed, OwnerResult::Applied, "close {cycle}");
    }

    let reopened = open(&client, &project, "open-capacity-after").await;
    assert_eq!(reopened, OwnerResult::Applied);
    let live = current(&client, &project).await.unwrap();
    assert_eq!(live.phase, MasterSessionPhase::Configured);
    let rejected = open(&client, &project, "open-capacity-twice").await;
    assert_eq!(
        rejected,
        OwnerResult::Rejected {
            code: "master_session_active".into()
        }
    );
    service.shutdown().await.unwrap();
}
