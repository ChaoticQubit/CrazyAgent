use super::*;
use crate::master_session::{
    MasterBackend, MasterOwnerAction, MasterSelection, MasterSessionView,
    RcliConfiguredProfileProof,
};
use crate::model_gateway::AttemptRequest;
use crate::routing::{ResourceBudget, RouteRequest};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const CONFIG: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = ["configured-profile"]
[routing]
approved_accounts = ["configured-account"]
global_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
project_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
task_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
[model_access]
accounts = ["configured-account"]
[[model_access.connections]]
id = "configured-connection"
provider = "local-provider"
harness = "rcli"
protocol = "openai-chat"
account_ref = "configured-account"
endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }
model = { id = "local-model", author = "local-author" }
auth = { method = "none" }
[[model_access.pools]]
id = "configured-pool"
members = ["configured-connection"]
projects = ["configured-project"]
roles = ["project_leader"]
fallback_allowed = false
"#;

pub(crate) struct ConfiguredInitialConversationFixture {
    pub(crate) db: Connection,
    pub(crate) config: EffectiveConfig,
    pub(crate) request: AttemptRequest,
    pub(crate) proof: RcliConfiguredProfileProof,
    pub(crate) binding: NativeProfileBinding,
    pub(crate) generation: u64,
    pub(crate) host: HostStamp,
    pub(crate) session: MasterSessionView,
    pub(crate) global_path: PathBuf,
    pub(crate) profile_home: PathBuf,
    storage: Storage,
    root: tempfile::TempDir,
}

impl ConfiguredInitialConversationFixture {
    pub(crate) async fn close(self) {
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::OwnerStop(self.host, send))
            .unwrap_or_else(|_| {
                panic!("actual configured fixture owner stop queue unavailable")
            });
        receive.await.unwrap().unwrap();
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::Close(send))
            .unwrap_or_else(|_| {
                panic!("actual configured fixture actor close queue unavailable")
            });
        receive.await.unwrap().unwrap();
        self.db.close().map_err(|(_, error)| error).unwrap();
        assert!(self.root.path().is_dir());
    }
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

async fn read(storage: &Storage, started: &Started, query: OwnerRead) -> OwnerReadReply {
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerRead(
            started.owners[0].admission.clone(),
            query,
            send,
        ))
        .unwrap_or_else(|_| panic!("actual configured fixture owner read queue unavailable"));
    receive.await.unwrap().unwrap()
}

async fn apply(storage: &Storage, started: &Started, id: &str, mutation: OwnerMutation) {
    let basis = read(storage, started, OwnerRead::Capabilities).await;
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerApply(
            started.owners[0].admission.clone(),
            OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new(id).unwrap(),
                expected_revision: basis.revision,
                operation: OwnerOperation::Apply { mutation },
            },
            send,
        ))
        .unwrap_or_else(|_| {
            panic!("actual configured fixture owner mutation queue unavailable")
        });
    assert_eq!(receive.await.unwrap().unwrap().result, OwnerResult::Applied);
}

pub(crate) async fn configured_initial_conversation_fixture()
-> ConfiguredInitialConversationFixture {
    let root = tempfile::Builder::new()
        .prefix("ca-cgf-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global_path = root.path().join("global.toml");
    private_file(&global_path, CONFIG.as_bytes());
    let profile_home = root.path().join("configured-home");
    std::fs::create_dir(&profile_home).unwrap();
    std::fs::set_permissions(&profile_home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let sdk = profile_home.join(".backboard");
    std::fs::create_dir(&sdk).unwrap();
    std::fs::set_permissions(&sdk, std::fs::Permissions::from_mode(0o700)).unwrap();
    private_file(&sdk.join("config.json"), &serde_json::to_vec(&serde_json::json!({
        "model": { "provider": "local-provider", "model": "local-model" },
        "providers": [{"id": "local-provider", "name": "Local provider", "protocol": "openai-chat",
            "baseUrl": "http://127.0.0.1:11434/v1", "auth": { "type": "none" }}]
    })).unwrap());
    let database_path = root.path().join("engine.db");
    let (storage, config) = Storage::open_with_settings_sources(
        &database_path,
        &global_path,
        &root.path().join("project.toml"),
    )
    .await
    .unwrap();
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerStart(send))
        .unwrap_or_else(|_| panic!("actual configured fixture owner start queue unavailable"));
    let started = receive.await.unwrap().unwrap();
    let project = ProjectId::new("configured-project").unwrap();
    apply(
        &storage,
        &started,
        "create-configured-project",
        OwnerMutation::CreateProject {
            project_id: project.clone(),
            spec: ProjectSpec {
                name: "Configured conversation gateway".into(),
                repositories: vec![],
            },
        },
    )
    .await;
    let basis = read(
        &storage,
        &started,
        OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Pools,
            cursor: None,
            limit: 1,
        },
    )
    .await;
    let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
        panic!("actual configured fixture source basis missing");
    };
    apply(
        &storage,
        &started,
        "open-configured-master",
        OwnerMutation::ProjectMaster {
            action: MasterOwnerAction::Open {
                project_id: project.clone(),
                expected_config_id: basis.config_id,
                expected_source_hash: source_hash,
                selection: Box::new(MasterSelection {
                    backend: MasterBackend::Rcli,
                    pool_ref: reference("configured-pool"),
                    target_profile_ref: reference("configured-profile"),
                    route: RouteRequest {
                        preferred_target: Some(reference("configured-profile")),
                        required_target: Some(reference("configured-profile")),
                        required_capabilities: vec![],
                        account_ref: reference("configured-account"),
                        budget_ref: reference("conversation-budget"),
                        resources: ResourceBudget::unit(),
                    },
                }),
            },
        },
    )
    .await;
    let state = read(
        &storage,
        &started,
        OwnerRead::ProjectMaster {
            project_id: project.clone(),
        },
    )
    .await;
    let OwnerData::ProjectMaster {
        session: Some(session),
    } = state.data
    else {
        panic!("actual configured fixture committed session missing");
    };
    let session = *session;
    apply(
        &storage,
        &started,
        "submit-configured-conversation",
        OwnerMutation::ProjectMaster {
            action: MasterOwnerAction::Submit {
                project_id: project.clone(),
                session_id: session.session_id.clone(),
                text: "Describe this project without using tools.".into(),
            },
        },
    )
    .await;
    let state = read(
        &storage,
        &started,
        OwnerRead::ProjectMaster {
            project_id: project.clone(),
        },
    )
    .await;
    let OwnerData::ProjectMaster {
        session: Some(session),
    } = state.data
    else {
        panic!("actual configured fixture queued session missing");
    };
    let session = *session;
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::MasterRuntime(
            Box::new(MasterRuntimeCommand::RegisterRcliProfile {
                host: started.host.clone(),
                project_id: project.clone(),
                session_id: session.session_id.clone(),
                configured_home: profile_home.clone(),
            }),
            send,
        ))
        .unwrap_or_else(|_| panic!("actual configured fixture registration queue unavailable"));
    let MasterRuntimeResult::ConfiguredProfile(proof) = receive.await.unwrap().unwrap() else {
        panic!("actual configured fixture persisted registration missing");
    };
    let proof = *proof;
    assert_eq!(proof.database_instance, started.host.db_instance);
    assert_eq!(proof.engine_epoch, started.host.engine_epoch);
    assert_eq!(proof.config_id, config.id);
    assert_eq!(proof.source_hash, session.source_hash);
    assert_eq!(proof.profile_ref, session.selection.target_profile_ref);
    assert!(proof.generation > 0);
    let binding = NativeProfileBinding {
        database_instance: proof.database_instance.clone(),
        engine_epoch: proof.engine_epoch.clone(),
        config_id: proof.config_id.clone(),
        source_hash: proof.source_hash.clone(),
        at_revision: proof.at_revision.clone(),
        connection: proof.connection.clone(),
        custody_id: proof.custody_id.clone(),
    };
    let request = AttemptRequest {
        id: RequestId::new("configured-gateway-first-attempt").unwrap(),
        task_id: master_session::turn_task_id(
            &session.session_id,
            session.current_turn.as_ref().unwrap(),
        )
        .unwrap(),
        project_id: project,
        role: crate::coordination::Role::ProjectLeader,
        pool_ref: session.selection.pool_ref.clone(),
        target_profile_ref: session.selection.target_profile_ref.clone(),
        route: session.selection.route.clone(),
        previous_attempt: None,
    };
    let generation = proof.generation;
    let db = Connection::open_with_flags(
        &database_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    db.busy_timeout(std::time::Duration::from_secs(1)).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM model_gateway_attempts", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM native_auth_operations", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM model_gateway_observations",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert!(!profile_home.join(".backboard/keys.json").exists());
    ConfiguredInitialConversationFixture {
        db,
        config,
        request,
        proof,
        binding,
        generation,
        host: started.host,
        session,
        global_path,
        profile_home,
        storage,
        root,
    }
}
