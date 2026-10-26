use super::prompt;
use crate::conversation::{ConversationContent, ConversationMutation};
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{
    ModelAccessCollection, OwnerClient, OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation,
    OwnerRead, OwnerReadReply, OwnerRequest, OwnerResponse, OwnerResult, OwnerSummary, OwnerView,
};
use crate::master_session::{
    MasterBackend, MasterOwnerAction, MasterReason, MasterSelection, MasterSessionPhase,
    MasterSessionView, MasterTurnPhase, MasterTurnView,
};
use crate::protocol::{CredentialRef, Cursor, PROTOCOL_VERSION, ProjectId, ProjectSpec, RequestId};
use crate::routing::{ResourceBudget, RouteRequest};
use rusqlite::{Connection, OpenFlags};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use tokio::sync::watch;

const CATALOG: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = ["brief-profile"]
[routing]
approved_accounts = ["brief-account"]
[model_access]
accounts = ["brief-account"]
[[model_access.connections]]
id = "brief-connection"
provider = "local-provider"
harness = "rcli"
protocol = "openai-chat"
account_ref = "brief-account"
endpoint = { kind="http", url="http://127.0.0.1:11434/v1" }
model = { id="local-model", author="local" }
auth = { method="none" }
[[model_access.pools]]
id = "brief-pool"
members = ["brief-connection"]
projects = ["brief-project"]
roles = ["project_leader"]
fallback_allowed = false
"#;

const FIRST_BRIEF: &str = "Initial inventory brief: retain the obsolete blue ledger.";
const CURRENT_BRIEF: &str = "Current inventory brief: use the green ledger 🦀,\nrequire explicit approval before execution.";
const USER_MESSAGE: &str = "Explain the next planning step without executing work.";

async fn read(client: &OwnerClient, query: OwnerRead) -> OwnerReadReply {
    let reply = client
        .request(OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new("master-brief-read").unwrap(),
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

async fn apply(client: &OwnerClient, id: &str, mutation: OwnerMutation) {
    let basis = read(client, OwnerRead::Capabilities).await;
    let request = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: basis.revision,
        operation: OwnerOperation::Apply { mutation },
    };
    let reply = client.request(request.clone()).await.unwrap();
    let OwnerResponse::Applied { operation } = reply.response else {
        panic!("actual durable owner mutation required: {reply:?}");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    assert_eq!(operation.provenance.operation_id, request.request_id);
    assert_eq!(
        operation.provenance.request_hash,
        request.mutation_hash().unwrap()
    );
    assert!(!operation.historical_replay);
}

async fn session(client: &OwnerClient, project: &ProjectId) -> MasterSessionView {
    let state = read(
        client,
        OwnerRead::ProjectMaster {
            project_id: project.clone(),
        },
    )
    .await;
    let OwnerData::ProjectMaster {
        session: Some(session),
    } = state.data
    else {
        panic!("actual persisted master session required");
    };
    let session = *session;
    assert_eq!(session.project_id, *project);
    session
}

async fn open(client: &OwnerClient, project: &ProjectId, id: &str) -> MasterSessionView {
    let basis = read(
        client,
        OwnerRead::ModelAccessRecord {
            collection: ModelAccessCollection::Pools,
            id: CredentialRef::new("brief-pool").unwrap(),
        },
    )
    .await;
    let OwnerData::ModelAccessRecord { source_hash, .. } = basis.data else {
        panic!("actual current pool/source basis required");
    };
    let target = CredentialRef::new("brief-profile").unwrap();
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
                    pool_ref: CredentialRef::new("brief-pool").unwrap(),
                    target_profile_ref: target.clone(),
                    route: RouteRequest {
                        preferred_target: Some(target.clone()),
                        required_target: Some(target),
                        required_capabilities: vec![],
                        account_ref: CredentialRef::new("brief-account").unwrap(),
                        budget_ref: CredentialRef::new("brief-budget").unwrap(),
                        resources: ResourceBudget::unit(),
                    },
                }),
            },
        },
    )
    .await;
    let session = session(client, project).await;
    assert_eq!(session.phase, MasterSessionPhase::Configured);
    session
}

async fn submit(
    client: &OwnerClient,
    opened: &MasterSessionView,
    id: &str,
    reason: MasterReason,
) -> (MasterSessionView, MasterTurnView) {
    apply(
        client,
        id,
        OwnerMutation::ProjectMaster {
            action: MasterOwnerAction::Submit {
                project_id: opened.project_id.clone(),
                session_id: opened.session_id.clone(),
                text: USER_MESSAGE.into(),
            },
        },
    )
    .await;
    for _ in 0..200 {
        let state = read(
            client,
            OwnerRead::ProjectMasterTurns {
                project_id: opened.project_id.clone(),
                session_id: opened.session_id.clone(),
                cursor: None,
                limit: 8,
            },
        )
        .await;
        let OwnerData::ProjectMasterTurns { mut page } = state.data else {
            panic!("actual persisted submitted turn required");
        };
        assert_eq!(page.project_id, opened.project_id);
        assert_eq!(page.session_id, opened.session_id);
        assert!(page.next.is_none());
        assert_eq!(page.turns.len(), 1);
        let turn = page.turns.remove(0);
        assert_eq!(turn.id, RequestId::new(id).unwrap());
        assert_eq!(turn.session_id, opened.session_id);
        assert!(turn.attempt.is_none() && turn.runtime.is_none() && turn.reply.is_none());
        if turn.phase == MasterTurnPhase::Queued {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }
        assert_eq!(turn.phase, MasterTurnPhase::Blocked);
        assert_eq!(turn.reason, Some(reason));
        let active = session(client, &opened.project_id).await;
        assert_eq!(active.session_id, opened.session_id);
        assert_eq!(active.current_turn, None);
        return (active, turn);
    }
    panic!("actual master dispatch left turn {id} queued");
}

fn database(root: &Path) -> Connection {
    let db = Connection::open_with_flags(
        root.join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    db.busy_timeout(Duration::from_secs(1)).unwrap();
    db
}

fn assembled(root: &Path, session: &MasterSessionView, turn: &MasterTurnView) -> String {
    let mut db = database(root);
    let tx = db.transaction().unwrap();
    prompt(&tx, session, turn).unwrap()
}

#[tokio::test]
async fn current_project_brief_reaches_master_prompt_after_restart() {
    let root = tempfile::Builder::new()
        .prefix("ca-pb17-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let repository = root.path().join("repository");
    std::fs::create_dir(&repository).unwrap();
    std::fs::write(
        repository.join("README.md"),
        "Declared master brief repository\n",
    )
    .unwrap();
    let global = root.path().join("global.toml");
    let project_settings = root.path().join("project.toml");
    std::fs::write(&global, CATALOG).unwrap();
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_shutdown, stopped) = watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &project_settings,
        &stopped,
    )
    .await
    .unwrap();
    let (client, identity) =
        OwnerClient::discover(root.path(), OwnerClientKind::Tui, Duration::from_secs(3))
            .await
            .unwrap();
    apply(
        &client,
        "create-master-brief-project",
        OwnerMutation::CreateProject {
            project_id: ProjectId::new("brief-project").unwrap(),
            spec: ProjectSpec {
                name: "Master brief project".into(),
                repositories: vec![repository.to_str().unwrap().into()],
            },
        },
    )
    .await;
    let projects = read(
        &client,
        OwnerRead::Page {
            view: OwnerView::Projects,
            cursor: None,
            limit: 16,
        },
    )
    .await;
    let OwnerData::Page {
        mut items,
        next: None,
    } = projects.data
    else {
        panic!("actual committed project page required");
    };
    assert_eq!(items.len(), 1);
    let OwnerSummary::Project {
        project_id: project,
        name,
        repository_count,
    } = items.remove(0)
    else {
        panic!("actual committed project identity required");
    };
    assert_eq!(name, "Master brief project");
    assert_eq!(repository_count, 1);
    apply(
        &client,
        "master-brief-v1",
        OwnerMutation::Conversation {
            mutation: ConversationMutation::ReplaceBrief {
                project_id: project.clone(),
                expected_version: 0,
                text: FIRST_BRIEF.into(),
            },
        },
    )
    .await;
    let opened = open(&client, &project, "open-master-brief").await;
    let (active, first_turn) = submit(
        &client,
        &opened,
        "submit-master-brief",
        MasterReason::Unknown,
    )
    .await;
    let initial = assembled(root.path(), &active, &first_turn);
    apply(
        &client,
        "master-brief-v2",
        OwnerMutation::Conversation {
            mutation: ConversationMutation::ReplaceBrief {
                project_id: project.clone(),
                expected_version: 1,
                text: CURRENT_BRIEF.into(),
            },
        },
    )
    .await;
    let edited = assembled(root.path(), &active, &first_turn);
    let before_restart = read(
        &client,
        OwnerRead::CurrentBrief {
            project_id: project.clone(),
        },
    )
    .await;
    let OwnerData::CurrentBrief {
        brief: Some(expected),
        ..
    } = before_restart.data
    else {
        panic!("actual current brief required");
    };
    assert_eq!(expected.content, ConversationContent::Brief { version: 2 });
    assert_eq!(expected.text, CURRENT_BRIEF);
    apply(
        &client,
        "close-master-brief",
        OwnerMutation::ProjectMaster {
            action: MasterOwnerAction::Close {
                project_id: project.clone(),
                session_id: opened.session_id.clone(),
            },
        },
    )
    .await;
    assert_eq!(
        session(&client, &project).await.phase,
        MasterSessionPhase::Closed
    );
    drop(client);
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();

    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &project_settings,
        &stopped,
    )
    .await
    .unwrap();
    let (client, current_identity) = OwnerClient::discover(
        root.path(),
        OwnerClientKind::Desktop,
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(
        current_identity.database_instance(),
        identity.database_instance()
    );
    assert_ne!(current_identity.engine_epoch(), identity.engine_epoch());
    let closed = session(&client, &project).await;
    assert_eq!(closed.session_id, opened.session_id);
    assert_eq!(closed.phase, MasterSessionPhase::Closed);
    let reopened = open(&client, &project, "reopen-master-brief").await;
    assert_ne!(reopened.session_id, opened.session_id);
    assert_eq!(reopened.project_id, opened.project_id);
    assert_eq!(reopened.selection, opened.selection);
    assert_eq!(reopened.owner.engine_epoch, current_identity.engine_epoch());
    let after_restart = read(
        &client,
        OwnerRead::CurrentBrief {
            project_id: reopened.project_id.clone(),
        },
    )
    .await;
    let OwnerData::CurrentBrief {
        brief: Some(actual),
        ..
    } = after_restart.data
    else {
        panic!("actual reopened current brief required");
    };
    assert_eq!(actual, expected);
    let (reopened_active, reopened_turn) = submit(
        &client,
        &reopened,
        "resubmit-master-brief",
        MasterReason::Unknown,
    )
    .await;
    drop(client);
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    let restored = assembled(root.path(), &reopened_active, &reopened_turn);
    let mut db = database(root.path());
    let tx = db.transaction().unwrap();
    let OwnerData::CurrentBrief {
        brief: Some(selected),
        ..
    } = crate::conversation::current_brief(&tx, &project).unwrap()
    else {
        panic!("existing transactional current brief selector required");
    };
    assert_eq!(selected, expected);
    for table in [
        "model_gateway_attempts",
        "native_auth_operations",
        "configured_master_profiles",
        "master_preparations",
        "worktree_leases",
    ] {
        let count: i64 = tx
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "no fabricated admission or physical runtime fixture: {table}"
        );
    }
    assert!(
        edited.contains(CURRENT_BRIEF),
        "current persisted project brief is missing from the shared master prompt: {edited:?}"
    );
    assert_eq!(edited.matches(CURRENT_BRIEF).count(), 1);
    assert!(edited.contains(USER_MESSAGE));
    assert!(!edited.contains(FIRST_BRIEF));
    assert!(initial.contains(FIRST_BRIEF));
    assert!(!initial.contains(CURRENT_BRIEF));
    assert!(initial.contains(USER_MESSAGE));
    assert_eq!(restored, edited);
}
