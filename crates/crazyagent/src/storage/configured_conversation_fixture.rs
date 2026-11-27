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
    /// Stops the fixture's owner, closes the storage actor and the row's connection.
    /// Input: the fixture. Output: none, or the step that failed.
    pub(crate) async fn close(self) -> Result<(), String> {
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::OwnerStop(self.host, send))
            .map_err(|_| "the configured fixture's owner stop queue is unavailable".to_owned())?;
        named(
            named(receive.await, "receive the owner stop")?,
            "stop the owner",
        )?;
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::Close(send))
            .map_err(|_| "the configured fixture's actor close queue is unavailable".to_owned())?;
        named(
            named(receive.await, "receive the actor close")?,
            "close the actor",
        )?;
        named(
            self.db.close().map_err(|(_, error)| error),
            "close the row's connection",
        )?;
        assert!(self.root.path().is_dir());
        Ok(())
    }
}

/// A step's result with the step named in its error. Inputs: the result and the step.
/// Output: the value, or the named error.
fn named<T, E: std::fmt::Display>(
    result: Result<T, E>,
    step: impl std::fmt::Display,
) -> Result<T, String> {
    result.map_err(|error| format!("{step}: {error}"))
}

/// A credential reference from a fixed fixture word. Input: the word. Output: the
/// reference, or why the word is refused.
fn reference(value: &str) -> Result<CredentialRef, String> {
    named(
        CredentialRef::new(value),
        format!("make the reference {value}"),
    )
}

/// Writes a private 0600 file. Inputs: the path and the bytes. Output: none, or the step
/// that failed.
fn private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    named(
        std::fs::write(path, bytes),
        format!("write {}", path.display()),
    )?;
    named(
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)),
        format!("make {} private", path.display()),
    )
}

/// Reads one owner view through the storage actor. Inputs: the storage, the started owner
/// and the query. Output: the reply, or the step that failed.
async fn read(
    storage: &Storage,
    started: &Started,
    query: OwnerRead,
) -> Result<OwnerReadReply, String> {
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerRead(
            started.owners[0].admission.clone(),
            query,
            send,
        ))
        .map_err(|_| "the configured fixture's owner read queue is unavailable".to_owned())?;
    named(
        named(receive.await, "receive the owner read")?,
        "read the owner view",
    )
}

/// Applies one owner mutation at the current revision and checks it is applied. Inputs:
/// the storage, the started owner, the request id and the mutation. Output: none, or the
/// step that failed.
async fn apply(
    storage: &Storage,
    started: &Started,
    id: &str,
    mutation: OwnerMutation,
) -> Result<(), String> {
    let basis = read(storage, started, OwnerRead::Capabilities).await?;
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerApply(
            started.owners[0].admission.clone(),
            OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: named(RequestId::new(id), format!("make the request id {id}"))?,
                expected_revision: basis.revision,
                operation: OwnerOperation::Apply { mutation },
            },
            send,
        ))
        .map_err(|_| "the configured fixture's owner mutation queue is unavailable".to_owned())?;
    let applied = named(
        named(receive.await, format!("receive the owner apply {id}"))?,
        format!("apply {id}"),
    )?;
    assert_eq!(applied.result, OwnerResult::Applied);
    Ok(())
}

/// Makes a private 0700 folder. Input: the path. Output: none, or the step that failed.
fn private_folder(path: &Path) -> Result<(), String> {
    named(
        std::fs::create_dir(path),
        format!("make {}", path.display()),
    )?;
    named(
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)),
        format!("make {} private", path.display()),
    )
}

/// A real storage actor with one configured project whose master session holds a queued
/// first turn and a registered rcli profile, ready for its first model attempt. Input: none.
/// Output: the fixture, or the step that failed.
pub(crate) async fn configured_initial_conversation_fixture()
-> Result<ConfiguredInitialConversationFixture, String> {
    let base = named(std::fs::canonicalize("/tmp"), "canonicalize /tmp")?;
    let root = named(
        tempfile::Builder::new().prefix("ca-cgf-").tempdir_in(&base),
        format!("make a folder in {}", base.display()),
    )?;
    named(
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)),
        "make the fixture root private",
    )?;
    let global_path = root.path().join("global.toml");
    private_file(&global_path, CONFIG.as_bytes())?;
    let profile_home = root.path().join("configured-home");
    private_folder(&profile_home)?;
    let sdk = profile_home.join(".backboard");
    private_folder(&sdk)?;
    let sdk_config = named(
        serde_json::to_vec(&serde_json::json!({
            "model": { "provider": "local-provider", "model": "local-model" },
            "providers": [{"id": "local-provider", "name": "Local provider", "protocol": "openai-chat",
                "baseUrl": "http://127.0.0.1:11434/v1", "auth": { "type": "none" }}]
        })),
        "encode the SDK config",
    )?;
    private_file(&sdk.join("config.json"), &sdk_config)?;
    let database_path = root.path().join("engine.db");
    let (storage, config) = named(
        Storage::open_with_settings_sources(
            &database_path,
            &global_path,
            &root.path().join("project.toml"),
        )
        .await,
        "open the storage",
    )?;
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerStart(send))
        .map_err(|_| "the configured fixture's owner start queue is unavailable".to_owned())?;
    let started = named(
        named(receive.await, "receive the owner start")?,
        "start the owner",
    )?;
    let project = named(ProjectId::new("configured-project"), "make the project id")?;
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
    .await?;
    let basis = read(
        &storage,
        &started,
        OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Pools,
            cursor: None,
            limit: 1,
        },
    )
    .await?;
    let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
        return Err("the configured fixture's source basis is missing".to_owned());
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
                    pool_ref: reference("configured-pool")?,
                    target_profile_ref: reference("configured-profile")?,
                    route: RouteRequest {
                        preferred_target: Some(reference("configured-profile")?),
                        required_target: Some(reference("configured-profile")?),
                        required_capabilities: vec![],
                        account_ref: reference("configured-account")?,
                        budget_ref: reference("conversation-budget")?,
                        resources: ResourceBudget::unit(),
                    },
                }),
            },
        },
    )
    .await?;
    let state = read(
        &storage,
        &started,
        OwnerRead::ProjectMaster {
            project_id: project.clone(),
        },
    )
    .await?;
    let OwnerData::ProjectMaster {
        session: Some(session),
    } = state.data
    else {
        return Err("the configured fixture's committed session is missing".to_owned());
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
    .await?;
    let state = read(
        &storage,
        &started,
        OwnerRead::ProjectMaster {
            project_id: project.clone(),
        },
    )
    .await?;
    let OwnerData::ProjectMaster {
        session: Some(session),
    } = state.data
    else {
        return Err("the configured fixture's queued session is missing".to_owned());
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
        .map_err(|_| "the configured fixture's registration queue is unavailable".to_owned())?;
    let registered = named(
        named(receive.await, "receive the profile registration")?,
        "register the rcli profile",
    )?;
    let MasterRuntimeResult::ConfiguredProfile(proof) = registered else {
        return Err("the configured fixture's persisted registration is missing".to_owned());
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
    let turn = session
        .current_turn
        .as_ref()
        .ok_or_else(|| "the configured fixture's session has no current turn".to_owned())?;
    let request = AttemptRequest {
        id: named(
            RequestId::new("configured-gateway-first-attempt"),
            "make the attempt id",
        )?,
        task_id: named(
            master_session::turn_task_id(&session.session_id, turn),
            "derive the turn task id",
        )?,
        project_id: project,
        role: crate::coordination::Role::ProjectLeader,
        pool_ref: session.selection.pool_ref.clone(),
        target_profile_ref: session.selection.target_profile_ref.clone(),
        route: session.selection.route.clone(),
        previous_attempt: None,
    };
    let generation = proof.generation;
    let db = named(
        Connection::open_with_flags(
            &database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        ),
        format!("open {}", database_path.display()),
    )?;
    named(
        db.busy_timeout(std::time::Duration::from_secs(1)),
        "set the busy timeout",
    )?;
    for table in [
        "model_gateway_attempts",
        "native_auth_operations",
        "model_gateway_observations",
    ] {
        let rows = named(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            }),
            format!("count {table}"),
        )?;
        assert_eq!(rows, 0, "{table}");
    }
    assert!(!profile_home.join(".backboard/keys.json").exists());
    Ok(ConfiguredInitialConversationFixture {
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
    })
}
