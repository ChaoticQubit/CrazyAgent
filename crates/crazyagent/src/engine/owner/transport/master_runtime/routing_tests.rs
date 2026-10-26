use super::*;
use crate::engine::NativeProfileCustody;
use crate::engine::bootstrap::{EngineService, NativeProfileUsePurpose};
use crate::engine::owner::{
    HostStamp, ModelAccessCollection, OwnerClient, OwnerClientKind, OwnerData, OwnerMutation,
    OwnerOperation, OwnerRead, OwnerReadReply, OwnerReply, OwnerRequest, OwnerResponse,
    OwnerResult,
};
use crate::master_session::{
    MasterBootstrapAuth, MasterOwnerAction, MasterSelection, MasterSessionView,
    RcliConfiguredProfileProof,
};
use crate::protocol::{CredentialRef, Cursor, PROTOCOL_VERSION, ProjectSpec};
use crate::routing::{ResourceBudget, RouteRequest};
use rusqlite::{Connection, OpenFlags, Transaction, params, types::Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::watch;

const TEST_BOUND: Duration = Duration::from_secs(10);
const EFFECT_TABLES: [&str; 6] = [
    "master_preparations",
    "model_gateway_attempts",
    "model_gateway_observations",
    "native_auth_operations",
    "worktree_leases",
    "bootstrap_workspaces",
];
const CATALOG: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = ["codex-target", "local-profile", "claude-target"]
[routing]
approved_accounts = ["codex-account", "local-account", "claude-account"]
global_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
project_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
task_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
[model_access]
accounts = ["codex-account", "local-account", "claude-account"]
[[model_access.connections]]
id = "claude-connection"
provider = "anthropic"
harness = "claude-code"
protocol = "native"
account_ref = "claude-account"
endpoint = { kind = "native" }
model = { id = "configured-claude-model", author = "anthropic" }
auth = { method = "native_managed", profile_ref = "claude-profile" }
[[model_access.connections]]
id = "codex-connection"
provider = "openai"
harness = "codex"
protocol = "native"
account_ref = "codex-account"
endpoint = { kind = "native" }
model = { id = "configured-codex-model", author = "openai" }
auth = { method = "native_managed", profile_ref = "codex-profile" }
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
id = "codex-pool"
members = ["codex-connection"]
projects = ["p"]
roles = ["project_leader"]
fallback_allowed = false
[[model_access.pools]]
id = "claude-pool"
members = ["claude-connection"]
projects = ["p"]
roles = ["project_leader"]
fallback_allowed = false
[[model_access.pools]]
id = "local-pool"
members = ["local-connection"]
projects = ["p"]
roles = ["project_leader"]
fallback_allowed = false
"#;
const LOCAL_CONFIG: &[u8] = br#"{
    "model": {"provider": "local-provider", "model": "local-model"},
    "providers": [{"id": "local-provider", "name": "Local provider",
        "protocol": "openai-chat", "baseUrl": "http://127.0.0.1:11434/v1",
        "auth": {"type": "none"}}]
}"#;

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn private_file(path: &Path, contents: &[u8]) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn private_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn selection(backend: MasterBackend) -> MasterSelection {
    let (pool, target, account) = match backend {
        MasterBackend::Codex => ("codex-pool", "codex-target", "codex-account"),
        MasterBackend::Rcli => ("local-pool", "local-profile", "local-account"),
        MasterBackend::Claude => ("claude-pool", "claude-target", "claude-account"),
    };
    MasterSelection {
        backend,
        pool_ref: reference(pool),
        target_profile_ref: reference(target),
        route: RouteRequest {
            preferred_target: Some(reference(target)),
            required_target: Some(reference(target)),
            required_capabilities: vec![],
            account_ref: reference(account),
            budget_ref: reference("conversation-budget"),
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

fn applied(reply: &OwnerReply) {
    let OwnerResponse::Applied { operation } = &reply.response else {
        panic!("actual owner operation missing: {reply:?}");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    assert!(!operation.historical_replay);
}

fn git(repository: &Path, home: &Path, arguments: &[&str]) {
    let mut child = crate::spawn_gate::spawn(
        std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(["-c", "user.name=Master routing fixture"])
            .args(["-c", "user.email=master-routing@example.invalid"])
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let deadline = Instant::now() + TEST_BOUND;
    loop {
        let failure = match child.try_wait() {
            Ok(Some(status)) => {
                assert!(status.success(), "actual Git fixture failed");
                return;
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            Ok(None) => None,
            Err(error) => Some(error),
        };
        let retired = crate::engine::native_auth_host::custody::cleanup_direct_child(
            &mut child,
            Instant::now() + TEST_BOUND,
        );
        assert!(
            matches!(retired, Ok(Some(_))),
            "actual Git fixture cleanup uncertain: {retired:?}"
        );
        panic!("actual Git fixture timed out or failed to wait and was reaped: {failure:?}");
    }
}

fn rows(tx: &Transaction<'_>, table: &str) -> Vec<Vec<Value>> {
    let mut statement = tx
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
        .unwrap();
    let columns = statement.column_count();
    let values = statement
        .query_map([], |row| {
            (0..columns).map(|column| row.get(column)).collect()
        })
        .unwrap();
    values.collect::<Result<Vec<_>, _>>().unwrap()
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    revision: u64,
    events: Vec<Vec<Value>>,
    sessions: Vec<Vec<Value>>,
    turns: Vec<Vec<Value>>,
    registrations: Vec<Vec<Value>>,
    effects: [Vec<Vec<Value>>; 6],
    captures: Vec<PathBuf>,
}

impl Snapshot {
    fn assert_no_effects(&self) {
        for (table, values) in EFFECT_TABLES.iter().zip(&self.effects) {
            assert!(values.is_empty(), "unexpected persisted effects in {table}");
        }
        assert!(self.captures.is_empty());
    }
}

struct Fixture {
    root: tempfile::TempDir,
    service: EngineService,
    owner: OwnerClient,
    session: MasterSessionView,
    turn: MasterTurnView,
    submitted: OwnerRequest,
    repository: PathBuf,
    home: PathBuf,
    _stop: watch::Sender<bool>,
}

impl Fixture {
    async fn new(backend: MasterBackend, with_repository: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-mrt-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        private_file(&global, CATALOG.as_bytes());
        let home = root.path().join("configured-home");
        if backend == MasterBackend::Rcli {
            private_directory(&home);
            private_directory(&home.join(".backboard"));
            private_file(&home.join(".backboard/config.json"), LOCAL_CONFIG);
        }
        let repository = root.path().join("repository");
        if with_repository {
            private_directory(&repository);
            git(
                &repository,
                root.path(),
                &["init", "--initial-branch=trunk"],
            );
            private_file(
                &repository.join("README"),
                b"Actual master routing fixture.\n",
            );
            git(&repository, root.path(), &["add", "README"]);
            git(
                &repository,
                root.path(),
                &["commit", "-qm", "Initialize master routing fixture"],
            );
        }
        let (stop, stopped) = watch::channel(false);
        let service = tokio::time::timeout(
            TEST_BOUND,
            EngineService::start_with_settings_sources(
                root.path(),
                "e.sock",
                &global,
                &root.path().join("project.toml"),
                &stopped,
            ),
        )
        .await
        .expect("actual engine startup exceeded the test bound")
        .unwrap();
        let owner = OwnerClient::discover(root.path(), OwnerClientKind::Tui, TEST_BOUND)
            .await
            .unwrap()
            .0;
        let project = ProjectId::new("p").unwrap();
        let create = request(
            "routing-create",
            Self::read_owner(&owner, OwnerRead::Capabilities)
                .await
                .revision
                .value(),
            OwnerOperation::Apply {
                mutation: OwnerMutation::CreateProject {
                    project_id: project.clone(),
                    spec: ProjectSpec {
                        name: "Actual master routing".into(),
                        repositories: if with_repository {
                            vec![repository.to_str().unwrap().into()]
                        } else {
                            vec![]
                        },
                    },
                },
            },
        );
        applied(&owner.request(create).await.unwrap());
        let basis = Self::read_owner(
            &owner,
            OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Pools,
                cursor: None,
                limit: 16,
            },
        )
        .await;
        let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
            panic!("actual model source basis missing");
        };
        let open = request(
            "routing-open",
            basis.revision.value(),
            OwnerOperation::Apply {
                mutation: OwnerMutation::ProjectMaster {
                    action: MasterOwnerAction::Open {
                        project_id: project.clone(),
                        expected_config_id: basis.config_id,
                        expected_source_hash: source_hash,
                        selection: Box::new(selection(backend)),
                    },
                },
            },
        );
        applied(&Self::commit_owner(&service, &owner, &open).await);
        let session = Self::read_session(&owner, &project).await;
        let submitted = request(
            "routing-turn",
            Self::read_owner(&owner, OwnerRead::Capabilities)
                .await
                .revision
                .value(),
            OwnerOperation::Apply {
                mutation: OwnerMutation::ProjectMaster {
                    action: MasterOwnerAction::Submit {
                        project_id: project.clone(),
                        session_id: session.session_id.clone(),
                        text: "Describe this project in one sentence without using tools.".into(),
                    },
                },
            },
        );
        applied(&Self::commit_owner(&service, &owner, &submitted).await);
        let state = Self::read_owner(
            &owner,
            OwnerRead::ProjectMasterTurns {
                project_id: project.clone(),
                session_id: session.session_id.clone(),
                cursor: None,
                limit: 16,
            },
        )
        .await;
        let OwnerData::ProjectMasterTurns { mut page } = state.data else {
            panic!("actual queued turn page missing");
        };
        assert_eq!(page.turns.len(), 1);
        let turn = page.turns.remove(0);
        let session = Self::read_session(&owner, &project).await;
        assert_eq!(turn.phase, MasterTurnPhase::Queued);
        assert_eq!(turn.id, submitted.request_id);
        assert_eq!(session.current_turn.as_ref(), Some(&turn.id));
        let fixture = Self {
            root,
            service,
            owner,
            session,
            turn,
            submitted,
            repository,
            home,
            _stop: stop,
        };
        fixture.snapshot().assert_no_effects();
        fixture
    }

    fn runtime(&self) -> &OwnerRuntimeHandle {
        self.service.owner_runtime().unwrap()
    }

    async fn read_owner(owner: &OwnerClient, query: OwnerRead) -> OwnerReadReply {
        let reply = owner
            .request(request("routing-read", 0, OwnerOperation::Read { query }))
            .await
            .unwrap();
        let OwnerResponse::Read { state } = reply.response else {
            panic!("actual owner read failed: {reply:?}");
        };
        state
    }

    async fn read_session(owner: &OwnerClient, project: &ProjectId) -> MasterSessionView {
        let state = Self::read_owner(
            owner,
            OwnerRead::ProjectMaster {
                project_id: project.clone(),
            },
        )
        .await;
        let OwnerData::ProjectMaster {
            session: Some(session),
        } = state.data
        else {
            panic!("actual committed session missing");
        };
        *session
    }

    async fn commit_owner(
        service: &EngineService,
        owner: &OwnerClient,
        request: &OwnerRequest,
    ) -> OwnerReply {
        let runtime = service.owner_runtime().unwrap();
        let mut auth = [0; 37];
        auth[..4].copy_from_slice(b"CAO1");
        auth[4] = 0;
        auth[5..].copy_from_slice(&owner.token);
        let admission = runtime.authority.authenticate(&auth).unwrap();
        let _barrier = tokio::time::timeout(TEST_BOUND, runtime.engine.owner_egress.write())
            .await
            .expect("actual owner barrier exceeded the test bound");
        runtime
            .engine
            .dispatch_owner(admission, request.clone(), None)
            .await
            .unwrap()
    }

    async fn command(
        &self,
        command: MasterRuntimeCommand,
    ) -> Result<MasterRuntimeResult, EngineError> {
        tokio::time::timeout(TEST_BOUND, self.runtime().master_runtime_command(command))
            .await
            .expect("actual runtime actor exceeded the test bound")
    }

    fn route(&self, host: HostStamp, project: ProjectId, turn: RequestId) -> MasterRuntimeCommand {
        MasterRuntimeCommand::Route {
            host,
            project_id: project,
            session_id: self.session.session_id.clone(),
            turn_id: turn,
        }
    }

    async fn drive(&self) -> Result<MasterTurnView, EngineError> {
        tokio::time::timeout(
            TEST_BOUND,
            self.runtime().drive_master_turn(
                &self.session.project_id,
                &self.session.session_id,
                &self.turn.id,
            ),
        )
        .await
        .expect("actual master driver exceeded the test bound")
    }

    async fn persisted_turn(&self) -> MasterTurnView {
        let state = Self::read_owner(
            &self.owner,
            OwnerRead::ProjectMasterTurns {
                project_id: self.session.project_id.clone(),
                session_id: self.session.session_id.clone(),
                cursor: None,
                limit: 16,
            },
        )
        .await;
        let OwnerData::ProjectMasterTurns { mut page } = state.data else {
            panic!("actual persisted turn page missing");
        };
        assert_eq!(page.turns.len(), 1);
        page.turns.remove(0)
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

    fn snapshot(&self) -> Snapshot {
        let mut db = self.db();
        let tx = db.transaction().unwrap();
        let capture_root = &self.runtime().authority.root;
        capture_root.verify().unwrap();
        assert_eq!(capture_root.path.parent(), Some(self.root.path()));
        let mut captures: Vec<_> = fs::read_dir(&capture_root.path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("master-runtime-")
            })
            .collect();
        captures.sort();
        let revision: i64 = tx
            .query_row("SELECT revision FROM state WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        Snapshot {
            revision: u64::try_from(revision).expect("actual owner revision must be nonnegative"),
            events: rows(&tx, "events"),
            sessions: rows(&tx, "master_sessions"),
            turns: rows(&tx, "master_turns"),
            registrations: rows(&tx, "configured_master_profiles"),
            effects: EFFECT_TABLES.map(|table| rows(&tx, table)),
            captures,
        }
    }

    async fn register(&self) -> RcliConfiguredProfileProof {
        let result = self
            .command(MasterRuntimeCommand::RegisterRcliProfile {
                host: self.runtime().identity(),
                project_id: self.session.project_id.clone(),
                session_id: self.session.session_id.clone(),
                configured_home: self.home.clone(),
            })
            .await
            .unwrap();
        let MasterRuntimeResult::ConfiguredProfile(proof) = result else {
            panic!("actual configured RCLI registration proof missing");
        };
        *proof
    }

    async fn cancel(&self) {
        let revision = Self::read_owner(&self.owner, OwnerRead::Capabilities)
            .await
            .revision
            .value();
        let cancel = request(
            "routing-cancel",
            revision,
            OwnerOperation::Apply {
                mutation: OwnerMutation::ProjectMaster {
                    action: MasterOwnerAction::Cancel {
                        project_id: self.session.project_id.clone(),
                        session_id: self.session.session_id.clone(),
                        turn_id: self.turn.id.clone(),
                    },
                },
            },
        );
        applied(&Self::commit_owner(&self.service, &self.owner, &cancel).await);
    }

    async fn finish(mut self) {
        let engine = Arc::downgrade(&self.runtime().engine);
        tokio::time::timeout(TEST_BOUND, self.service.shutdown())
            .await
            .expect("actual owner shutdown exceeded the test bound")
            .unwrap();
        drop(self.service);
        assert!(engine.upgrade().is_none());
        assert!(self.root.path().is_dir());
    }
}

fn unallocated(turn: &MasterTurnView, phase: MasterTurnPhase, reason: Option<MasterReason>) {
    assert_eq!(turn.phase, phase);
    assert_eq!(turn.reason, reason);
    assert!(turn.attempt.is_none());
    assert!(turn.runtime.is_none());
    assert!(turn.reply.is_none());
}

#[tokio::test]
async fn claude_drive_runs_the_native_readiness_checks_without_preparation_attempt_or_runtime() {
    let case = Fixture::new(MasterBackend::Claude, false).await;
    let before = case.snapshot();
    assert!(!case.repository.exists());
    assert!(!case.home.exists());
    let returned = case.drive().await.unwrap();
    unallocated(
        &returned,
        MasterTurnPhase::Blocked,
        Some(MasterReason::ConnectionChanged),
    );
    assert_eq!(returned.id, case.turn.id);
    assert_eq!(returned.session_id, case.turn.session_id);
    assert_eq!(returned.ordinal, case.turn.ordinal);
    assert_eq!(returned.input_cursor, case.turn.input_cursor);
    assert_eq!(returned, case.persisted_turn().await);
    let after = case.snapshot();
    after.assert_no_effects();
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.events.len(), before.events.len() + 1);
    assert_eq!(
        &after.events[..before.events.len()],
        before.events.as_slice()
    );
    assert_eq!(after.registrations, before.registrations);
    let session = Fixture::read_session(&case.owner, &case.session.project_id).await;
    assert!(session.current_turn.is_none());
    assert_eq!(session.version.value(), after.revision);
    let stored: String = case
        .db()
        .query_row(
            "SELECT body FROM master_turns WHERE session=?1 AND id=?2",
            params![case.session.session_id.as_str(), case.turn.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<MasterTurnView>(&stored).unwrap(),
        returned
    );
    case.finish().await;
}

#[tokio::test]
async fn codex_subscription_master_is_not_blocked_as_an_unsupported_backend() {
    let case = Fixture::new(MasterBackend::Codex, false).await;
    let returned = case.drive().await;
    let persisted = case.persisted_turn().await;
    assert_ne!(
        persisted.reason,
        Some(MasterReason::UnsupportedCapability),
        "{returned:?}"
    );
    if let Ok(turn) = &returned {
        assert_ne!(turn.reason, Some(MasterReason::UnsupportedCapability));
    }
    assert!(
        !matches!(&returned, Err(error) if error.0.contains("master_runtime_backend_unsupported")),
        "{returned:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn blocked_drive_replay_returns_the_same_persisted_view_without_new_work() {
    let case = Fixture::new(MasterBackend::Claude, false).await;
    let blocked = case.drive().await.unwrap();
    unallocated(
        &blocked,
        MasterTurnPhase::Blocked,
        Some(MasterReason::ConnectionChanged),
    );
    let before = case.snapshot();
    assert_eq!(case.drive().await.unwrap(), blocked);
    assert_eq!(case.persisted_turn().await, blocked);
    let replay = Fixture::commit_owner(&case.service, &case.owner, &case.submitted).await;
    let OwnerResponse::Applied { operation } = replay.response else {
        panic!("actual historical replay missing");
    };
    assert_eq!(operation.result, OwnerResult::Applied);
    assert!(operation.historical_replay);
    assert_eq!(case.snapshot(), before);
    before.assert_no_effects();
    case.finish().await;

    let cancelled = Fixture::new(MasterBackend::Claude, false).await;
    cancelled.cancel().await;
    let persisted = cancelled.persisted_turn().await;
    unallocated(
        &persisted,
        MasterTurnPhase::Cancelled,
        Some(MasterReason::Cancelled),
    );
    let before = cancelled.snapshot();
    assert_eq!(cancelled.drive().await.unwrap(), persisted);
    assert_eq!(cancelled.snapshot(), before);
    before.assert_no_effects();
    cancelled.finish().await;
}

#[tokio::test]
async fn actor_route_enforces_real_host_project_and_turn_joins_before_writes() {
    let case = Fixture::new(MasterBackend::Codex, false).await;
    let foreign = Fixture::new(MasterBackend::Codex, false).await;
    let other = ProjectId::new("other").unwrap();
    let revision = Fixture::read_owner(&case.owner, OwnerRead::Capabilities)
        .await
        .revision
        .value();
    let create = request(
        "routing-other-project",
        revision,
        OwnerOperation::Apply {
            mutation: OwnerMutation::CreateProject {
                project_id: other.clone(),
                spec: ProjectSpec {
                    name: "Actual different project".into(),
                    repositories: vec![],
                },
            },
        },
    );
    applied(&case.owner.request(create).await.unwrap());
    let invalid = [
        (
            case.route(
                foreign.runtime().identity(),
                case.session.project_id.clone(),
                case.turn.id.clone(),
            ),
            "owner_runtime_stale",
        ),
        (
            case.route(case.runtime().identity(), other, case.turn.id.clone()),
            "master_session_scope_conflict",
        ),
        (
            case.route(
                case.runtime().identity(),
                case.session.project_id.clone(),
                RequestId::new("missing-real-turn").unwrap(),
            ),
            "master_turn_missing",
        ),
    ];
    let foreign_before = foreign.snapshot();
    for (command, expected) in invalid {
        let before = case.snapshot();
        let result = case.command(command).await;
        assert_eq!(
            result.err().expect("invalid actor scope accepted").0,
            expected
        );
        assert_eq!(case.snapshot(), before);
        before.assert_no_effects();
        assert_eq!(case.persisted_turn().await, case.turn);
    }
    assert_eq!(foreign.snapshot(), foreign_before);
    foreign.finish().await;
    case.finish().await;
}

#[tokio::test]
async fn rcli_route_preserves_queued_view_and_real_configuration_without_allocating() {
    let case = Fixture::new(MasterBackend::Rcli, false).await;
    let proof = case.register().await;
    assert_eq!(
        proof.database_instance,
        case.runtime().identity().db_instance
    );
    assert_eq!(proof.engine_epoch, case.runtime().identity().engine_epoch);
    assert_eq!(proof.config_id, case.session.config_id);
    assert_eq!(proof.source_hash, case.session.source_hash);
    assert_eq!(proof.profile_ref, case.session.selection.target_profile_ref);
    assert_eq!(proof.connection.harness, reference("rcli"));
    assert_eq!(proof.connection.auth, ModelAuthentication::None {});
    assert!(proof.generation > 0);
    let before = case.snapshot();
    before.assert_no_effects();
    assert_eq!(before.registrations.len(), 1);
    let result = case
        .command(case.route(
            case.runtime().identity(),
            case.session.project_id.clone(),
            case.turn.id.clone(),
        ))
        .await
        .unwrap();
    let MasterRuntimeResult::Turn(returned) = result else {
        panic!("RCLI route allocated instead of returning the persisted view");
    };
    unallocated(&returned, MasterTurnPhase::Queued, None);
    assert_eq!(*returned, case.turn);
    assert_eq!(case.persisted_turn().await, case.turn);
    assert_eq!(case.snapshot(), before);
    assert_eq!(
        fs::read(case.home.join(".backboard/config.json")).unwrap(),
        LOCAL_CONFIG
    );
    assert!(!case.repository.exists());
    assert!(!case.home.join(".backboard/keys.json").exists());
    case.finish().await;
}

#[tokio::test]
async fn rcli_drive_reaches_f_using_the_real_prepared_proof_and_busy_physical_guard() {
    let case = Fixture::new(MasterBackend::Rcli, true).await;
    let proof = case.register().await;
    let result = case
        .command(MasterRuntimeCommand::Prepare {
            host: case.runtime().identity(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
        })
        .await
        .unwrap();
    let MasterRuntimeResult::Prepared(prepared) = result else {
        panic!("real configured RCLI preparation missing");
    };
    let auth = MasterBootstrapAuth::RcliConfigured(proof.clone());
    assert_eq!(prepared.profile.binding(), &auth.binding());
    assert_eq!(prepared.session.project_id, case.session.project_id);
    assert_eq!(prepared.session.session_id, case.session.session_id);
    assert_eq!(prepared.session.current_turn.as_ref(), Some(&case.turn.id));
    assert_eq!(prepared.attempt.request.project_id, case.session.project_id);
    assert_eq!(
        prepared.attempt.request.task_id,
        crate::storage::turn_task_id(&case.session.session_id, &case.turn.id).unwrap()
    );
    assert_eq!(prepared.attempt.connection, proof.connection);
    assert_eq!(
        prepared.attempt.connection.account_ref,
        reference("local-account")
    );
    assert_eq!(prepared.attempt.connection.model.id.as_str(), "local-model");
    assert_eq!(
        prepared.attempt.request.target_profile_ref,
        proof.profile_ref
    );
    assert_eq!(prepared.lease.project_id, case.session.project_id);
    assert_eq!(prepared.lease.task_id, case.session.bootstrap_task_id);
    assert_eq!(prepared.lease.owner_session, case.session.session_id);
    assert_eq!(prepared.repository, case.repository);
    assert_eq!(prepared.cwd, PathBuf::from(&prepared.lease.path));
    assert!(prepared.cwd.join(".git").is_file());
    assert!(prepared.attempt.completion.is_none());
    {
        let mut db = case.db();
        let tx = db.transaction().unwrap();
        let body: String = tx
            .query_row(
                "SELECT body FROM master_preparations WHERE session=?1 AND turn=?2",
                params![case.session.session_id.as_str(), case.turn.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let persisted: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(persisted["auth"], serde_json::to_value(&auth).unwrap());
        assert_eq!(
            persisted["worktree_lease"],
            serde_json::to_value(&prepared.worktree_lease).unwrap()
        );
    }
    let database = case.root.path().join("engine.db");
    let mut custody =
        NativeProfileCustody::capture(&database, &fs::metadata(&database).unwrap()).unwrap();
    let guard = custody
        .acquire_configured_rcli_use(
            &case.runtime().identity(),
            prepared.profile.binding(),
            &proof.profile_ref,
            &case.turn.id,
            case.turn.ordinal,
        )
        .unwrap();
    let before = case.snapshot();
    assert_eq!(case.persisted_turn().await.phase, MasterTurnPhase::Prepared);
    assert_eq!(before.effects.each_ref().map(Vec::len), [1, 1, 0, 0, 1, 1]);
    assert!(before.captures.is_empty());
    let returned = case.drive().await.unwrap();
    assert_eq!(returned.phase, MasterTurnPhase::Delivering);
    assert!(returned.reason.is_none());
    assert!(returned.runtime.is_none());
    assert!(returned.reply.is_none());
    assert_eq!(returned.attempt.as_ref(), Some(&prepared.attempt));
    assert_eq!(returned.id, case.turn.id);
    assert_eq!(returned.session_id, case.session.session_id);
    assert_eq!(returned, case.persisted_turn().await);
    let after = case.snapshot();
    assert_eq!(after.effects, before.effects);
    assert_eq!(after.registrations, before.registrations);
    assert_eq!(after.captures, before.captures);
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.events.len(), before.events.len() + 1);
    guard
        .revalidate(
            &case.runtime().identity(),
            prepared.profile.binding(),
            &case.turn.id,
            case.turn.ordinal,
            NativeProfileUsePurpose::Execution,
        )
        .unwrap();
    assert_eq!(guard.profile_path().unwrap(), case.home);
    assert!(matches!(
        custody.acquire_configured_rcli_use(
            &case.runtime().identity(),
            prepared.profile.binding(),
            &proof.profile_ref,
            &case.turn.id,
            case.turn.ordinal,
        ),
        Err(error) if error.0 == "native_profile_in_use"
    ));
    drop(guard);
    let released = custody
        .acquire_configured_rcli_use(
            &case.runtime().identity(),
            prepared.profile.binding(),
            &proof.profile_ref,
            &case.turn.id,
            case.turn.ordinal,
        )
        .unwrap();
    released.revalidate_retained().unwrap();
    drop(released);
    drop(custody);
    drop(prepared);
    case.cancel().await;
    case.finish().await;
}

#[tokio::test]
async fn rcli_turn_after_a_first_turn_that_never_ran_uses_the_initial_admission() {
    let case = Fixture::new(MasterBackend::Rcli, true).await;
    case.register().await;
    case.cancel().await;
    let revision = Fixture::read_owner(&case.owner, OwnerRead::Capabilities)
        .await
        .revision
        .value();
    let second = request(
        "routing-second-turn",
        revision,
        OwnerOperation::Apply {
            mutation: OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Submit {
                    project_id: case.session.project_id.clone(),
                    session_id: case.session.session_id.clone(),
                    text: "Describe this project again without using tools.".into(),
                },
            },
        },
    );
    applied(&Fixture::commit_owner(&case.service, &case.owner, &second).await);
    let result = case
        .command(MasterRuntimeCommand::Prepare {
            host: case.runtime().identity(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: second.request_id.clone(),
        })
        .await
        .unwrap();
    match result {
        MasterRuntimeResult::Prepared(prepared) => {
            assert_eq!(
                prepared.session.current_turn.as_ref(),
                Some(&second.request_id)
            );
            assert!(prepared.attempt.request.previous_attempt.is_none());
            assert_eq!(
                prepared.attempt.connection.account_ref,
                reference("local-account")
            );
        }
        MasterRuntimeResult::Turn(turn) => {
            panic!(
                "second RCLI turn was not admitted: {:?} {:?}",
                turn.phase, turn.reason
            )
        }
        _ => panic!("actual RCLI preparation returned an unexpected result"),
    }
    case.finish().await;
}
