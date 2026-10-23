use crazyagent::client_controller::OwnerController;
use crazyagent::config::{ModelAuthentication, ModelConnection, ModelEndpoint};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::*;
use crazyagent::master_session::{
    MasterBackend, MasterOwnerAction, MasterSelection, MasterSessionPhase, MasterSessionView,
    RcliProfileRegistration, RcliProfileRegistrationMetadata,
};
use crazyagent::protocol::{
    ConfigId, CredentialRef, Cursor, PROTOCOL_VERSION, ProjectId, ProjectSpec, RequestId, SessionId,
};
use crazyagent::routing::{ResourceBudget, RouteRequest};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::watch;

const CATALOG: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = ["local-profile"]
[routing]
approved_accounts = ["local-account"]
global_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
project_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
task_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
[model_access]
accounts = ["local-account"]
[[model_access.connections]]
id = "local-connection"
provider = "local-provider"
harness = "rcli"
protocol = "openai-chat"
account_ref = "local-account"
endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }
model = { id = "local-model", author = "local-author" }
auth = { method = "none" }
[[model_access.pools]]
id = "local-pool"
members = ["local-connection"]
projects = ["rcli-project"]
roles = ["project_leader"]
fallback_allowed = false
"#;

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn project() -> ProjectId {
    ProjectId::new("rcli-project").unwrap()
}

fn private_directory(path: &Path) {
    std::fs::create_dir(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn changed_hex(value: &str) -> String {
    assert_eq!(value.len(), 64);
    let mut changed = value.to_owned();
    changed.replace_range(..1, if value.starts_with('a') { "b" } else { "a" });
    assert_ne!(changed, value);
    changed
}

fn registration_mutation(intent: RcliProfileRegistration) -> OwnerMutation {
    OwnerMutation::ProjectMaster {
        action: MasterOwnerAction::RegisterRcliProfile {
            registration: Box::new(intent),
        },
    }
}

fn applied(response: OwnerResponse) -> OwnerApplied {
    let OwnerResponse::Applied { operation } = response else {
        panic!("actual owner application missing: {response:?}");
    };
    operation
}

fn metadata(operation: &OwnerApplied) -> &RcliProfileRegistrationMetadata {
    let OwnerResult::RcliProfileRegistered { registration } = &operation.result else {
        panic!("actual configured registration metadata missing: {operation:?}");
    };
    registration
}

fn rejected(reply: OwnerReply) {
    let code = match reply.response {
        OwnerResponse::Rejected { code } => code,
        OwnerResponse::Applied { operation } => {
            let OwnerResult::Rejected { code } = operation.result else {
                panic!("invalid registration applied: {operation:?}");
            };
            code
        }
        response => panic!("expected an actual rejection, received {response:?}"),
    };
    assert!(!code.is_empty());
    assert!(code.len() <= 4096);
}

struct Fixture {
    root: tempfile::TempDir,
    service: EngineService,
    _shutdown: watch::Sender<bool>,
    global: PathBuf,
    home: PathBuf,
}

impl Fixture {
    async fn start(api_key: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-rpr-")
            .tempdir_in("/private/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        let catalog = if api_key {
            CATALOG.replace(
                "auth = { method = \"none\" }",
                "auth = { method = \"api_key\", credential_ref = \"owner-key-reference\" }",
            )
        } else {
            CATALOG.to_owned()
        };
        private_file(&global, catalog.as_bytes());
        let home = root.path().join("configured-home");
        private_directory(&home);
        private_directory(&home.join(".backboard"));
        private_file(
            &home.join(".backboard/config.json"),
            &serde_json::to_vec(&json!({
                "model": {"provider": "local-provider", "model": "local-model"},
                "providers": [{
                    "id": "local-provider", "name": "Local provider",
                    "protocol": "openai-chat", "baseUrl": "http://127.0.0.1:11434/v1",
                    "auth": {"type": if api_key { "apiKey" } else { "none" }}
                }]
            }))
            .unwrap(),
        );
        let (shutdown, stopped) = watch::channel(false);
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
            _shutdown: shutdown,
            global,
            home,
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
            .request(OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new("registration-read").unwrap(),
                expected_revision: Cursor::new(0),
                operation: OwnerOperation::Read { query },
            })
            .await
            .unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("actual owner read missing: {reply:?}");
        };
        state
    }

    async fn open(&self) -> (OwnerController, MasterSessionView, ModelConnection) {
        let mut controller = OwnerController::attach(
            self.root.path(),
            OwnerClientKind::Tui,
            Duration::from_secs(3),
        )
        .await
        .unwrap();
        controller.read(OwnerRead::Capabilities).await.unwrap();
        controller
            .begin_mutation(
                OwnerMutation::CreateProject {
                    project_id: project(),
                    spec: ProjectSpec {
                        name: "Configured SDK registration".into(),
                        repositories: vec![],
                    },
                },
                1,
            )
            .unwrap();
        assert_eq!(
            applied(controller.send_pending().await.unwrap()).result,
            OwnerResult::Applied
        );
        let catalog = controller
            .read(OwnerRead::ModelAccessRecord {
                collection: ModelAccessCollection::Connections,
                id: reference("local-connection"),
            })
            .await
            .unwrap();
        let OwnerData::ModelAccessRecord {
            source_hash,
            writable,
            record: ModelAccessRecord::Connection { connection },
        } = catalog.data
        else {
            panic!("actual configured connection missing");
        };
        assert!(writable);
        controller
            .begin_mutation(
                OwnerMutation::ProjectMaster {
                    action: MasterOwnerAction::Open {
                        project_id: project(),
                        expected_config_id: catalog.config_id,
                        expected_source_hash: source_hash,
                        selection: Box::new(MasterSelection {
                            backend: MasterBackend::Rcli,
                            pool_ref: reference("local-pool"),
                            target_profile_ref: reference("local-profile"),
                            route: RouteRequest {
                                preferred_target: Some(reference("local-profile")),
                                required_target: Some(reference("local-profile")),
                                required_capabilities: vec![],
                                account_ref: reference("local-account"),
                                budget_ref: reference("conversation-budget"),
                                resources: ResourceBudget::unit(),
                            },
                        }),
                    },
                },
                2,
            )
            .unwrap();
        let opened = applied(controller.send_pending().await.unwrap());
        assert_eq!(opened.result, OwnerResult::Applied);
        let state = controller
            .read(OwnerRead::ProjectMaster {
                project_id: project(),
            })
            .await
            .unwrap();
        let OwnerData::ProjectMaster {
            session: Some(session),
        } = state.data
        else {
            panic!("actual committed master session missing");
        };
        let session = *session;
        assert_eq!(session.owner, opened.provenance);
        assert_eq!(session.phase, MasterSessionPhase::Configured);
        assert_eq!(session.current_turn, None);
        assert_eq!(session.config_id, state.config_id);
        assert_eq!(session.selection.route.account_ref, connection.account_ref);
        (controller, session, connection)
    }

    fn intent(
        &self,
        session: &MasterSessionView,
        connection: &ModelConnection,
    ) -> RcliProfileRegistration {
        RcliProfileRegistration {
            expected_session: session.clone(),
            expected_connection: connection.clone(),
            configured_home: self.home.clone(),
        }
    }

    async fn request(
        &self,
        owner: &OwnerClient,
        id: &str,
        intent: RcliProfileRegistration,
    ) -> OwnerRequest {
        let basis = self.read(owner, OwnerRead::Capabilities).await;
        OwnerRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(id).unwrap(),
            expected_revision: basis.revision,
            operation: OwnerOperation::Apply {
                mutation: registration_mutation(intent),
            },
        }
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

    fn registration_count(&self) -> i64 {
        self.db()
            .query_row(
                "SELECT COUNT(*) FROM configured_master_profiles",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn registration_body(&self, session: &SessionId) -> String {
        self.db()
            .query_row(
                "SELECT body FROM configured_master_profiles WHERE session=?1",
                [session.as_str()],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn assert_no_execution(&self) {
        let db = self.db();
        for table in [
            "master_turns",
            "master_preparations",
            "model_gateway_attempts",
            "model_gateway_observations",
            "native_auth_operations",
            "worktree_leases",
            "bootstrap_workspaces",
        ] {
            let count: i64 = db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "registration allocated {table}");
        }
        assert!(!self.home.join(".backboard/keys.json").exists());
    }

    fn assert_receipt(
        &self,
        session: &MasterSessionView,
        connection: &ModelConnection,
        request: &OwnerRequest,
        operation: &OwnerApplied,
    ) {
        let public = metadata(operation);
        assert_eq!(public.project_id, session.project_id);
        assert_eq!(public.session_id, session.session_id);
        assert_eq!(public.profile_ref, session.selection.target_profile_ref);
        assert!(public.generation > 0);
        let encoded = serde_json::to_value(public).unwrap();
        let mut fields = encoded
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "generation",
                "profile_ref",
                "project_id",
                "registration_id",
                "session_id"
            ]
        );
        assert!(serde_json::to_vec(&operation.result).unwrap().len() <= 4096);
        assert_eq!(operation.provenance.operation_id, request.request_id);
        assert_eq!(
            operation.provenance.request_hash,
            request.mutation_hash().unwrap()
        );
        assert_eq!(operation.provenance.owner_id, session.owner.owner_id);
        assert_eq!(
            operation.provenance.engine_epoch,
            session.owner.engine_epoch
        );
        assert_eq!(
            operation.provenance.admitted_epoch,
            session.owner.resulting_epoch
        );
        let record: Value =
            serde_json::from_str(&self.registration_body(&session.session_id)).unwrap();
        assert_eq!(record["project_id"], json!(session.project_id));
        assert_eq!(record["session_id"], json!(session.session_id));
        assert_eq!(record["owner"], json!(session.owner));
        let proof = &record["proof"];
        assert_eq!(proof["registration_id"], json!(public.registration_id));
        assert_eq!(proof["generation"], json!(public.generation));
        assert_eq!(proof["profile_ref"], json!(public.profile_ref));
        assert_eq!(proof["connection"], json!(connection));
        assert_eq!(proof["config_id"], json!(session.config_id));
        assert_eq!(proof["source_hash"], json!(session.source_hash));
        let db = self.db();
        let (count, bytes, hash): (i64, Vec<u8>, Vec<u8>) = db
            .query_row(
                "SELECT COUNT(*),result,request_hash FROM owner_operations WHERE origin='native_owner' AND owner_id=?1 AND operation_id=?2",
                params![session.owner.owner_id, request.request_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            serde_json::from_slice::<OwnerResult>(&bytes).unwrap(),
            operation.result
        );
        let stored_hash = hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(stored_hash, request.mutation_hash().unwrap());
    }

    async fn close(mut self) {
        tokio::time::timeout(Duration::from_secs(5), self.service.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn rcli_profile_registration_uses_real_owner_and_committed_session() {
    for api_key in [false, true] {
        let fixture = Fixture::start(api_key).await;
        let (mut controller, session, connection) = fixture.open().await;
        let config_path = fixture.home.join(".backboard/config.json");
        let original_config = std::fs::read(&config_path).unwrap();
        assert_eq!(fixture.registration_count(), 0);
        controller
            .begin_mutation(
                registration_mutation(fixture.intent(&session, &connection)),
                3,
            )
            .unwrap();
        let pending = controller.pending().unwrap();
        let exact = pending.request().clone();
        assert_eq!(pending.generation(), 3);
        assert!(!pending.uncertain());
        assert_eq!(pending.hash(), exact.mutation_hash().unwrap());
        assert_eq!(fixture.registration_count(), 0);
        let operation = applied(controller.send_pending().await.unwrap());
        assert!(!operation.historical_replay);
        assert!(controller.pending().is_none());
        assert_eq!(
            operation.provenance.revision_before,
            exact.expected_revision
        );
        assert_eq!(
            operation.provenance.revision_after.value(),
            exact.expected_revision.value() + 1
        );
        assert_eq!(
            operation.provenance.event_cursor,
            Some(operation.current_revision.clone())
        );
        assert_eq!(fixture.registration_count(), 1);
        fixture.assert_receipt(&session, &connection, &exact, &operation);
        assert_eq!(std::fs::read(config_path).unwrap(), original_config);
        let read = controller
            .read(OwnerRead::ProjectMaster {
                project_id: project(),
            })
            .await
            .unwrap();
        assert_eq!(
            read.data,
            OwnerData::ProjectMaster {
                session: Some(Box::new(session))
            }
        );
        fixture.assert_no_execution();
        fixture.close().await;
    }
}

#[tokio::test]
async fn rcli_profile_registration_reconciles_exact_pending_request_once() {
    let fixture = Fixture::start(false).await;
    let (mut controller, session, connection) = fixture.open().await;
    controller
        .begin_mutation(
            registration_mutation(fixture.intent(&session, &connection)),
            11,
        )
        .unwrap();
    let exact = controller.pending().unwrap().request().clone();
    let hash = controller.pending().unwrap().hash().to_owned();
    let owner = fixture.owner(OwnerClientKind::Tui).await;
    let first = applied(owner.request(exact.clone()).await.unwrap().response);
    let original_body = fixture.registration_body(&session.session_id);
    assert!(!first.historical_replay);
    assert_eq!(controller.pending().unwrap().request(), &exact);
    controller.detach();
    controller.reattach(fixture.root.path()).await.unwrap();
    let pending = controller.pending().unwrap();
    assert_eq!(pending.request(), &exact);
    assert_eq!(pending.hash(), hash);
    assert_eq!(pending.generation(), 11);
    let response = controller.reconcile_pending().await.unwrap();
    let OwnerResponse::Committed {
        operation: reconciled,
    } = response
    else {
        panic!("actual owner receipt was not reconciled: {response:?}");
    };
    assert_eq!(reconciled.provenance, first.provenance);
    assert_eq!(reconciled.result, first.result);
    assert!(controller.pending().is_none());
    let replay = applied(owner.request(exact.clone()).await.unwrap().response);
    assert!(replay.historical_replay);
    assert_eq!(replay.provenance, first.provenance);
    assert_eq!(replay.result, first.result);
    fixture.assert_receipt(&session, &connection, &exact, &replay);
    assert_eq!(fixture.registration_count(), 1);
    assert_eq!(
        fixture.registration_body(&session.session_id),
        original_body
    );
    let absent_home = fixture.root.path().join("never-selected-home");
    let mut changed = exact.clone();
    let OwnerOperation::Apply {
        mutation:
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::RegisterRcliProfile { registration },
            },
    } = &mut changed.operation
    else {
        panic!("retained registration action changed");
    };
    registration.configured_home = absent_home.clone();
    let reply = owner.request(changed).await.unwrap();
    assert!(matches!(
        reply.response,
        OwnerResponse::Rejected { code } if code == "operation_identity_conflict"
    ));
    assert!(!absent_home.exists());
    assert_eq!(fixture.registration_count(), 1);
    assert_eq!(
        fixture.registration_body(&session.session_id),
        original_body
    );
    fixture.assert_no_execution();
    fixture.close().await;
}

#[tokio::test]
async fn rcli_profile_registration_rejects_stale_scope_and_physical_replacement() {
    let fixture = Fixture::start(false).await;
    let (mut controller, session, connection) = fixture.open().await;
    let owner = fixture.owner(OwnerClientKind::Tui).await;
    for field in [
        "project",
        "session",
        "version",
        "config",
        "source",
        "owner",
        "epoch",
        "owner_epoch",
        "profile",
        "connection_id",
        "provider",
        "harness",
        "protocol",
        "account",
        "endpoint",
        "model",
        "author",
        "authentication",
    ] {
        let mut intent = fixture.intent(&session, &connection);
        match field {
            "project" => {
                intent.expected_session.project_id = ProjectId::new("other-project").unwrap()
            }
            "session" => {
                intent.expected_session.session_id = SessionId::new("other-session").unwrap()
            }
            "version" => intent.expected_session.version = Cursor::new(session.version.value() + 1),
            "config" => {
                intent.expected_session.config_id =
                    ConfigId::new(changed_hex(session.config_id.as_str())).unwrap()
            }
            "source" => intent.expected_session.source_hash = changed_hex(&session.source_hash),
            "owner" => {
                intent.expected_session.owner.owner_id = changed_hex(&session.owner.owner_id)
            }
            "epoch" => {
                intent.expected_session.owner.engine_epoch =
                    changed_hex(&session.owner.engine_epoch)
            }
            "owner_epoch" => intent.expected_session.owner.resulting_epoch += 1,
            "profile" => {
                intent.expected_session.selection.target_profile_ref = reference("other-profile")
            }
            "connection_id" => intent.expected_connection.id = reference("other-connection"),
            "provider" => intent.expected_connection.provider = reference("other-provider"),
            "harness" => intent.expected_connection.harness = reference("other-harness"),
            "protocol" => intent.expected_connection.protocol = reference("other-protocol"),
            "account" => intent.expected_connection.account_ref = reference("other-account"),
            "endpoint" => {
                intent.expected_connection.endpoint = ModelEndpoint::Http {
                    url: "http://127.0.0.1:9999/v1".into(),
                }
            }
            "model" => intent.expected_connection.model.id = "other-model".into(),
            "author" => intent.expected_connection.model.author = reference("other-author"),
            "authentication" => {
                intent.expected_connection.auth = ModelAuthentication::ApiKey {
                    credential_ref: reference("other-key-reference"),
                }
            }
            _ => unreachable!(),
        }
        let request = fixture
            .request(&owner, &format!("reject-{field}"), intent)
            .await;
        rejected(owner.request(request).await.unwrap());
        assert_eq!(fixture.registration_count(), 0, "accepted stale {field}");
        fixture.assert_no_execution();
    }
    let request = fixture
        .request(
            &owner,
            "reject-other-owner",
            fixture.intent(&session, &connection),
        )
        .await;
    let desktop = fixture.owner(OwnerClientKind::Desktop).await;
    rejected(desktop.request(request).await.unwrap());
    assert_eq!(fixture.registration_count(), 0);
    let config_path = fixture.home.join(".backboard/config.json");
    let saved_config = fixture.home.join(".backboard/original.json");
    let original_config = std::fs::read(&config_path).unwrap();
    let request = fixture
        .request(
            &owner,
            "reject-missing-config",
            fixture.intent(&session, &connection),
        )
        .await;
    std::fs::rename(&config_path, &saved_config).unwrap();
    rejected(owner.request(request).await.unwrap());
    assert!(!config_path.exists());
    assert_eq!(fixture.registration_count(), 0);
    symlink(&saved_config, &config_path).unwrap();
    let request = fixture
        .request(
            &owner,
            "reject-symlink-config",
            fixture.intent(&session, &connection),
        )
        .await;
    rejected(owner.request(request).await.unwrap());
    assert_eq!(fixture.registration_count(), 0);
    std::fs::remove_file(&config_path).unwrap();
    std::fs::rename(&saved_config, &config_path).unwrap();
    let source_bytes = std::fs::read(&fixture.global).unwrap();
    let request = fixture
        .request(
            &owner,
            "reject-actual-source-drift",
            fixture.intent(&session, &connection),
        )
        .await;
    let mut changed_source = source_bytes.clone();
    changed_source.push(b'\n');
    private_file(&fixture.global, &changed_source);
    rejected(owner.request(request).await.unwrap());
    assert_eq!(std::fs::read(&fixture.global).unwrap(), changed_source);
    assert_eq!(fixture.registration_count(), 0);
    private_file(&fixture.global, &source_bytes);
    controller
        .read(OwnerRead::ProjectMaster {
            project_id: project(),
        })
        .await
        .unwrap();
    controller
        .begin_mutation(
            registration_mutation(fixture.intent(&session, &connection)),
            31,
        )
        .unwrap();
    let registered = applied(controller.send_pending().await.unwrap());
    assert!(metadata(&registered).generation > 0);
    let original_body = fixture.registration_body(&session.session_id);
    let original_identity = std::fs::metadata(&config_path).unwrap();
    std::fs::rename(&config_path, &saved_config).unwrap();
    private_file(&config_path, &original_config);
    let replacement = std::fs::metadata(&config_path).unwrap();
    assert_ne!(
        (original_identity.dev(), original_identity.ino()),
        (replacement.dev(), replacement.ino())
    );
    let request = fixture
        .request(
            &owner,
            "reject-replaced-config",
            fixture.intent(&session, &connection),
        )
        .await;
    rejected(owner.request(request).await.unwrap());
    assert_eq!(fixture.registration_count(), 1);
    assert_eq!(
        fixture.registration_body(&session.session_id),
        original_body
    );
    assert_eq!(std::fs::read(config_path).unwrap(), original_config);
    fixture.assert_no_execution();
    fixture.close().await;
}
