use crazyagent::config::{
    AccountPool, ModelAccessEdit, ModelAuthentication, ModelConnection, ModelEndpoint,
    ModelReference, ProviderQuotaPolicy,
};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::*;
use crazyagent::master_session::{
    MasterBackend, MasterOwnerAction, MasterSelection, MasterSessionPhase, MasterSessionView,
};
use crazyagent::protocol::{
    CredentialRef, Cursor, PROTOCOL_VERSION, ProjectId, ProjectSpec, RequestId,
};
use crazyagent::routing::{ResourceBudget, RouteRequest};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::watch;

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn connection() -> ModelConnection {
    ModelConnection {
        id: reference("local-connection"),
        provider: reference("local-provider"),
        harness: reference("rcli"),
        protocol: reference("openai-chat"),
        account_ref: reference("local-account"),
        endpoint: ModelEndpoint::Http {
            url: "http://127.0.0.1:11434/v1".into(),
        },
        model: ModelReference {
            id: "local-model".into(),
            author: reference("local-author"),
        },
        auth: ModelAuthentication::None {},
    }
}

fn pool() -> AccountPool {
    AccountPool {
        id: reference("local-pool"),
        name: None,
        members: vec![connection().id],
        projects: vec![],
        roles: vec![],
        fallback_allowed: false,
        provider_quota_policy: ProviderQuotaPolicy::BoundedAttempt,
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn applied(response: OwnerResponse) -> OwnerApplied {
    let OwnerResponse::Applied { operation } = response else {
        panic!("actual owner application required: {response:?}");
    };
    operation
}

#[derive(Debug, PartialEq, Eq)]
struct DiskPin {
    path: PathBuf,
    device: u64,
    inode: u64,
    mode: u32,
    bytes: u64,
    sha256: Option<String>,
}

fn disk_pins(root: &Path, path: &Path, depth: usize, pins: &mut Vec<DiskPin>) {
    assert!(depth <= 5);
    assert!(pins.len() < 128);
    let metadata = std::fs::symlink_metadata(path).unwrap();
    assert!(!metadata.file_type().is_symlink());
    assert!(metadata.is_dir() || metadata.is_file());
    let sha256 = if metadata.is_file() {
        assert!(metadata.len() <= 65536);
        assert_ne!(path.file_name().unwrap(), "keys.json");
        Some(digest(&std::fs::read(path).unwrap()))
    } else {
        None
    };
    pins.push(DiskPin {
        path: path.strip_prefix(root).unwrap().to_path_buf(),
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        bytes: if metadata.is_file() {
            metadata.len()
        } else {
            0
        },
        sha256,
    });
    if metadata.is_dir() {
        let mut entries = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert!(entries.len() <= 65);
        entries.sort();
        for entry in entries {
            disk_pins(root, &entry, depth + 1, pins);
        }
    }
}

type AccountRecord = (Vec<u8>, Vec<u8>, String, String, i64, String);

struct Fixture {
    root: tempfile::TempDir,
    service: EngineService,
    shutdown: watch::Sender<bool>,
    global: PathBuf,
    project_settings: PathBuf,
}

impl Fixture {
    async fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-amj-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let profile = format!(
            "configured-{}",
            digest(&serde_json::to_vec(&connection()).unwrap())
        );
        let global = root.path().join("global.toml");
        let project_settings = root.path().join("project.toml");
        private_file(
            &global,
            format!(
                "version = 1\ntimeout_ms = 2000\nallowed_destinations = [\"{profile}\"]\n[routing]\napproved_accounts = [\"local-account\", \"other-account\"]\n"
            )
            .as_bytes(),
        );
        let (shutdown, stopped) = watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &project_settings,
            &stopped,
        )
        .await
        .unwrap();
        let fixture = Self {
            root,
            service,
            shutdown,
            global,
            project_settings,
        };
        fixture.assert_no_projects().await;
        for (id, edit) in [
            (
                "add-account",
                ModelAccessEdit::AddAccount {
                    account_ref: connection().account_ref,
                },
            ),
            (
                "add-connection",
                ModelAccessEdit::CreateConnection {
                    connection: connection(),
                },
            ),
            (
                "add-catalog-pool",
                ModelAccessEdit::CreatePool { pool: pool() },
            ),
        ] {
            fixture.edit(id, edit).await;
        }
        fixture.assert_no_projects().await;
        let catalog = fixture
            .read(OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Pools,
                id: pool().id,
            })
            .await;
        assert!(matches!(catalog.data, OwnerData::ModelAccessRecord {
            record: ModelAccessRecord::Pool { pool: actual }, ..
        } if actual == pool()));
        fixture
    }

    async fn client(&self) -> OwnerClient {
        OwnerClient::discover(
            self.root.path(),
            OwnerClientKind::Tui,
            Duration::from_secs(3),
        )
        .await
        .unwrap()
        .0
    }

    async fn read(&self, query: OwnerRead) -> OwnerReadReply {
        let reply = self
            .client()
            .await
            .request(OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new("join-read").unwrap(),
                expected_revision: Cursor::new(0),
                operation: OwnerOperation::Read { query },
            })
            .await
            .unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("actual owner read required: {reply:?}");
        };
        state
    }

    async fn mutate(&self, id: &str, mutation: OwnerMutation) -> (OwnerRequest, OwnerApplied) {
        let state = self.read(OwnerRead::Capabilities).await;
        let request = OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(id).unwrap(),
            expected_revision: state.revision,
            operation: OwnerOperation::Apply { mutation },
        };
        let operation = applied(
            self.client()
                .await
                .request(request.clone())
                .await
                .unwrap()
                .response,
        );
        assert_eq!(operation.provenance.operation_id, request.request_id);
        assert_eq!(
            operation.provenance.request_hash,
            request.mutation_hash().unwrap()
        );
        assert!(!operation.historical_replay);
        (request, operation)
    }

    async fn edit(&self, id: &str, edit: ModelAccessEdit) {
        let basis = self
            .read(OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Accounts,
                cursor: None,
                limit: 16,
            })
            .await;
        let OwnerData::ModelAccessPage {
            source_hash,
            writable,
            ..
        } = basis.data
        else {
            panic!("actual writable catalog required");
        };
        assert!(writable);
        let (_, operation) = self
            .mutate(
                id,
                OwnerMutation::ModelAccess {
                    expected_config_id: basis.config_id,
                    expected_source_hash: source_hash,
                    edit,
                },
            )
            .await;
        assert_eq!(operation.result, OwnerResult::Applied);
    }

    async fn assert_no_projects(&self) {
        let state = self
            .read(OwnerRead::Page {
                view: OwnerView::Projects,
                cursor: None,
                limit: 16,
            })
            .await;
        assert!(matches!(state.data, OwnerData::Page { items, next: None } if items.is_empty()));
    }

    async fn configure(
        &self,
    ) -> (
        AccountProfileRequest,
        AccountProfileReply,
        AccountProfileReceipt,
    ) {
        self.assert_no_projects().await;
        let basis = self
            .read(OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Connections,
                id: connection().id,
            })
            .await;
        let OwnerData::ModelAccessRecord {
            source_hash,
            record: ModelAccessRecord::Connection { connection: actual },
            ..
        } = basis.data
        else {
            panic!("actual connection required before profile configuration");
        };
        assert_eq!(actual, connection());
        let (client, identity) = OwnerClient::discover(
            self.root.path(),
            OwnerClientKind::Tui,
            Duration::from_secs(3),
        )
        .await
        .unwrap();
        let request = AccountProfileRequest {
            request_id: RequestId::new("configure-before-project").unwrap(),
            scope: NativeAuthScope {
                expected_database_instance: identity.database_instance().to_owned(),
                connection: actual.id.clone(),
                expected_config_id: basis.config_id.clone(),
                expected_source_hash: source_hash.clone(),
            },
        };
        let reply = client
            .configure_account_profile(request.clone(), None)
            .await
            .unwrap();
        let AccountProfileOutcome::ConfiguredOnly {
            receipt,
            historical_replay,
        } = &reply.outcome
        else {
            panic!("genuine configured-only owner receipt required");
        };
        assert!(!historical_replay);
        assert_eq!(receipt.connection, actual);
        assert_eq!(receipt.config_id, basis.config_id);
        assert_eq!(receipt.source_hash, source_hash);
        assert_eq!(reply.engine_epoch, identity.engine_epoch());
        assert_eq!(reply.database_instance, identity.database_instance());
        assert!(!receipt.custody_id.is_empty());
        assert_eq!(receipt.profile_config_sha256.len(), 64);
        assert!(receipt.profile_generation > 0);
        let receipt = receipt.clone();
        let replay = client.read_account_profile(request.clone()).await.unwrap();
        assert!(
            matches!(replay.outcome, AccountProfileOutcome::ConfiguredOnly {
            receipt: actual, historical_replay: true,
        } if actual == receipt)
        );
        self.assert_no_projects().await;
        self.assert_no_execution();
        (request, reply, receipt)
    }

    async fn create_and_bind(&self) -> ProjectId {
        let repository = self.root.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        let requested = ProjectId::new("actual-created-project").unwrap();
        let (_, operation) = self
            .mutate(
                "create-real-project",
                OwnerMutation::CreateProject {
                    project_id: requested.clone(),
                    spec: ProjectSpec {
                        name: "Account before project".into(),
                        repositories: vec![repository.to_str().unwrap().to_owned()],
                    },
                },
            )
            .await;
        assert_eq!(operation.result, OwnerResult::Applied);
        let page = self
            .read(OwnerRead::Page {
                view: OwnerView::Projects,
                cursor: None,
                limit: 16,
            })
            .await;
        let OwnerData::Page {
            mut items,
            next: None,
        } = page.data
        else {
            panic!("committed project page required");
        };
        assert_eq!(items.len(), 1);
        let OwnerSummary::Project {
            project_id,
            name,
            repository_count,
        } = items.remove(0)
        else {
            panic!("actual committed project identity required");
        };
        assert_eq!(project_id, requested);
        assert_eq!(name, "Account before project");
        assert_eq!(repository_count, 1);
        let mut selected = pool();
        selected.projects = vec![reference(project_id.as_str())];
        selected.roles = vec![reference("project_leader")];
        self.edit(
            "bind-returned-project",
            ModelAccessEdit::ReplacePool {
                pool: selected.clone(),
            },
        )
        .await;
        let readback = self
            .read(OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Pools,
                id: selected.id.clone(),
            })
            .await;
        assert!(matches!(readback.data, OwnerData::ModelAccessRecord {
            record: ModelAccessRecord::Pool { pool: actual }, ..
        } if actual == selected));
        let durable =
            crazyagent::config::resolve_config(&self.global, &self.project_settings).unwrap();
        assert_eq!(durable.settings.model_access.unwrap().pools, vec![selected]);
        project_id
    }

    async fn open(
        &self,
        project_id: &ProjectId,
        receipt: &AccountProfileReceipt,
    ) -> (OwnerRequest, OwnerApplied) {
        let basis = self
            .read(OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Connections,
                id: connection().id,
            })
            .await;
        let OwnerData::ModelAccessRecord {
            source_hash,
            record:
                ModelAccessRecord::Connection {
                    connection: current,
                },
            ..
        } = basis.data
        else {
            panic!("current connection required for master open");
        };
        self.mutate(
            "open-master",
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Open {
                    project_id: project_id.clone(),
                    expected_config_id: basis.config_id,
                    expected_source_hash: source_hash,
                    selection: MasterSelection {
                        backend: MasterBackend::Rcli,
                        pool_ref: pool().id,
                        target_profile_ref: receipt.profile_ref.clone(),
                        route: RouteRequest {
                            preferred_target: Some(receipt.profile_ref.clone()),
                            required_target: Some(receipt.profile_ref.clone()),
                            required_capabilities: vec![],
                            account_ref: current.account_ref,
                            budget_ref: reference("conversation-budget"),
                            resources: ResourceBudget::unit(),
                        },
                    },
                },
            },
        )
        .await
    }

    async fn session(&self, project: &ProjectId) -> MasterSessionView {
        let state = self
            .read(OwnerRead::ProjectMaster {
                project_id: project.clone(),
            })
            .await;
        let OwnerData::ProjectMaster {
            session: Some(session),
        } = state.data
        else {
            panic!("actual committed master session required");
        };
        assert_eq!(session.config_id, state.config_id);
        session
    }

    fn db(&self) -> Connection {
        let db = Connection::open_with_flags(
            self.root.path().join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(1)).unwrap();
        db
    }

    fn account_record(&self) -> AccountRecord {
        self.db().query_row(
            "SELECT request,receipt,phase,owner_id,owner_epoch,engine_epoch FROM account_profile_operations",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        ).unwrap()
    }

    fn count(&self, table: &str) -> i64 {
        self.db()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn registration(&self, session: &MasterSessionView) -> Option<Vec<u8>> {
        self.db()
            .query_row(
                "SELECT CAST(body AS BLOB) FROM configured_master_profiles WHERE session=?1",
                [session.session_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
    }

    fn physical_pins(&self) -> Vec<DiskPin> {
        let mut pins = Vec::new();
        for name in ["configured-sdk-homes", "native-profiles"] {
            disk_pins(self.root.path(), &self.root.path().join(name), 0, &mut pins);
        }
        pins
    }

    fn assert_no_execution(&self) {
        for table in [
            "master_turns",
            "master_preparations",
            "model_gateway_attempts",
            "model_gateway_observations",
            "native_auth_operations",
            "worktree_leases",
            "bootstrap_workspaces",
        ] {
            assert_eq!(self.count(table), 0, "custody join created {table}");
        }
    }

    async fn restart_with_source_only_change(&mut self) {
        let before =
            crazyagent::config::resolve_config(&self.global, &self.project_settings).unwrap();
        self.stop().await;
        let mut bytes = std::fs::read(&self.global).unwrap();
        bytes.push(b'\n');
        private_file(&self.global, &bytes);
        let after =
            crazyagent::config::resolve_config(&self.global, &self.project_settings).unwrap();
        assert_eq!(after.id, before.id);
        let stopped = self.shutdown.subscribe();
        self.service = EngineService::start_with_settings_sources(
            self.root.path(),
            "e.sock",
            &self.global,
            &self.project_settings,
            &stopped,
        )
        .await
        .unwrap();
    }

    async fn stop(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), self.service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

fn assert_join(
    bytes: Option<Vec<u8>>,
    session: &MasterSessionView,
    receipt: &AccountProfileReceipt,
    original: &AccountRecord,
) {
    let bytes =
        bytes.expect("Master Open must freshly join the actual pre-project configured custody");
    let row: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(row["session_id"], json!(session.session_id));
    assert_eq!(row["project_id"], json!(session.project_id));
    assert_eq!(row["bootstrap_task_id"], json!(session.bootstrap_task_id));
    assert_eq!(row["owner"], json!(session.owner));
    let proof = &row["proof"];
    assert_eq!(
        proof["database_instance"],
        json!(
            serde_json::from_slice::<AccountProfileRequest>(&original.0)
                .unwrap()
                .scope
                .expected_database_instance
        )
    );
    assert_eq!(proof["engine_epoch"], json!(session.owner.engine_epoch));
    assert_eq!(proof["config_id"], json!(session.config_id));
    assert_eq!(proof["source_hash"], json!(session.source_hash));
    assert_eq!(proof["at_revision"], json!(session.owner.revision_after));
    assert_eq!(proof["connection"], json!(receipt.connection));
    assert_eq!(proof["profile_ref"], json!(receipt.profile_ref));
    assert_eq!(proof["custody_id"], json!(receipt.custody_id));
    assert_eq!(
        proof["profile_config_sha256"],
        json!(receipt.profile_config_sha256)
    );
    assert_eq!(proof["generation"], json!(receipt.profile_generation));
    assert!(!proof["registration_id"].as_str().unwrap().is_empty());
    assert_ne!(session.config_id, receipt.config_id);
    assert_ne!(session.source_hash, receipt.source_hash);
    assert_eq!(session.phase, MasterSessionPhase::Configured);
    assert_eq!(session.current_turn, None);
    assert_eq!(session.owner.owner_id, original.3);
    assert_eq!(session.owner.resulting_epoch, original.4);
}

#[tokio::test]
async fn account_before_project_joins_same_custody_after_explicit_pool_binding_and_replays_once() {
    let mut fixture = Fixture::start().await;
    let (_, _, receipt) = fixture.configure().await;
    let original = fixture.account_record();
    let physical = fixture.physical_pins();
    let project = fixture.create_and_bind().await;
    let (request, operation) = fixture.open(&project, &receipt).await;
    assert_eq!(operation.result, OwnerResult::Applied);
    let session = fixture.session(&project).await;
    assert_eq!(session.owner, operation.provenance);
    let joined = fixture.registration(&session);
    let replay = applied(
        fixture
            .client()
            .await
            .request(request.clone())
            .await
            .unwrap()
            .response,
    );
    assert!(replay.historical_replay);
    assert_eq!(replay.provenance, operation.provenance);
    assert_eq!(replay.result, operation.result);
    assert_eq!(fixture.registration(&session), joined);
    assert_eq!(fixture.count("master_sessions"), 1);
    assert_eq!(fixture.count("account_profile_operations"), 1);
    let (stored_hash, stored_result): (Vec<u8>, Vec<u8>) = fixture.db().query_row(
        "SELECT request_hash,result FROM owner_operations WHERE origin='native_owner' AND owner_id=?1 AND operation_id=?2",
        rusqlite::params![session.owner.owner_id, request.request_id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(
        stored_hash,
        Sha256::digest(serde_json::to_vec(&request).unwrap()).to_vec()
    );
    assert_eq!(
        serde_json::from_slice::<OwnerResult>(&stored_result).unwrap(),
        operation.result
    );
    assert_eq!(fixture.account_record(), original);
    assert_eq!(fixture.physical_pins(), physical);
    fixture.assert_no_execution();
    fixture.stop().await;
    assert_join(joined, &session, &receipt, &original);
    assert_eq!(fixture.count("configured_master_profiles"), 1);
}

#[tokio::test]
async fn account_custody_is_freshly_adopted_after_real_restart_and_source_only_change() {
    let mut fixture = Fixture::start().await;
    let (_, configured, receipt) = fixture.configure().await;
    let original = fixture.account_record();
    let physical = fixture.physical_pins();
    fixture.restart_with_source_only_change().await;
    let (_, current) = OwnerClient::discover(
        fixture.root.path(),
        OwnerClientKind::Tui,
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(current.database_instance(), configured.database_instance);
    assert_ne!(current.engine_epoch(), configured.engine_epoch);
    let current_basis = fixture
        .read(OwnerRead::ModelAccessRecord {
            collection: ModelAccessCollection::Connections,
            id: receipt.connection.id.clone(),
        })
        .await;
    let OwnerData::ModelAccessRecord { source_hash, .. } = current_basis.data else {
        panic!("actual source-only reload basis required");
    };
    assert_eq!(current_basis.config_id, receipt.config_id);
    assert_ne!(source_hash, receipt.source_hash);
    let project = fixture.create_and_bind().await;
    let (_, operation) = fixture.open(&project, &receipt).await;
    assert_eq!(operation.result, OwnerResult::Applied);
    let session = fixture.session(&project).await;
    assert_eq!(session.owner.engine_epoch, current.engine_epoch());
    let joined = fixture.registration(&session);
    assert_eq!(fixture.account_record(), original);
    assert_eq!(fixture.physical_pins(), physical);
    fixture.assert_no_execution();
    fixture.stop().await;
    assert_join(joined, &session, &receipt, &original);
}

#[tokio::test]
async fn changed_connection_identity_never_automatically_reuses_configured_account_custody() {
    for field in [
        "endpoint",
        "model",
        "provider",
        "harness",
        "account",
        "secret_reference",
        "protocol",
    ] {
        let mut fixture = Fixture::start().await;
        let (_, _, receipt) = fixture.configure().await;
        let original = fixture.account_record();
        let physical = fixture.physical_pins();
        let mut changed = connection();
        match field {
            "endpoint" => {
                changed.endpoint = ModelEndpoint::Http {
                    url: "http://127.0.0.1:9999/v1".into(),
                }
            }
            "model" => changed.model.id = "different-model".into(),
            "provider" => changed.provider = reference("other-provider"),
            "harness" => changed.harness = reference("other-harness"),
            "account" => {
                fixture
                    .edit(
                        "add-other-account",
                        ModelAccessEdit::AddAccount {
                            account_ref: reference("other-account"),
                        },
                    )
                    .await;
                changed.account_ref = reference("other-account");
            }
            "secret_reference" => {
                changed.auth = ModelAuthentication::ApiKey {
                    credential_ref: reference("different-key-reference"),
                }
            }
            "protocol" => changed.protocol = reference("anthropic-messages"),
            _ => unreachable!(),
        }
        fixture
            .edit(
                "replace-connection-identity",
                ModelAccessEdit::ReplaceConnection {
                    connection: changed,
                },
            )
            .await;
        let project = fixture.create_and_bind().await;
        let (_, operation) = fixture.open(&project, &receipt).await;
        assert!(matches!(
            operation.result,
            OwnerResult::Applied | OwnerResult::Rejected { .. }
        ));
        assert_eq!(
            fixture.count("configured_master_profiles"),
            0,
            "reused changed {field}"
        );
        assert_eq!(fixture.account_record(), original);
        assert_eq!(fixture.physical_pins(), physical);
        fixture.assert_no_execution();
        fixture.stop().await;
    }
}

#[tokio::test]
async fn physically_replaced_generated_config_cannot_be_adopted_by_master_open() {
    let mut fixture = Fixture::start().await;
    let (_, _, receipt) = fixture.configure().await;
    let original = fixture.account_record();
    let homes = std::fs::read_dir(fixture.root.path().join("configured-sdk-homes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(homes.len(), 1);
    let config = homes[0].join(".backboard/config.json");
    let retained = homes[0].join(".backboard/original.json");
    let bytes = std::fs::read(&config).unwrap();
    let inode = std::fs::metadata(&config).unwrap().ino();
    std::fs::rename(&config, &retained).unwrap();
    private_file(&config, &bytes);
    assert_ne!(std::fs::metadata(&config).unwrap().ino(), inode);
    let replacement = fixture.physical_pins();
    let project = fixture.create_and_bind().await;
    let (_, operation) = fixture.open(&project, &receipt).await;
    assert!(matches!(
        operation.result,
        OwnerResult::Applied | OwnerResult::Rejected { .. }
    ));
    assert_eq!(fixture.count("configured_master_profiles"), 0);
    assert_eq!(fixture.account_record(), original);
    assert_eq!(fixture.physical_pins(), replacement);
    assert_eq!(std::fs::read(&config).unwrap(), bytes);
    assert!(retained.is_file());
    fixture.assert_no_execution();
    fixture.stop().await;
}
