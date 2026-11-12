use super::*;
use crate::engine::NativeProfileCustody;
use crate::engine::bootstrap::EngineService;
use crate::engine::owner::{
    HostStamp, ModelAccessCollection, NativeAuthAction, NativeAuthFact, NativeAuthLogout,
    NativeAuthMethod, NativeAuthPhase, NativeAuthScope, NativeAuthStart, OwnerClient,
    OwnerClientKind, OwnerData, OwnerMutation, OwnerOperation, OwnerRead, OwnerReadReply,
    OwnerReply, OwnerRequest, OwnerResponse, OwnerResult,
};
use crate::master_session::{
    MasterAccountState, MasterBootstrapAuth, MasterOwnerAction, MasterSelection, MasterSessionView,
    RcliConfiguredProfileProof,
};
use crate::master_step_limit::{MasterStepSettings, StepLimit};
use crate::master_time_limit::{MasterTimeSettings, TimeLimit};
use crate::protocol::{ConfigId, CredentialRef, Cursor, PROTOCOL_VERSION, ProjectSpec};
use crate::routing::{ResourceBudget, RouteRequest};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params, types::Value};
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
const POOL_CATALOG: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = ["claude-a", "claude-b", "codex-c"]
[routing]
approved_accounts = ["claude-account-a", "claude-account-b", "codex-account-c"]
global_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
project_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
task_capacity = { slots=1, cpu_millis=1000, memory_mib=512, processes=4, output_bytes=1048576, wall_time_ms=60000, spend_microunits=0 }
[model_access]
accounts = ["claude-account-a", "claude-account-b", "codex-account-c"]
[[model_access.connections]]
id = "claude-a"
provider = "anthropic"
harness = "claude"
protocol = "native"
account_ref = "claude-account-a"
endpoint = { kind = "native" }
model = { id = "configured-claude-model", author = "anthropic" }
auth = { method = "native_managed", profile_ref = "claude-profile-a" }
[[model_access.connections]]
id = "claude-b"
provider = "anthropic"
harness = "claude"
protocol = "native"
account_ref = "claude-account-b"
endpoint = { kind = "native" }
model = { id = "configured-claude-model", author = "anthropic" }
auth = { method = "native_managed", profile_ref = "claude-profile-b" }
[[model_access.connections]]
id = "codex-c"
provider = "openai"
harness = "codex"
protocol = "native"
account_ref = "codex-account-c"
endpoint = { kind = "native" }
model = { id = "configured-codex-model", author = "openai" }
auth = { method = "native_managed", profile_ref = "codex-profile-c" }
[[model_access.pools]]
id = "shared-pool"
members = ["claude-a", "claude-b", "codex-c"]
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

fn pool_selection() -> MasterSelection {
    MasterSelection {
        backend: MasterBackend::Claude,
        pool_ref: reference("shared-pool"),
        target_profile_ref: reference("claude-a"),
        route: RouteRequest {
            preferred_target: Some(reference("claude-a")),
            required_target: Some(reference("claude-a")),
            required_capabilities: vec![],
            account_ref: reference("claude-account-a"),
            budget_ref: reference("conversation-budget"),
            resources: ResourceBudget::unit(),
        },
    }
}

fn account(
    account: &str,
    connection: &str,
    harness: &str,
    reason: Option<MasterReason>,
) -> MasterAccountState {
    MasterAccountState {
        account_ref: reference(account),
        connection: reference(connection),
        harness: reference(harness),
        reason,
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

/// An exclusive lock on a native profile's manifest, as an account change holds it. Holds
/// the open manifest. Dropping it unlocks the open file description, as the engine's own
/// locks do, so a child another thread is starting, which briefly shares the description,
/// cannot keep the profile held after the hold ends.
struct ProfileHold(fs::File);

impl Drop for ProfileHold {
    /// Unlocks the manifest for every descriptor that shares its open file description.
    /// Input: the hold being dropped. Output: none.
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.0, rustix::fs::FlockOperation::Unlock);
    }
}

fn exclusive_profile_hold(root: &Path, profile_ref: &CredentialRef) -> Option<ProfileHold> {
    let manifest = root
        .join("native-profiles")
        .join(format!(
            "{:x}",
            <sha2::Sha256 as sha2::Digest>::digest(profile_ref.as_str().as_bytes())
        ))
        .join("identity.json");
    let file = fs::File::open(manifest).unwrap();
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .ok()
        .map(|()| ProfileHold(file))
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
    config_id: ConfigId,
    source_hash: String,
    _stop: watch::Sender<bool>,
}

impl Fixture {
    async fn new(backend: MasterBackend, with_repository: bool) -> Self {
        Self::open(CATALOG, selection(backend), with_repository).await
    }

    async fn open(catalog: &str, selection: MasterSelection, with_repository: bool) -> Self {
        let backend = selection.backend;
        let root = tempfile::Builder::new()
            .prefix("ca-mrt-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        private_file(&global, catalog.as_bytes());
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
        let config_id = basis.config_id.clone();
        let basis_hash = source_hash.clone();
        let open = request(
            "routing-open",
            basis.revision.value(),
            OwnerOperation::Apply {
                mutation: OwnerMutation::ProjectMaster {
                    action: MasterOwnerAction::Open {
                        project_id: project.clone(),
                        expected_config_id: basis.config_id,
                        expected_source_hash: source_hash,
                        selection: Box::new(selection),
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
            config_id,
            source_hash: basis_hash,
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
        let mut turns = self.turns().await;
        assert_eq!(turns.len(), 1);
        turns.remove(0)
    }

    async fn turns(&self) -> Vec<MasterTurnView> {
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
        let OwnerData::ProjectMasterTurns { page } = state.data else {
            panic!("actual persisted turn page missing");
        };
        page.turns
    }

    async fn sign_in(&self, connection: &str, authenticated: bool) {
        let method = if connection.starts_with("codex") {
            NativeAuthMethod::CodexBrowser
        } else {
            NativeAuthMethod::ClaudeOfficial
        };
        let reply = self
            .owner
            .start_native_auth(NativeAuthStart {
                request_id: RequestId::new(format!("routing-sign-in-{connection}")).unwrap(),
                scope: self.auth_scope(connection),
                method,
            })
            .await
            .unwrap();
        assert_eq!(reply.fact.phase, NativeAuthPhase::Unavailable);
        assert!(reply.account.is_none());
        if !authenticated {
            return;
        }
        let observed = NativeAuthFact {
            phase: NativeAuthPhase::AuthenticatedObserved,
            code: None,
            ..reply.fact
        };
        let db = Connection::open_with_flags(
            self.root.path().join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(1)).unwrap();
        let changed = db
            .execute(
                "UPDATE native_auth_operations SET fact=?1,phase='authenticated_observed'
                 WHERE id=?2 AND generation=?3",
                params![
                    crate::engine::owner::native_auth::encode_fact(&observed).unwrap(),
                    observed.operation_id.as_str(),
                    i64::try_from(observed.generation).unwrap()
                ],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    fn auth_scope(&self, connection: &str) -> NativeAuthScope {
        NativeAuthScope {
            expected_database_instance: self.runtime().identity().db_instance,
            connection: reference(connection),
            expected_config_id: self.config_id.clone(),
            expected_source_hash: self.source_hash.clone(),
        }
    }

    async fn sign_out(&self, connection: &str) {
        let reply = self
            .owner
            .logout_native_auth(NativeAuthLogout {
                request_id: RequestId::new(format!("routing-sign-out-{connection}")).unwrap(),
                scope: self.auth_scope(connection),
            })
            .await
            .unwrap();
        assert_eq!(reply.fact.action, NativeAuthAction::Logout);
        assert_ne!(reply.fact.phase, NativeAuthPhase::AuthenticatedObserved);
    }

    /// Moves a connection's latest sign-in to an earlier engine run, the state an engine
    /// restart leaves it in. Input: the connection. Output: none; its fact names another run.
    fn move_to_earlier_run(&self, connection: &str) {
        let db = Connection::open_with_flags(
            self.root.path().join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(1)).unwrap();
        let mut statement = db
            .prepare("SELECT fact FROM native_auth_operations ORDER BY generation DESC")
            .unwrap();
        let mut fact = statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(|bytes| crate::engine::owner::native_auth::decode_fact(&bytes.unwrap()).unwrap())
            .find(|fact| fact.binding.connection.id == reference(connection))
            .unwrap();
        drop(statement);
        let earlier = "b".repeat(64);
        assert_ne!(fact.binding.engine_epoch, earlier);
        fact.binding.engine_epoch = earlier;
        let changed = db
            .execute(
                "UPDATE native_auth_operations SET fact=?1 WHERE id=?2 AND generation=?3",
                params![
                    crate::engine::owner::native_auth::encode_fact(&fact).unwrap(),
                    fact.operation_id.as_str(),
                    i64::try_from(fact.generation).unwrap()
                ],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    /// Asks the engine to check a connection's existing sign-in. With no account-check host
    /// registered the check cannot run, as when the host is busy at an engine restart.
    /// Input: the connection. Output: none; the check ends unavailable.
    async fn check(&self, connection: &str) {
        let reply = self
            .owner
            .start_native_auth(NativeAuthStart {
                request_id: RequestId::new(format!("routing-check-{connection}")).unwrap(),
                scope: self.auth_scope(connection),
                method: NativeAuthMethod::ClaudeExisting,
            })
            .await
            .unwrap();
        assert_eq!(reply.fact.phase, NativeAuthPhase::Unavailable);
        assert_eq!(
            reply.fact.code.as_deref(),
            Some("native_auth_host_unavailable")
        );
    }

    /// Lists the sign-ins the engine re-proves before this conversation's next turn.
    /// Input: the fixture. Output: their connections, sorted.
    async fn stale(&self) -> Vec<CredentialRef> {
        let runtime = self.runtime();
        let session = Some(self.session.session_id.clone());
        let mut stale: Vec<_> = runtime
            .engine
            .native_auth_job(|reply| {
                crate::storage::Job::NativeAuthStale(runtime.identity(), session, reply)
            })
            .await
            .unwrap()
            .into_iter()
            .map(|start| start.scope.connection)
            .collect();
        stale.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        stale
    }

    /// Commits many files and one large file to the repository in one pack, as a clone
    /// stores them, so a conversation worktree takes a while to check out without the
    /// fixture writing a loose object per file. Input: the file count and the large file's
    /// bytes. Output: none; the repository head has them.
    fn grow(&self, files: usize, large: usize) {
        private_directory(&self.repository.join("bulk"));
        for index in 0..files {
            private_file(
                &self.repository.join(format!("bulk/{index:05}.txt")),
                format!("Actual fixture file {index}.\n").as_bytes(),
            );
        }
        private_file(&self.repository.join("large.bin"), &vec![7; large]);
        git(
            &self.repository,
            self.root.path(),
            &["-c", "core.bigFileThreshold=1", "add", "-A"],
        );
        git(
            &self.repository,
            self.root.path(),
            &["commit", "-qm", "Grow master routing fixture"],
        );
    }

    /// Waits until the conversation's worktree lease is reserved and not yet checked out.
    /// Input: the fixture. Output: none; panics when the checkout is never seen running.
    async fn checkout_started(&self) {
        let db = self.db();
        let deadline = Instant::now() + TEST_BOUND;
        loop {
            let state: Option<String> = db
                .query_row(
                    "SELECT json_extract(body,'$.lease.state') FROM worktree_leases WHERE task=?1",
                    [self.session.bootstrap_task_id.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();
            if state.as_deref() == Some("Reserved") {
                return;
            }
            assert!(
                state.is_none() && Instant::now() < deadline,
                "actual checkout was not seen running: {state:?}"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// Holds the engine database write lock from another connection, as an agent register
    /// commit does under load, longer than storage waits for it. Input: how long. Output:
    /// the thread that commits and releases the lock.
    fn hold_write_lock(&self, held: Duration) -> std::thread::JoinHandle<()> {
        let db = Connection::open_with_flags(
            self.root.path().join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(held);
            db.execute_batch("COMMIT").unwrap();
        })
    }

    async fn submit(&self, text: &str) -> RequestId {
        let revision = Self::read_owner(&self.owner, OwnerRead::Capabilities)
            .await
            .revision
            .value();
        let submitted = request(
            &format!("routing-turn-{revision}"),
            revision,
            OwnerOperation::Apply {
                mutation: OwnerMutation::ProjectMaster {
                    action: MasterOwnerAction::Submit {
                        project_id: self.session.project_id.clone(),
                        session_id: self.session.session_id.clone(),
                        text: text.into(),
                    },
                },
            },
        );
        applied(&Self::commit_owner(&self.service, &self.owner, &submitted).await);
        submitted.request_id
    }

    async fn prepared(&self, turn: &RequestId) -> Box<PreparedMasterTurn> {
        let result = self
            .command(MasterRuntimeCommand::Prepare {
                host: self.runtime().identity(),
                project_id: self.session.project_id.clone(),
                session_id: self.session.session_id.clone(),
                turn_id: turn.clone(),
            })
            .await
            .unwrap();
        match result {
            MasterRuntimeResult::Prepared(prepared) => prepared,
            MasterRuntimeResult::Turn(turn) => {
                panic!("turn was not prepared: {:?} {:?}", turn.phase, turn.reason)
            }
            _ => panic!("actual preparation returned an unexpected result"),
        }
    }

    async fn fail(&self, turn: &RequestId, attempt: &RequestId, reason: MasterReason) {
        let result = self
            .command(MasterRuntimeCommand::Observe {
                host: self.runtime().identity(),
                project_id: self.session.project_id.clone(),
                session_id: self.session.session_id.clone(),
                turn_id: turn.clone(),
                attempt_id: attempt.clone(),
                observation: MasterRuntimeObservation::Failed {
                    reason,
                    cause: "routing_test_failure".into(),
                },
            })
            .await
            .unwrap();
        let MasterRuntimeResult::Turn(failed) = result else {
            panic!("failed turn did not settle");
        };
        assert_eq!(failed.phase, MasterTurnPhase::Failed);
        assert_eq!(failed.reason, Some(reason));
    }

    fn paused_until(&self, connection: &str) -> Option<i64> {
        self.db()
            .query_row(
                "SELECT until_ms FROM team_paused_accounts WHERE connection=?1",
                [connection],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
    }

    fn reject_sign_in(&self, connection: &str, age: Duration) {
        let config = crate::config::resolve_config(
            &self.root.path().join("global.toml"),
            &self.root.path().join("project.toml"),
        )
        .unwrap();
        let model = config
            .settings
            .model_access
            .as_ref()
            .unwrap()
            .connections
            .iter()
            .find(|item| item.id == reference(connection))
            .unwrap()
            .clone();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let observed_at_ms = u64::try_from(now.saturating_sub(age).as_millis()).unwrap();
        let mut db = Connection::open_with_flags(
            self.root.path().join("engine.db"),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(1)).unwrap();
        let tx = db.transaction().unwrap();
        crate::model_gateway::observe(
            &tx,
            &config,
            &crate::model_gateway::Observation {
                id: RequestId::new(format!("routing-rejected-{connection}")).unwrap(),
                connection: model,
                sequence: 1,
                observed_at_ms,
                valid_until_ms: observed_at_ms + 60_000,
                kind: crate::model_gateway::ObservationKind::InvalidAuthentication,
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    fn rejected(&self, connection: &str) -> bool {
        let config = crate::config::resolve_config(
            &self.root.path().join("global.toml"),
            &self.root.path().join("project.toml"),
        )
        .unwrap();
        let model = config
            .settings
            .model_access
            .unwrap()
            .connections
            .into_iter()
            .find(|item| item.id == reference(connection))
            .unwrap();
        crate::model_gateway::sign_in_rejected(&self.db(), &model).unwrap()
    }

    fn gateway_state(
        &self,
        connection: &str,
        now_ms: u64,
    ) -> crate::model_gateway::ConnectionState {
        let config = crate::config::resolve_config(
            &self.root.path().join("global.toml"),
            &self.root.path().join("project.toml"),
        )
        .unwrap();
        let model = config
            .settings
            .model_access
            .unwrap()
            .connections
            .into_iter()
            .find(|item| item.id == reference(connection))
            .unwrap();
        crate::model_gateway::connection_state(&self.db(), &model, now_ms).unwrap()
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
async fn a_turn_that_cannot_start_ends_blocked_with_its_reason_in_the_read() {
    let case = Fixture::new(MasterBackend::Rcli, false).await;
    case.register().await;
    let returned = case.drive().await;
    assert_eq!(
        returned
            .expect_err("a project without a repository cannot start a turn")
            .0,
        "master_repository_missing"
    );
    let failed = case.persisted_turn().await;
    assert_eq!(failed.id, case.turn.id);
    unallocated(
        &failed,
        MasterTurnPhase::Blocked,
        Some(MasterReason::RepositoryMissing),
    );
    let session = Fixture::read_session(&case.owner, &case.session.project_id).await;
    assert_eq!(session.current_turn, None);
    let before = case.snapshot();
    assert_eq!(case.drive().await.unwrap(), failed);
    assert_eq!(case.snapshot(), before);
    case.finish().await;
}

#[tokio::test]
async fn a_turn_whose_write_waits_on_another_engine_connection_still_ends_with_its_own_reason() {
    let case = Fixture::new(MasterBackend::Rcli, false).await;
    case.register().await;
    let holder = case.hold_write_lock(Duration::from_millis(600));
    let returned = case.drive().await;
    holder.join().unwrap();
    assert_eq!(
        returned
            .expect_err("a project without a repository cannot start a turn")
            .0,
        "master_repository_missing"
    );
    unallocated(
        &case.persisted_turn().await,
        MasterTurnPhase::Blocked,
        Some(MasterReason::RepositoryMissing),
    );
    case.finish().await;
}

/// How long the preparation row keeps another connection's write lock after storage starts
/// the preparation: past the first write of a preparation that reads first, under the
/// 250 ms storage busy wait.
const SHORT_LOCK: Duration = Duration::from_millis(150);

/// A preparation that meets another engine connection's write lock, as the research loop's
/// stale-job update or a team runtime commit holds it, waits the lock out and prepares the
/// turn (BL-DBLOCK). Preparation reads before it writes, and SQLite refuses a read
/// transaction's first write at once, with no wait, while another connection holds the
/// lock. Storage is held until the preparation is queued behind the taken lock, so the lock
/// spans the start of the preparation on a cold run or a slow machine too.
#[tokio::test]
async fn a_preparation_that_meets_a_short_write_lock_waits_it_out_and_prepares_the_turn() {
    let case = Fixture::open(POOL_CATALOG, codex_pool_selection(), true).await;
    case.sign_in("codex-c", true).await;
    let (host, project, turn) = (
        case.runtime().identity(),
        case.session.project_id.clone(),
        case.turn.id.clone(),
    );
    let routed = case
        .command(case.route(host.clone(), project.clone(), turn.clone()))
        .await;
    assert!(
        matches!(routed, Ok(MasterRuntimeResult::Turn(_))),
        "a route reads the turn first, so the preparation's reads run warm"
    );
    let lock = Connection::open_with_flags(
        case.root.path().join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let storage = case.runtime().engine.storage.sender.clone();
    let (release, held) = tokio::sync::oneshot::channel();
    storage.try_send(crate::storage::Job::Hold(held)).unwrap();
    let deadline = Instant::now() + TEST_BOUND;
    while storage.capacity() < storage.max_capacity() {
        assert!(Instant::now() < deadline, "storage never took the hold");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let preparing = async {
        let prepared = case
            .command(MasterRuntimeCommand::Prepare {
                host,
                project_id: project,
                session_id: case.session.session_id.clone(),
                turn_id: turn,
            })
            .await;
        (prepared, Instant::now())
    };
    let releasing = async {
        while storage.capacity() == storage.max_capacity() {
            assert!(
                Instant::now() < deadline,
                "the preparation was never queued"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        release.send(()).unwrap();
        let released = Instant::now();
        tokio::time::sleep(SHORT_LOCK).await;
        lock.execute_batch("COMMIT").unwrap();
        released
    };
    let ((prepared, ended), released) = tokio::join!(preparing, releasing);
    let prepared = match prepared {
        Ok(MasterRuntimeResult::Prepared(prepared)) => prepared,
        Ok(_) => panic!("the preparation returned no prepared turn"),
        Err(error) => panic!("the turn was not prepared: {}", error.0),
    };
    assert!(
        ended.duration_since(released) >= SHORT_LOCK,
        "the preparation started while the other connection held the lock and waited it out"
    );
    assert_eq!(prepared.session.session_id, case.session.session_id);
    assert_eq!(
        case.persisted_turn().await.phase,
        MasterTurnPhase::Prepared,
        "the prepared turn is stored"
    );
    drop(prepared);
    case.finish().await;
}

#[tokio::test]
async fn an_unregistered_rcli_profile_stops_the_turn_with_its_own_cause() {
    let case = Fixture::new(MasterBackend::Rcli, true).await;
    let returned = case
        .drive()
        .await
        .expect("an RCLI master without a registered key blocks its turn as every master does");
    let stopped = case.persisted_turn().await;
    assert_eq!(returned.phase, MasterTurnPhase::Blocked);
    assert_eq!(stopped.id, case.turn.id);
    assert_eq!(stopped.phase, MasterTurnPhase::Blocked);
    assert_eq!(stopped.reason, Some(MasterReason::RuntimeUnavailable));
    assert_eq!(
        stopped.cause.as_deref(),
        Some("rcli_profile_configuration_required")
    );
    assert_eq!(stopped.detail, None);
    let session = Fixture::read_session(&case.owner, &case.session.project_id).await;
    assert_eq!(session.current_turn, None);
    case.finish().await;
}

#[tokio::test]
async fn an_unclassified_cause_is_kept_as_a_code_and_private_text_is_redacted() {
    for (cause, stored) in [
        ("master_runtime_binding", "master_runtime_binding"),
        (
            "Provider said: token sk-live-123 rejected at /private/tmp/x",
            "master_cause_redacted",
        ),
    ] {
        let case = Fixture::new(MasterBackend::Rcli, true).await;
        let result = case
            .command(MasterRuntimeCommand::Fail {
                host: case.runtime().identity(),
                project_id: case.session.project_id.clone(),
                session_id: case.session.session_id.clone(),
                turn_id: case.turn.id.clone(),
                reason: reason_for_error(&EngineError(cause.into())),
                cause: cause.into(),
            })
            .await
            .unwrap();
        let MasterRuntimeResult::Turn(returned) = result else {
            panic!("a failed dispatch returns the settled turn");
        };
        let stopped = case.persisted_turn().await;
        assert_eq!(*returned, stopped);
        assert_eq!(stopped.phase, MasterTurnPhase::Blocked, "{cause}");
        assert_eq!(stopped.reason, Some(MasterReason::Unknown), "{cause}");
        assert_eq!(stopped.cause.as_deref(), Some(stored), "{cause}");
        case.finish().await;
    }
}

/// A running master turn reads the agent policy the owner saved now through the storage
/// actor, with the same scope check as its inspect; a toolchain turned off stops it as a
/// settings change, and an unreadable policy reads as none.
#[tokio::test]
async fn a_running_turn_reads_the_saved_agent_policy_and_an_unreadable_one_as_none() {
    use crate::sandbox::policy::{
        AgentAccess, AgentPolicy, Narrowing, ToolchainChoice, narrowing, save,
    };
    use crate::sandbox::toolchains::Toolchain;
    let case = Fixture::new(MasterBackend::Rcli, true).await;
    let read = || {
        case.command(MasterRuntimeCommand::Policy {
            host: case.runtime().identity(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
        })
    };
    let saved = |result: MasterRuntimeResult| match result {
        MasterRuntimeResult::Policy(policy) => *policy,
        _ => panic!("the policy read returns the saved policy"),
    };
    assert_eq!(saved(read().await.unwrap()), Some(AgentPolicy::default()));
    let off = AgentPolicy {
        access: AgentAccess::Sandboxed,
        toolchains: vec![ToolchainChoice {
            toolchain: Toolchain::Rust,
            allowed: false,
        }],
        ide_git_hooks: false,
        tool_cache: Default::default(),
        ide_agent_files: Default::default(),
        network: crate::sandbox::policy::AgentNetwork::default(),
    };
    let writer = Connection::open(case.root.path().join("engine.db")).unwrap();
    writer.busy_timeout(Duration::from_secs(5)).unwrap();
    save(&writer, case.session.project_id.as_str(), &off).unwrap();
    let read_off = saved(read().await.unwrap());
    assert_eq!(read_off, Some(off));
    let stopped = narrowing(
        read_off.as_ref(),
        None,
        Some(AgentAccess::Sandboxed),
        None,
        &[Toolchain::Rust],
    )
    .map(Narrowing::code)
    .unwrap();
    assert_eq!(stopped, "agent_toolchain_lowered");
    assert_eq!(
        reason_for_error(&EngineError(stopped.into())),
        MasterReason::ConnectionChanged
    );
    writer
        .execute(
            "UPDATE project_agent_policy SET body='{' WHERE project=?1",
            [case.session.project_id.as_str()],
        )
        .unwrap();
    assert_eq!(saved(read().await.unwrap()), None);
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
    let account_change = exclusive_profile_hold(case.root.path(), &proof.profile_ref)
        .expect("no agent holds the configured profile yet");
    let before = case.snapshot();
    assert_eq!(case.persisted_turn().await.phase, MasterTurnPhase::Prepared);
    assert_eq!(before.effects.each_ref().map(Vec::len), [1, 1, 0, 0, 1, 1]);
    assert!(before.captures.is_empty());
    let returned = case.drive().await;
    assert_eq!(
        returned.expect_err("an account change holds the profile").0,
        "native_profile_in_use"
    );
    let failed = case.persisted_turn().await;
    assert_eq!(failed.phase, MasterTurnPhase::Failed);
    assert_eq!(failed.reason, Some(MasterReason::ProfileBusy));
    assert_eq!(failed.cause.as_deref(), Some("native_profile_in_use"));
    assert!(failed.runtime.is_none());
    assert!(failed.reply.is_none());
    assert_eq!(failed.id, case.turn.id);
    assert_eq!(failed.session_id, case.session.session_id);
    let attempt = failed.attempt.as_ref().expect("prepared attempt kept");
    assert_eq!(attempt.request, prepared.attempt.request);
    assert!(attempt.completion.is_some());
    let after = case.snapshot();
    assert_eq!(
        after.effects.each_ref().map(Vec::len),
        before.effects.each_ref().map(Vec::len)
    );
    assert_eq!(after.registrations, before.registrations);
    assert_eq!(after.captures, before.captures);
    assert_eq!(after.revision, before.revision + 2);
    assert_eq!(after.events.len(), before.events.len() + 2);
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
    drop(account_change);
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
    assert_eq!(released.profile_path().unwrap(), case.home);
    drop(released);
    drop(custody);
    drop(prepared);
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

/// An rcli master turn is prepared with the saved master time limit, as a Claude or Codex
/// one is (OB-5d): one setting for every master harness. With No limit it names no run
/// time, so its host gets no clock; with 30 min it names 30 minutes.
#[tokio::test]
async fn an_rcli_master_turn_is_prepared_with_the_saved_time_limit() {
    for limit in [TimeLimit::NoLimit, TimeLimit::Minutes30] {
        let case = Fixture::new(MasterBackend::Rcli, true).await;
        case.register().await;
        store_time_limit(&case, limit);
        let prepared = case.prepared(&case.turn.id).await;
        assert_eq!(prepared.session.selection.backend, MasterBackend::Rcli);
        assert_eq!(
            prepared.time, limit,
            "the turn carries the saved time limit"
        );
        assert_eq!(prepared.time.run_time(), limit.run_time());
        assert_eq!(
            prepared.time.run_time().is_none(),
            limit == TimeLimit::NoLimit
        );
        drop(prepared);
        case.finish().await;
    }
}

#[tokio::test]
async fn a_repository_vs_code_and_the_codex_app_wrote_to_gets_a_master_worktree_lease() {
    let case = Fixture::new(MasterBackend::Rcli, true).await;
    let mut tools = String::from(concat!(
        "[remote \"origin\"]\n",
        "\turl = https://example.invalid/owner/project.git\n",
        "\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        "[branch \"trunk\"]\n",
        "\tremote = origin\n",
        "\tmerge = refs/heads/trunk\n",
    ));
    for index in 0..50 {
        tools.push_str(&format!(
            "[branch \"feature/{index}\"]\n\tvscode-merge-base = origin/trunk\n"
        ));
    }
    let config = case.repository.join(".git/config");
    let mut written = fs::read(&config).unwrap();
    written.extend_from_slice(tools.as_bytes());
    fs::write(&config, written).unwrap();
    git(
        &case.repository,
        case.root.path(),
        &["config", "extensions.worktreeConfig", "true"],
    );
    let linked = case.root.path().join("linked");
    git(
        &case.repository,
        case.root.path(),
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
        ],
    );
    git(
        &linked,
        case.root.path(),
        &[
            "config",
            "--worktree",
            "codex.localEnvironmentConfigPath",
            "__none__",
        ],
    );
    assert!(
        case.repository
            .join(".git/worktrees/linked/config.worktree")
            .is_file()
    );
    case.register().await;
    let result = case
        .command(MasterRuntimeCommand::Prepare {
            host: case.runtime().identity(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
        })
        .await;
    let prepared = match result {
        Ok(MasterRuntimeResult::Prepared(prepared)) => prepared,
        Ok(_) => panic!("the master turn was not prepared"),
        Err(error) => panic!("the master refused the repository: {}", error.0),
    };
    assert_eq!(prepared.repository, case.repository);
    assert_eq!(prepared.cwd, PathBuf::from(&prepared.lease.path));
    assert_eq!(
        fs::read(prepared.cwd.join("README")).unwrap(),
        b"Actual master routing fixture.\n"
    );
    case.finish().await;
}

#[tokio::test]
async fn the_owner_reads_while_the_first_send_checks_out_the_conversation_worktree() {
    let case = Fixture::new(MasterBackend::Rcli, true).await;
    case.grow(500, 16 << 20);
    case.register().await;
    let prepare = async {
        let result = case
            .command(MasterRuntimeCommand::Prepare {
                host: case.runtime().identity(),
                project_id: case.session.project_id.clone(),
                session_id: case.session.session_id.clone(),
                turn_id: case.turn.id.clone(),
            })
            .await;
        (result, Instant::now())
    };
    let read = async {
        case.checkout_started().await;
        Fixture::read_owner(
            &case.owner,
            OwnerRead::ProjectMaster {
                project_id: case.session.project_id.clone(),
            },
        )
        .await;
        Instant::now()
    };
    let ((result, prepared_at), read_at) = tokio::join!(prepare, read);
    let prepared = match result {
        Ok(MasterRuntimeResult::Prepared(prepared)) => prepared,
        Ok(_) => panic!("the master turn was not prepared"),
        Err(error) => panic!("the master refused the repository: {}", error.0),
    };
    assert_eq!(
        fs::read(prepared.cwd.join("bulk/00499.txt")).unwrap(),
        b"Actual fixture file 499.\n"
    );
    assert!(
        read_at < prepared_at,
        "the owner read was answered {:?} after the worktree checkout",
        read_at - prepared_at
    );
    case.finish().await;
}

#[tokio::test]
async fn a_repository_whose_config_runs_a_command_is_refused_with_the_key_and_no_fence() {
    for (key, value, code, named) in [
        (
            "core.hooksPath",
            "/private/tmp",
            "core_hookspath",
            "core.hooksPath",
        ),
        (
            "filter.lfs.process",
            "git-lfs filter-process",
            "filter_driver",
            "filter.<driver>",
        ),
    ] {
        let case = Fixture::new(MasterBackend::Rcli, true).await;
        git(&case.repository, case.root.path(), &["config", key, value]);
        let proof = case.register().await;
        let returned = case.drive().await;
        assert_eq!(
            returned
                .expect_err("a repository whose config runs a command must be refused")
                .0,
            format!("repository_config_refused:{code}"),
            "{key}"
        );
        let failed = case.persisted_turn().await;
        assert_eq!(failed.phase, MasterTurnPhase::Blocked, "{key}");
        assert_eq!(
            failed.reason,
            Some(MasterReason::RepositoryConfigRefused),
            "{key}"
        );
        assert_eq!(
            failed.cause.as_deref(),
            Some(format!("repository_config_refused:{code}").as_str()),
            "{key}"
        );
        let detail = failed
            .detail
            .as_deref()
            .expect("the refusal names the repository, the key and why");
        assert!(
            detail.starts_with(&format!(
                "{} has the Git setting {named}, which ",
                case.repository.display()
            )),
            "{detail}"
        );
        let after = case.snapshot();
        after.assert_no_effects();
        {
            let mut db = case.db();
            let tx = db.transaction().unwrap();
            assert!(rows(&tx, "native_auth_fences").is_empty(), "{key}");
        }
        assert!(
            exclusive_profile_hold(case.root.path(), &proof.profile_ref).is_some(),
            "the refused turn left the profile held: {key}"
        );
        let session = Fixture::read_session(&case.owner, &case.session.project_id).await;
        assert_eq!(session.current_turn, None, "{key}");
        case.finish().await;
    }
}

#[tokio::test]
async fn a_master_turn_runs_on_the_next_account_when_the_first_is_not_signed_in() {
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-b", true).await;
    let prepared = case.prepared(&case.turn.id).await;
    assert_eq!(prepared.attempt.connection.id, reference("claude-b"));
    assert_eq!(
        prepared.attempt.connection.account_ref,
        reference("claude-account-b")
    );
    assert_eq!(
        prepared.profile.binding().connection.id,
        reference("claude-b")
    );
    let persisted = case.persisted_turn().await;
    assert_eq!(persisted.phase, MasterTurnPhase::Prepared);
    assert_eq!(
        persisted
            .attempt
            .as_ref()
            .map(|attempt| &attempt.connection.account_ref),
        Some(&reference("claude-account-b"))
    );
    assert!(persisted.accounts.is_empty());
    let session = Fixture::read_session(&case.owner, &case.session.project_id).await;
    assert_eq!(session.selection, pool_selection());
    assert_eq!(case.paused_until("claude-a"), None);
    drop(prepared);
    case.finish().await;
}

#[tokio::test]
async fn a_rate_limited_account_hands_the_next_turn_and_the_conversation_to_the_next_account() {
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    case.sign_in("claude-b", true).await;
    let first = case.prepared(&case.turn.id).await;
    assert_eq!(first.attempt.connection.id, reference("claude-a"));
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let failed = case
        .command(MasterRuntimeCommand::Observe {
            host: case.runtime().identity(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
            attempt_id: first.attempt.request.id.clone(),
            observation: MasterRuntimeObservation::Failed {
                reason: MasterReason::RateLimited,
                cause: "rate_limited".into(),
            },
        })
        .await
        .unwrap();
    let MasterRuntimeResult::Turn(failed) = failed else {
        panic!("rate-limited turn did not settle");
    };
    assert_eq!(failed.phase, MasterTurnPhase::Failed);
    assert_eq!(failed.reason, Some(MasterReason::RateLimited));
    drop(first);
    assert!(case.paused_until("claude-a").unwrap() > i64::try_from(before).unwrap());
    let second = case.submit("Summarize what we discussed so far.").await;
    let next = case.prepared(&second).await;
    assert_eq!(next.attempt.connection.id, reference("claude-b"));
    assert_eq!(
        next.attempt.connection.account_ref,
        reference("claude-account-b")
    );
    assert!(
        next.text
            .contains("Describe this project in one sentence without using tools."),
        "{}",
        next.text
    );
    assert!(next.text.ends_with("Summarize what we discussed so far."));
    let turns = case.turns().await;
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].id, case.turn.id);
    assert_eq!(turns[0].phase, MasterTurnPhase::Failed);
    assert_eq!(
        turns[0]
            .attempt
            .as_ref()
            .map(|attempt| &attempt.connection.id),
        Some(&reference("claude-a"))
    );
    assert_eq!(turns[1].id, second);
    assert_eq!(turns[1].phase, MasterTurnPhase::Prepared);
    assert_eq!(
        turns[1]
            .attempt
            .as_ref()
            .map(|attempt| &attempt.connection.id),
        Some(&reference("claude-b"))
    );
    drop(next);
    case.finish().await;
}

#[tokio::test]
async fn a_rejected_sign_in_is_not_paused_and_hands_the_next_turn_to_the_next_account() {
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    case.sign_in("claude-b", true).await;
    let first = case.prepared(&case.turn.id).await;
    assert_eq!(first.attempt.connection.id, reference("claude-a"));
    case.fail(
        &case.turn.id,
        &first.attempt.request.id,
        MasterReason::InvalidAuthentication,
    )
    .await;
    drop(first);
    assert_eq!(case.paused_until("claude-a"), None);
    let second = case.submit("Summarize what we discussed so far.").await;
    let next = case.prepared(&second).await;
    assert_eq!(next.attempt.connection.id, reference("claude-b"));
    drop(next);
    case.finish().await;
}

#[tokio::test]
async fn a_turn_whose_credential_copy_fails_checks_the_account_and_hands_the_next_turn_on() {
    for code in [
        "native_profile_credential_unavailable",
        "native_profile_codex_api_key",
        "native_profile_codex_token_unreadable",
    ] {
        credential_copy_fails(code).await;
    }
}

/// A prepared turn whose credential copy fails with `code`: the turn fails as readiness
/// unknown with that cause, the account is checked again, not paused or rejected, and the
/// next turn goes to the next account. Input: the refusal code.
async fn credential_copy_fails(code: &str) {
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    case.sign_in("claude-b", true).await;
    let first = case.prepared(&case.turn.id).await;
    assert_eq!(first.attempt.connection.id, reference("claude-a"));
    let unavailable = EngineError(code.into());
    let result = case
        .command(MasterRuntimeCommand::Fail {
            host: case.runtime().identity(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
            reason: reason_for_error(&unavailable),
            cause: unavailable.0.clone(),
        })
        .await
        .unwrap();
    let MasterRuntimeResult::Turn(failed) = result else {
        panic!("the turn whose credential copy failed did not settle");
    };
    assert_eq!(failed.phase, MasterTurnPhase::Failed, "{code}");
    assert_eq!(
        failed.reason,
        Some(MasterReason::ReadinessUnknown),
        "{code}"
    );
    assert_eq!(failed.cause.as_deref(), Some(code));
    drop(first);
    assert_eq!(case.paused_until("claude-a"), None, "{code}");
    assert!(!case.rejected("claude-a"), "{code}");
    assert_eq!(case.stale().await, vec![reference("claude-a")], "{code}");
    let second = case.submit("Summarize what we discussed so far.").await;
    let next = case.prepared(&second).await;
    assert_eq!(next.attempt.connection.id, reference("claude-b"));
    drop(next);
    case.finish().await;
}

#[tokio::test]
async fn a_turn_whose_sign_in_renewal_fails_holds_the_account_briefly_and_hands_the_next_turn_on() {
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    case.sign_in("claude-b", true).await;
    let first = case.prepared(&case.turn.id).await;
    assert_eq!(first.attempt.connection.id, reference("claude-a"));
    let renewal = EngineError("native_profile_renewal_failed".into());
    let result = case
        .command(MasterRuntimeCommand::Observe {
            host: case.runtime().identity(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
            attempt_id: first.attempt.request.id.clone(),
            observation: MasterRuntimeObservation::Failed {
                reason: reason_for_error(&renewal),
                cause: renewal.0.clone(),
            },
        })
        .await
        .unwrap();
    let observed = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let MasterRuntimeResult::Turn(failed) = result else {
        panic!("the turn whose sign-in renewal failed did not settle");
    };
    assert_eq!(failed.phase, MasterTurnPhase::Failed);
    assert_eq!(failed.reason, Some(MasterReason::TransientFailure));
    assert_eq!(
        failed.cause.as_deref(),
        Some("native_profile_renewal_failed")
    );
    drop(first);
    let held = case.gateway_state("claude-a", observed);
    assert_eq!(held.health, crate::model_gateway::Health::TransientFailure);
    let retry = held
        .cooldown_until_ms
        .expect("a failed renewal holds the account until a retry time");
    assert!(
        retry > observed && retry <= observed + 60_000,
        "retry {retry} is not within a minute of {observed}"
    );
    assert_eq!(case.paused_until("claude-a"), None);
    assert!(!case.rejected("claude-a"));
    let second = case.submit("Summarize what we discussed so far.").await;
    let next = case.prepared(&second).await;
    assert_eq!(next.attempt.connection.id, reference("claude-b"));
    drop(next);
    case.finish().await;
}

#[tokio::test]
async fn a_turn_whose_harness_has_no_sign_in_leaves_the_account_unrejected() {
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    case.sign_in("claude-b", true).await;
    let first = case.prepared(&case.turn.id).await;
    assert_eq!(first.attempt.connection.id, reference("claude-a"));
    let missing = EngineError(assistant_failure("not_signed_in").into());
    let result = case
        .command(MasterRuntimeCommand::Fail {
            host: case.runtime().identity(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
            reason: reason_for_error(&missing),
            cause: missing.0.clone(),
        })
        .await
        .unwrap();
    let MasterRuntimeResult::Turn(failed) = result else {
        panic!("the turn whose harness had no sign-in did not settle");
    };
    assert_eq!(failed.phase, MasterTurnPhase::Failed);
    assert_eq!(failed.reason, Some(MasterReason::ReadinessUnknown));
    assert_eq!(failed.cause.as_deref(), Some("master_sign_in_missing"));
    drop(first);
    assert_eq!(case.paused_until("claude-a"), None);
    let second = case.submit("Summarize what we discussed so far.").await;
    let next = case.prepared(&second).await;
    assert_eq!(next.attempt.connection.id, reference("claude-a"));
    drop(next);
    case.finish().await;
}

#[tokio::test]
async fn an_account_whose_sign_in_was_rejected_reads_sign_in_again_however_long_ago() {
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    case.reject_sign_in("claude-a", Duration::from_secs(10 * 60));
    let result = case
        .command(MasterRuntimeCommand::Prepare {
            host: case.runtime().identity(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
        })
        .await
        .unwrap();
    let MasterRuntimeResult::Turn(blocked) = result else {
        panic!("a turn was prepared on an account whose sign-in was rejected");
    };
    unallocated(
        &blocked,
        MasterTurnPhase::Blocked,
        Some(MasterReason::InvalidAuthentication),
    );
    assert_eq!(blocked.cause.as_deref(), Some("master_sign_in_rejected"));
    assert_eq!(
        blocked.accounts,
        vec![
            account(
                "claude-account-a",
                "claude-a",
                "claude",
                Some(MasterReason::InvalidAuthentication)
            ),
            account(
                "claude-account-b",
                "claude-b",
                "claude",
                Some(MasterReason::ReadinessUnknown)
            ),
        ]
    );
    assert_eq!(case.paused_until("claude-a"), None);
    case.finish().await;
}

#[tokio::test]
async fn a_master_turn_no_account_can_run_names_each_account_and_why() {
    let case = Fixture::open(POOL_CATALOG, pool_selection(), false).await;
    case.sign_in("claude-b", false).await;
    case.sign_in("codex-c", true).await;
    let blocked = case.drive().await.unwrap();
    unallocated(
        &blocked,
        MasterTurnPhase::Blocked,
        Some(MasterReason::ReadinessUnknown),
    );
    assert_eq!(
        blocked.cause.as_deref(),
        Some("master_authentication_unknown")
    );
    assert_eq!(
        blocked.accounts,
        vec![
            account(
                "claude-account-a",
                "claude-a",
                "claude",
                Some(MasterReason::ReadinessUnknown)
            ),
            account(
                "claude-account-b",
                "claude-b",
                "claude",
                Some(MasterReason::ReadinessUnknown)
            ),
            account("codex-account-c", "codex-c", "codex", None),
        ]
    );
    assert_eq!(blocked, case.persisted_turn().await);
    case.finish().await;
}

#[tokio::test]
async fn a_master_turn_moves_past_an_account_that_signed_out() {
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    case.sign_out("claude-a").await;
    case.sign_in("claude-b", true).await;
    let prepared = case.prepared(&case.turn.id).await;
    assert_eq!(prepared.attempt.connection.id, reference("claude-b"));
    drop(prepared);
    case.finish().await;
}

#[tokio::test]
async fn a_restart_re_proves_every_account_of_the_conversation_pool_so_a_turn_can_move_past_the_first()
 {
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    case.sign_in("claude-b", true).await;
    case.move_to_earlier_run("claude-a");
    case.move_to_earlier_run("claude-b");
    assert_eq!(
        case.stale().await,
        vec![reference("claude-a"), reference("claude-b")]
    );
    case.finish().await;
}

#[tokio::test]
async fn a_sign_in_check_the_host_could_not_run_leaves_the_earlier_sign_in_to_be_re_proved() {
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    case.move_to_earlier_run("claude-a");
    case.check("claude-a").await;
    assert_eq!(case.stale().await, vec![reference("claude-a")]);
    case.finish().await;
}

#[tokio::test]
async fn the_conversation_so_far_keeps_the_newest_turns_that_fit_the_prompt() {
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    let mut turn = case.turn.id.clone();
    for round in 0..3 {
        let prepared = case.prepared(&turn).await;
        case.fail(
            &turn,
            &prepared.attempt.request.id,
            MasterReason::RuntimeUnavailable,
        )
        .await;
        drop(prepared);
        turn = case
            .submit(&format!("message {round} {}", "x".repeat(7000)))
            .await;
    }
    let last = case.prepared(&turn).await;
    assert!(last.text.len() <= 16384, "{}", last.text.len());
    assert!(last.text.contains("(Earlier turns were left out to fit.)"));
    assert!(last.text.contains("Owner: message 1 "));
    assert!(last.text.contains("(No reply was received.)"));
    assert!(!last.text.contains("Owner: message 0 "));
    assert!(!last.text.contains("Describe this project in one sentence"));
    assert!(
        last.text
            .ends_with(&format!("message 2 {}", "x".repeat(7000)))
    );
    assert_eq!(case.paused_until("claude-a"), None);
    drop(last);
    case.finish().await;
}

/// Stores the project's team plan with the master tools switch for the routing fixture,
/// through a second connection to its real engine database. Input: the fixture and the
/// switch. Output: none; a panic if the row cannot be written.
fn store_master_tools(case: &Fixture, master_tools: bool) {
    let setup = crate::project_setup::StoredSetup {
        project_id: case.session.project_id.clone(),
        name: "Tools".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: crate::project_setup::TeamPlan {
            master: "codex".into(),
            master_tools,
            sub_harnesses: vec![crate::project_setup::HarnessSlot {
                harness: "codex".into(),
                agents: 1,
                tools: true,
            }],
            min_agents: 1,
            max_agents: 2,
        },
        documents: vec![],
        folder: case.root.path().join("repository").display().to_string(),
        created_ms: 1,
        revision: 1,
    };
    let writer = Connection::open(case.root.path().join("engine.db")).unwrap();
    writer
        .busy_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    writer
        .execute(
            "INSERT OR REPLACE INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
}

/// The pool selection of a Codex master whose turns run on the pool's Codex account.
fn codex_pool_selection() -> MasterSelection {
    MasterSelection {
        backend: MasterBackend::Codex,
        pool_ref: reference("shared-pool"),
        target_profile_ref: reference("codex-c"),
        route: RouteRequest {
            preferred_target: Some(reference("codex-c")),
            required_target: Some(reference("codex-c")),
            required_capabilities: vec![],
            account_ref: reference("codex-account-c"),
            budget_ref: reference("conversation-budget"),
            resources: ResourceBudget::unit(),
        },
    }
}

#[tokio::test]
async fn a_codex_master_turn_is_prepared_with_the_project_s_master_tools_switch() {
    let case = Fixture::open(POOL_CATALOG, codex_pool_selection(), true).await;
    store_master_tools(&case, true);
    case.sign_in("codex-c", true).await;
    let prepared = case.prepared(&case.turn.id).await;
    assert_eq!(prepared.session.selection.backend, MasterBackend::Codex);
    assert!(prepared.tools, "the project's master tools switch is on");
    assert!(
        prepared.instructions.contains("Your tools are on"),
        "{}",
        prepared.instructions
    );
    drop(prepared);
    case.finish().await;

    let case = Fixture::open(POOL_CATALOG, codex_pool_selection(), true).await;
    store_master_tools(&case, false);
    case.sign_in("codex-c", true).await;
    let prepared = case.prepared(&case.turn.id).await;
    assert!(!prepared.tools, "the project's master tools switch is off");
    assert!(
        !prepared.instructions.contains("Your tools are on"),
        "{}",
        prepared.instructions
    );
    drop(prepared);
    case.finish().await;
}

/// Switches one of the engine's team tools off for the routing fixture's project, through
/// a second connection to its real engine database, as the owner's Tools page does. Input:
/// the fixture and the tool. Output: none; a panic if the switch cannot be stored.
fn switch_off(case: &Fixture, tool: &str) {
    let writer = Connection::open(case.root.path().join("engine.db")).unwrap();
    writer
        .busy_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    crate::coord::tool_switches::set(
        &writer,
        Some(&case.session.project_id),
        crate::coord::tool_switches::CRAZYAGENT_SERVER,
        tool,
        crate::coord::tool_switches::Switch::Off,
    )
    .unwrap();
}

#[tokio::test]
async fn a_console_master_turn_with_tools_is_told_its_memory_tools() {
    let memory_line = |name: &str| {
        let (_, line) = crate::coord::tool_switches::BUILTIN_TOOLS
            .into_iter()
            .find(|(tool, _)| *tool == name)
            .unwrap();
        format!("- {name}: {line}\n")
    };
    let remember = memory_line(crate::team::tool::REMEMBER_TOOL);
    let recall = memory_line(crate::team::tool::RECALL_TOOL);
    for (selection, account, off) in [
        (codex_pool_selection(), "codex-c", None),
        (pool_selection(), "claude-a", None),
        (
            codex_pool_selection(),
            "codex-c",
            Some(crate::team::tool::RECALL_TOOL),
        ),
    ] {
        let case = Fixture::open(POOL_CATALOG, selection, true).await;
        store_master_tools(&case, true);
        if let Some(tool) = off {
            switch_off(&case, tool);
        }
        case.sign_in(account, true).await;
        let prepared = case.prepared(&case.turn.id).await;
        let instructions = prepared.instructions.clone();
        let tools_off = prepared.tools_off.clone();
        drop(prepared);
        case.finish().await;
        assert!(
            instructions.contains("\n## Memory tools\n"),
            "{account} {off:?}: {instructions}"
        );
        assert!(instructions.contains(&remember), "{account} {off:?}");
        assert_eq!(
            instructions.contains(&recall),
            off.is_none(),
            "{account} {off:?}"
        );
        assert_eq!(
            tools_off,
            off.map(str::to_owned).into_iter().collect::<Vec<_>>(),
            "{account}"
        );
    }
}

/// A stand-in Claude Code for a console master turn that keeps the turn running until the
/// test writes memory-answered into its working folder, then replies DONE.
const MEMORY_TURN_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
i=0
while [ ! -f memory-answered ] && [ "$i" -lt 1800 ]; do /bin/sleep 0.05; i=$((i+1)); done
printf '%s\n' '{"type":"assistant","uuid":"step-1","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-1","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"DONE"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":"DONE"}'
/bin/cat > /dev/null
exit 1
"#;

/// The team server's stdin for one remember call of the given text for this project.
fn remember_input(text: &str) -> Vec<u8> {
    let mut input = serde_json::to_vec(&serde_json::json!({"jsonrpc": "2.0", "id": 1,
    "method": "tools/call", "params": {
        "name": crate::team::tool::REMEMBER_TOOL,
        "arguments": {"text": text, "scope": "project"},
    }}))
    .unwrap();
    input.push(b'\n');
    input
}

/// Runs the real team server over one remember call on a thread, as the console master's
/// harness runs `team-mcp`, then writes `done` when given. Returns the server's answer.
fn remember_server(
    record: &Path,
    mailbox: &Path,
    text: &str,
    done: Option<PathBuf>,
) -> std::thread::JoinHandle<serde_json::Value> {
    let (record, mailbox, input) = (record.to_owned(), mailbox.to_owned(), remember_input(text));
    std::thread::spawn(move || {
        let mut output = Vec::new();
        crate::team::tool::serve(
            std::io::Cursor::new(input),
            &mut output,
            &record,
            Some(&mailbox),
            &[],
        )
        .unwrap();
        if let Some(done) = done {
            fs::write(done, b"").unwrap();
        }
        let line = output.split(|byte| *byte == b'\n').next().unwrap();
        serde_json::from_slice(line).unwrap()
    })
}

/// The text and error flag of one team server answer.
fn answer_text(answer: &serde_json::Value) -> (String, bool) {
    (
        answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        answer["result"]["isError"].as_bool().unwrap_or(true),
    )
}

/// Waits until the team server's call is in the mailbox, waiting for the engine.
async fn call_waits(mailbox: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !crate::team::tool::calls_waiting(mailbox) {
        assert!(
            Instant::now() < deadline,
            "the call never reached the mailbox"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The wall clock the stand-in hosts of rows about something other than time run under.
const TEST_CLOCK: Option<Duration> = Some(Duration::from_secs(150));

/// A real Claude host on a stand-in harness for a case's prepared console turn, with the
/// team server's drop record and mailbox in the stand-in's private tmp (OP-22). Inputs: the
/// case, its prepared turn, the host's wall clock, none for no clock, and the stand-in's
/// script. Outputs: the stand-in fixture, which owns the folders, the active master, the
/// record and mailbox.
async fn console_claude_master(
    case: &Fixture,
    prepared: &PreparedMasterTurn,
    clock: Option<Duration>,
    script: &str,
) -> (
    crate::engine::master_host_fixture::MasterFixture,
    ActiveMaster,
    PathBuf,
    PathBuf,
) {
    let mut fixture = crate::engine::master_host_fixture::MasterFixture::new("claude");
    let lease = fixture.lease("console-memory-turn", 1).unwrap();
    let home = lease.profile_path().unwrap();
    let harness = fixture.stand_in("claude", script);
    let sandbox = fixture.sandbox(&home, harness.parent().unwrap());
    let tmp = fixture.root.path().join("agent-tmp");
    let record = tmp.join(crate::team::tool::DROP_FILE);
    let mailbox = tmp.join(crate::team::tool::MAILBOX_FOLDER);
    private_directory(&mailbox);
    let engine = fixture.root.path().join("crazyagent");
    let mut command = crate::engine::claude_host::tools_launch_command(
        &harness,
        &fixture.project,
        &home,
        "native-default",
        None,
        crate::engine::claude_host::MasterTools {
            engine: &engine,
            drop: &record,
            mailbox: &mailbox,
            off: &[],
        },
    )
    .unwrap();
    crate::engine::claude_host::limit_steps(&mut command, prepared.steps);
    let profile_ref = CredentialRef::new("console-memory-profile").unwrap();
    let account_ref = fixture.connection.account_ref.clone();
    let claude = ClaudeHost::spawn(
        command,
        sandbox,
        lease,
        home,
        NativeAction::Launch {
            session_id: prepared.session.session_id.clone(),
            harness: NativeHarness::Claude,
            profile_ref: profile_ref.clone(),
            worktree_lease: prepared.worktree_lease.clone(),
        },
        account_ref.clone(),
        clock,
        ResourceBudget::unit(),
    )
    .await
    .unwrap();
    let (runtime_ref, capture_ref, capture) = case.runtime().allocate_master_capture().unwrap();
    let active = ActiveMaster {
        host: MasterHost::Claude(Box::new(claude)),
        capture: Capture::new(
            capture,
            prepared.attempt.request.route.resources.output_bytes,
        )
        .unwrap(),
        runtime: MasterRuntimeView {
            runtime_ref,
            artifact_sha256: "a".repeat(64),
            native_session_id: None,
        },
        capture_ref,
        provenance: Provenance {
            session_id: prepared.session.session_id.clone(),
            harness: NativeHarness::Claude,
            account_ref,
            profile_ref,
            worktree_lease: prepared.worktree_lease.clone(),
        },
        reported: Tokens::default(),
        access: crate::sandbox::policy::AgentAccess::Sandboxed,
        toolchains: Vec::new(),
    };
    (fixture, active, record, mailbox)
}

/// How a collected console turn ended, as one line a test compares: its reply text, its
/// failure cause, its engine error, or no end within the test's bound. Input: the bounded
/// collect outcome. Output: the line.
fn turn_end(
    observed: &Result<Result<MasterRuntimeObservation, EngineError>, tokio::time::error::Elapsed>,
) -> String {
    match observed {
        Ok(Ok(MasterRuntimeObservation::Response { reply, .. })) => format!("reply {}", reply.text),
        Ok(Ok(MasterRuntimeObservation::Failed { cause, .. })) => format!("failed {cause}"),
        Ok(Ok(_)) => "another observation".to_owned(),
        Ok(Err(error)) => format!("error {}", error.0),
        Err(_) => "no end in time".to_owned(),
    }
}

/// A console master turn reads its harness and replies while every inspection of the turn
/// outlasts the inspect interval, here behind an owner request holding the engine's owner
/// barrier 250 ms at a time: the next inspection waits one interval after the last one
/// ends, so the host's events and deadline are read between inspections (OP-22 r1).
#[tokio::test]
async fn a_console_master_turn_reads_its_harness_while_each_inspection_outlasts_the_interval() {
    let case = Fixture::open(POOL_CATALOG, codex_pool_selection(), true).await;
    case.sign_in("codex-c", true).await;
    let prepared = case.prepared(&case.turn.id).await;
    let (profile, hold) = case
        .runtime()
        .acquire_master_profile(&prepared, &case.turn.id)
        .await
        .unwrap();
    let (fixture, mut active, _, mailbox) =
        console_claude_master(&case, &prepared, TEST_CLOCK, MEMORY_TURN_HARNESS).await;
    fs::write(fixture.project.join("memory-answered"), b"").unwrap();
    let engine = Arc::clone(&case.runtime().engine);
    let barrier = tokio::spawn(async move {
        loop {
            let held = engine.owner_egress.read().await;
            tokio::time::sleep(Duration::from_millis(250)).await;
            drop(held);
        }
    });
    let observed = tokio::time::timeout(
        Duration::from_secs(30),
        case.runtime().collect_master_turn(
            &prepared,
            &case.turn.id,
            &prepared.text,
            &mut active,
            Some(&mailbox),
        ),
    )
    .await;
    barrier.abort();
    let _ = active.host.close().await;
    assert_eq!(
        turn_end(&observed),
        "reply DONE",
        "the turn read its harness while each inspection outlasted the interval"
    );
    let Ok(Ok(observed)) = observed else {
        unreachable!("the turn's reply was asserted");
    };
    let settled = case
        .runtime()
        .observe_master_turn(&prepared, &case.turn.id, observed)
        .await
        .unwrap();
    assert!(!settled.phase.outstanding(), "{:?}", settled.phase);
    drop(hold);
    drop(profile);
    drop(prepared);
    case.finish().await;
}

/// A console master's remember is answered by its own turn's tick (OP22-L1, F2): a real
/// Claude host on a stand-in harness holds the turn open, the team server writes the call
/// into the turn's mailbox, and collect_master_turn's inspect tick finds it waiting and sends
/// ServeMemory, so the answer reaches the server while the turn runs. Once the turn has
/// settled, ServeMemory refuses with master_memory_turn_not_running and a new call stays
/// untaken until the server gives up on it.
#[tokio::test]
async fn a_console_master_turn_s_own_tick_answers_its_remember_and_stops_with_the_turn() {
    use crate::memory::library::{MemoryOrigin, MemoryScope, PAGE_ITEMS_MAX};
    let case = Fixture::open(POOL_CATALOG, codex_pool_selection(), true).await;
    store_master_tools(&case, true);
    case.sign_in("codex-c", true).await;
    let prepared = case.prepared(&case.turn.id).await;
    assert!(prepared.tools, "the project's master tools switch is on");
    let (profile, hold) = case
        .runtime()
        .acquire_master_profile(&prepared, &case.turn.id)
        .await
        .unwrap();
    let (fixture, mut active, record, mailbox) =
        console_claude_master(&case, &prepared, TEST_CLOCK, MEMORY_TURN_HARNESS).await;
    let server = remember_server(
        &record,
        &mailbox,
        "Use cargo nextest.",
        Some(fixture.project.join("memory-answered")),
    );
    let observed = tokio::time::timeout(
        Duration::from_secs(120),
        case.runtime().collect_master_turn(
            &prepared,
            &case.turn.id,
            &prepared.text,
            &mut active,
            Some(&mailbox),
        ),
    )
    .await;
    let _ = active.host.close().await;
    let answered = server.join().unwrap();
    assert_eq!(
        turn_end(&observed),
        "reply DONE",
        "the turn replied after its memory call was answered"
    );
    let Ok(Ok(observed)) = observed else {
        unreachable!("the turn's reply was asserted");
    };
    let settled = case
        .runtime()
        .observe_master_turn(&prepared, &case.turn.id, observed)
        .await
        .unwrap();
    assert!(!settled.phase.outstanding(), "{:?}", settled.phase);
    let late = remember_server(&record, &mailbox, "Use rustfmt.", None);
    call_waits(&mailbox).await;
    let refused = case
        .runtime()
        .serve_master_memory(&prepared, &case.turn.id, &mailbox)
        .await
        .map_err(|error| error.0);
    let untaken = crate::team::tool::calls_waiting(&mailbox);
    let late = late.join().unwrap();
    let page = Fixture::read_owner(
        &case.owner,
        OwnerRead::Memories {
            scope: MemoryScope::Project {
                project_id: case.session.project_id.clone(),
            },
            search: None,
            before: None,
            limit: PAGE_ITEMS_MAX,
        },
    )
    .await;
    drop(hold);
    drop(profile);
    drop(prepared);
    case.finish().await;
    assert_eq!(
        answer_text(&answered),
        (
            "Saved to memory. recall finds it now, and agents started from now on get it."
                .to_owned(),
            false
        )
    );
    assert_eq!(refused, Err("master_memory_turn_not_running".to_owned()));
    assert!(untaken, "the call after the turn stays untaken");
    assert_eq!(
        answer_text(&late),
        (
            "Not sent: the engine did not take the call within a minute, so nothing was done."
                .to_owned(),
            true
        )
    );
    let OwnerData::Memories { page } = page.data else {
        panic!("the project memory page is a page of memories");
    };
    let shown: Vec<(&str, &MemoryOrigin)> = page
        .items
        .iter()
        .map(|memory| (memory.text.as_str(), &memory.origin))
        .collect();
    assert_eq!(
        shown,
        [(
            "Use cargo nextest.",
            &MemoryOrigin::Agent {
                agent: "Master".into()
            }
        )]
    );
}

/// A stand-in Claude Code for a tools-on console master turn that takes 30 tool steps and
/// then replies DONE. Like Claude Code, it ends the turn with `error_max_turns` once it has
/// taken as many steps as a `--max-turns` argument allows.
const THIRTY_STEP_HARNESS: &str = r#"#!/bin/sh
cap=0
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--max-turns" ]; then cap=$2; fi
  shift
done
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
i=0
while [ "$i" -lt 30 ]; do
  i=$((i+1))
  if [ "$cap" -gt 0 ] && [ "$i" -gt "$cap" ]; then
    printf '%s\n' '{"type":"result","subtype":"error_max_turns","uuid":"result-1","session_id":"native-session","is_error":true}'
    /bin/cat > /dev/null
    exit 1
  fi
  printf '{"type":"assistant","uuid":"call-%s","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-%s","type":"message","role":"assistant","model":"observed-model","content":[{"type":"tool_use","id":"tool-%s","name":"Read","input":{"file_path":"note.txt"}}]}}\n' "$i" "$i" "$i"
  printf '{"type":"user","uuid":"answer-%s","session_id":"native-session","parent_tool_use_id":null,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-%s","content":"read"}]}}\n' "$i" "$i"
done
printf '%s\n' '{"type":"assistant","uuid":"said-1","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-said","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"DONE"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":"DONE"}'
/bin/cat > /dev/null
exit 1
"#;

/// A stand-in Claude Code for a tools-on console master turn that says what it will do,
/// takes one tool step and ends the turn as a success without writing anything after it.
const SILENT_END_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
printf '%s\n' '{"type":"assistant","uuid":"said-1","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-1","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"I will write the note."}]}}'
printf '%s\n' '{"type":"assistant","uuid":"call-1","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-2","type":"message","role":"assistant","model":"observed-model","content":[{"type":"tool_use","id":"tool-1","name":"Write","input":{"file_path":"note.txt"}}]}}'
printf '%s\n' '{"type":"user","uuid":"answer-1","session_id":"native-session","parent_tool_use_id":null,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"written"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":""}'
/bin/cat > /dev/null
exit 1
"#;

/// Saves the owner's master step limit in a fixture's engine store, as the owner's Save in
/// Settings does. Inputs: the fixture and the limit. Output: none.
fn store_step_limit(case: &Fixture, limit: StepLimit) {
    let writer = Connection::open(case.root.path().join("engine.db")).unwrap();
    writer
        .busy_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let saved = crate::master_step_limit::save(&writer, &MasterStepSettings { limit }).unwrap();
    assert_eq!(saved, crate::protocol::Outcome::Applied);
    assert_eq!(
        crate::master_step_limit::load(&writer).unwrap().limit,
        limit
    );
}

/// The stand-in of `THIRTY_STEP_HARNESS` taking another number of tool steps before DONE.
/// Input: the number of steps. Output: the script.
fn stepping_harness(steps: u16) -> String {
    assert!((1..=400).contains(&steps));
    let script = THIRTY_STEP_HARNESS.replace("-lt 30 ]", &format!("-lt {steps} ]"));
    assert!(script.contains(&format!("\"$i\" -lt {steps} ]")));
    script
}

/// Saves the owner's master time limit in a fixture's engine store, as the owner's Save in
/// Settings does. Inputs: the fixture and the limit. Output: none.
fn store_time_limit(case: &Fixture, limit: TimeLimit) {
    let writer = Connection::open(case.root.path().join("engine.db")).unwrap();
    writer
        .busy_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let saved = crate::master_time_limit::save(&writer, &MasterTimeSettings { limit }).unwrap();
    assert_eq!(saved, crate::protocol::Outcome::Applied);
    assert_eq!(
        crate::master_time_limit::load(&writer).unwrap().limit,
        limit
    );
}

/// Collects one tools-on console master turn on a real Claude host running a stand-in
/// script, built by the engine's own tools-on command under the saved master step limit.
/// Inputs: the stand-in's script and the limit to save first. Outputs: how the turn ended,
/// as `turn_end` names it, and the turn as stored after a failed end was observed with the
/// reason its error maps to.
async fn tools_turn_on(script: &str, limit: StepLimit) -> (String, Option<MasterTurnView>) {
    limited_turn_on(script, limit, TimeLimit::NoLimit, TEST_CLOCK).await
}

/// Collects one tools-on console master turn on a real Claude host running a stand-in
/// script under the saved master step and time limits. `clock` is the wall clock the
/// stand-in host gets, so a row about a time limit of minutes ends in seconds. Inputs: the
/// script, both limits to save first and the clock. Outputs: those of `tools_turn_on`.
async fn limited_turn_on(
    script: &str,
    limit: StepLimit,
    time: TimeLimit,
    clock: Option<Duration>,
) -> (String, Option<MasterTurnView>) {
    let case = Fixture::open(POOL_CATALOG, codex_pool_selection(), true).await;
    store_master_tools(&case, true);
    store_step_limit(&case, limit);
    store_time_limit(&case, time);
    case.sign_in("codex-c", true).await;
    let prepared = case.prepared(&case.turn.id).await;
    assert!(prepared.tools, "the project's master tools switch is on");
    assert_eq!(
        prepared.steps, limit,
        "the turn carries the saved step limit"
    );
    assert_eq!(prepared.time, time, "the turn carries the saved time limit");
    let (profile, hold) = case
        .runtime()
        .acquire_master_profile(&prepared, &case.turn.id)
        .await
        .unwrap();
    let (_fixture, mut active, _, mailbox) =
        console_claude_master(&case, &prepared, clock, script).await;
    let observed = tokio::time::timeout(
        Duration::from_secs(60),
        case.runtime().collect_master_turn(
            &prepared,
            &case.turn.id,
            &prepared.text,
            &mut active,
            Some(&mailbox),
        ),
    )
    .await;
    let _ = active.host.close().await;
    let ended = turn_end(&observed);
    let stored = match &observed {
        Ok(Err(error)) => Some(
            case.runtime()
                .observe_master_turn(
                    &prepared,
                    &case.turn.id,
                    MasterRuntimeObservation::Failed {
                        reason: reason_for_error(error),
                        cause: error.0.clone(),
                    },
                )
                .await
                .unwrap(),
        ),
        _ => None,
    };
    drop(hold);
    drop(profile);
    drop(prepared);
    case.finish().await;
    (ended, stored)
}

/// A tools-on Claude master turn that needs 30 tool steps ends with its reply (OB-5): the
/// engine's command names no step count, so a harness that honours `--max-turns` as Claude
/// Code does is not stopped at step 24 with `error_max_turns`, which the owner saw as "its
/// reply was cut off" 92 s into a working turn.
#[tokio::test]
async fn a_tools_on_master_turn_of_thirty_tool_steps_ends_with_its_reply() {
    let (ended, stored) = tools_turn_on(THIRTY_STEP_HARNESS, StepLimit::NoLimit).await;
    assert_eq!(ended, "reply DONE", "the turn ran past 24 tool steps");
    assert!(stored.is_none(), "a replied turn is not stored as failed");
}

/// With the master step limit at No limit, the default, a tools-on Claude master turn of
/// 210 tool steps ends with its reply (OB-5b): past every limit the owner could pick, only
/// the host's run time and output limits bound it.
#[tokio::test]
async fn a_master_turn_with_no_step_limit_runs_past_two_hundred_steps_to_its_reply() {
    let (ended, stored) = tools_turn_on(&stepping_harness(210), StepLimit::NoLimit).await;
    assert_eq!(ended, "reply DONE", "no step count stopped the turn");
    assert!(stored.is_none(), "a replied turn is not stored as failed");
}

/// Under each master step limit the owner can pick, a tools-on Claude master turn that
/// needs more steps stops at the limit with the cause `master_step_limit_<steps>` and the
/// reason step_limit, not as a cut-off capture, and a turn of 30 steps still ends with its
/// reply (OB-5b).
#[tokio::test]
async fn a_master_turn_past_each_step_limit_stops_with_that_limit_named() {
    let limits = [
        (StepLimit::Steps50, 50_u16),
        (StepLimit::Steps100, 100),
        (StepLimit::Steps200, 200),
    ];
    for (limit, steps) in limits {
        let cause = format!("master_step_limit_{steps}");
        let (ended, stored) = tools_turn_on(&stepping_harness(steps + 5), limit).await;
        assert_eq!(ended, format!("error {cause}"));
        let stored = stored.expect("the stopped turn was stored");
        assert_eq!(stored.phase, MasterTurnPhase::Failed);
        assert_eq!(stored.reason, Some(MasterReason::StepLimit));
        assert_eq!(stored.cause.as_deref(), Some(cause.as_str()));
        assert!(stored.reply.is_none());
        let (ended, stored) = tools_turn_on(THIRTY_STEP_HARNESS, limit).await;
        assert_eq!(ended, "reply DONE", "30 steps fit under {steps}");
        assert!(stored.is_none(), "a replied turn is not stored as failed");
    }
}

/// A stand-in Claude Code for a console master turn that works three seconds without a
/// word and then replies DONE.
const SLOW_REPLY_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
/bin/sleep 3
printf '%s\n' '{"type":"assistant","uuid":"said-1","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-said","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"DONE"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":"DONE"}'
/bin/cat > /dev/null
exit 1
"#;

/// The stand-in of `SLOW_REPLY_HARNESS` working another number of seconds before DONE.
/// Input: the seconds. Output: the script.
fn slow_reply_harness(seconds: u16) -> String {
    assert!((1..=120).contains(&seconds));
    let script = SLOW_REPLY_HARNESS.replace("/bin/sleep 3\n", &format!("/bin/sleep {seconds}\n"));
    assert!(script.contains(&format!("\n/bin/sleep {seconds}\n")));
    script
}

/// With the master time limit at No limit, the default, the turn's host gets no wall clock
/// (OB-5c): the prepared turn names no run time, the host is started with none, and a turn
/// that works three seconds without a word ends with its reply.
#[tokio::test]
async fn a_master_turn_with_no_time_limit_runs_under_no_clock_to_its_reply() {
    let limit = TimeLimit::NoLimit;
    assert_eq!(limit.run_time(), None, "no limit gives the host no clock");
    let (ended, stored) = limited_turn_on(
        SLOW_REPLY_HARNESS,
        StepLimit::NoLimit,
        limit,
        limit.run_time(),
    )
    .await;
    assert_eq!(ended, "reply DONE", "no clock stopped the turn");
    assert!(stored.is_none(), "a replied turn is not stored as failed");
}

/// Under each master time limit the owner can pick, a turn its host's wall clock ends
/// stops with the cause `master_time_limit_<minutes>` and the reason time_limit, not
/// `claude_host_deadline` (OB-5c), and a turn of three seconds still ends with its reply.
/// The stopped stand-in works 90 s under a host clock of 8 s, so each row ends in seconds;
/// the clock the engine gives a real host is the limit's own minutes.
#[tokio::test]
async fn a_master_turn_its_clock_ends_under_each_time_limit_stops_with_that_limit_named() {
    let limits = [
        (TimeLimit::Minutes10, 10_u64),
        (TimeLimit::Minutes30, 30),
        (TimeLimit::Minutes60, 60),
    ];
    let long = slow_reply_harness(90);
    for (limit, minutes) in limits {
        assert_eq!(limit.run_time(), Some(Duration::from_secs(minutes * 60)));
        let cause = format!("master_time_limit_{minutes}");
        let clock = Some(Duration::from_secs(8));
        let (ended, stored) = limited_turn_on(&long, StepLimit::NoLimit, limit, clock).await;
        assert_eq!(ended, format!("error {cause}"));
        let stored = stored.expect("the stopped turn was stored");
        assert_eq!(stored.phase, MasterTurnPhase::Failed);
        assert_eq!(stored.reason, Some(MasterReason::TimeLimit));
        assert_eq!(stored.cause.as_deref(), Some(cause.as_str()));
        assert!(stored.reply.is_none());
        let (ended, stored) =
            limited_turn_on(SLOW_REPLY_HARNESS, StepLimit::NoLimit, limit, TEST_CLOCK).await;
        assert_eq!(
            ended, "reply DONE",
            "a three second turn fits in {minutes} minutes"
        );
        assert!(stored.is_none(), "a replied turn is not stored as failed");
    }
}

/// A tools-on master turn its harness ends as a success with no text after its last tool
/// step fails as a turn without a reply, not as a cut-off capture (OB-5): the cause is
/// master_empty_reply and the stored reason is no_reply.
#[tokio::test]
async fn a_tools_on_master_turn_that_ends_without_reply_text_says_so_and_is_not_cut_off() {
    let (ended, stored) = tools_turn_on(SILENT_END_HARNESS, StepLimit::NoLimit).await;
    assert_eq!(ended, "error master_empty_reply");
    let stored = stored.expect("the failed end was stored");
    assert_eq!(stored.phase, MasterTurnPhase::Failed);
    assert_eq!(stored.reason, Some(MasterReason::NoReply));
    assert_eq!(stored.cause.as_deref(), Some("master_empty_reply"));
    assert!(stored.reply.is_none());
}

/// ServeMemory answers a console master's mailbox only while its own turn runs with tools on
/// (OP22-L1): once the owner switches the project's master tools off, and once the turn has
/// failed, it refuses with master_memory_turn_not_running and the call stays untaken until
/// the server gives up on it.
#[tokio::test]
async fn a_console_master_s_memory_call_is_refused_with_tools_off_and_after_a_failed_turn() {
    let case = Fixture::open(POOL_CATALOG, codex_pool_selection(), true).await;
    store_master_tools(&case, true);
    case.sign_in("codex-c", true).await;
    let prepared = case.prepared(&case.turn.id).await;
    assert!(prepared.tools, "the project's master tools switch is on");
    let (profile, hold) = case
        .runtime()
        .acquire_master_profile(&prepared, &case.turn.id)
        .await
        .unwrap();
    let running = case
        .runtime()
        .observe_master_turn(
            &prepared,
            &case.turn.id,
            MasterRuntimeObservation::Running {
                runtime: MasterRuntimeView {
                    runtime_ref: RequestId::new("memory-runtime").unwrap(),
                    artifact_sha256: "a".repeat(64),
                    native_session_id: None,
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(running.phase, MasterTurnPhase::Running);
    let tmp = case.root.path().join("console-turn-tmp");
    private_directory(&tmp);
    let record = tmp.join(crate::team::tool::DROP_FILE);
    let mailbox = tmp.join(crate::team::tool::MAILBOX_FOLDER);
    private_directory(&mailbox);
    let server = remember_server(&record, &mailbox, "Use cargo nextest.", None);
    call_waits(&mailbox).await;
    store_master_tools(&case, false);
    let tools_off = case
        .runtime()
        .serve_master_memory(&prepared, &case.turn.id, &mailbox)
        .await
        .map_err(|error| error.0);
    let untaken_off = crate::team::tool::calls_waiting(&mailbox);
    store_master_tools(&case, true);
    let failed = case
        .runtime()
        .observe_master_turn(
            &prepared,
            &case.turn.id,
            MasterRuntimeObservation::Failed {
                reason: MasterReason::CaptureIncomplete,
                cause: "master_capture_incomplete".into(),
            },
        )
        .await
        .unwrap();
    let ended = case
        .runtime()
        .serve_master_memory(&prepared, &case.turn.id, &mailbox)
        .await
        .map_err(|error| error.0);
    let untaken_ended = crate::team::tool::calls_waiting(&mailbox);
    let answer = server.join().unwrap();
    drop(hold);
    drop(profile);
    drop(prepared);
    case.finish().await;
    let not_running = Err("master_memory_turn_not_running".to_owned());
    assert_eq!(tools_off, not_running, "master tools switched off");
    assert!(untaken_off, "the call stays untaken with master tools off");
    assert_eq!(failed.phase, MasterTurnPhase::Failed);
    assert_eq!(ended, not_running, "the turn failed");
    assert!(untaken_ended, "the call stays untaken after the turn");
    assert_eq!(
        answer_text(&answer),
        (
            "Not sent: the engine did not take the call within a minute, so nothing was done."
                .to_owned(),
            true
        )
    );
}

#[tokio::test]
async fn a_console_master_turn_s_remember_lands_on_the_project_memory_page() {
    use crate::memory::library::{MemoryOrigin, MemoryScope, PAGE_ITEMS_MAX};
    use crate::team::tool;
    let case = Fixture::open(POOL_CATALOG, codex_pool_selection(), true).await;
    store_master_tools(&case, true);
    case.sign_in("codex-c", true).await;
    let prepared = case.prepared(&case.turn.id).await;
    assert!(prepared.tools, "the project's master tools switch is on");
    let (profile, hold) = case
        .runtime()
        .acquire_master_profile(&prepared, &case.turn.id)
        .await
        .unwrap();
    let running = case
        .runtime()
        .observe_master_turn(
            &prepared,
            &case.turn.id,
            MasterRuntimeObservation::Running {
                runtime: MasterRuntimeView {
                    runtime_ref: RequestId::new("memory-runtime").unwrap(),
                    artifact_sha256: "a".repeat(64),
                    native_session_id: None,
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(running.phase, MasterTurnPhase::Running);
    let tmp = case.root.path().join("console-turn-tmp");
    private_directory(&tmp);
    let record = tmp.join(tool::DROP_FILE);
    let mailbox = tmp.join(tool::MAILBOX_FOLDER);
    private_directory(&mailbox);
    let mut input = Vec::new();
    for request in [
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": tool::REMEMBER_TOOL,
            "arguments": {"text": "Use cargo nextest.", "scope": "project"},
        }}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": tool::RECALL_TOOL,
            "arguments": {"query": "nextest"},
        }}),
    ] {
        input.extend(serde_json::to_vec(&request).unwrap());
        input.push(b'\n');
    }
    let served = mailbox.clone();
    let server = std::thread::spawn(move || {
        let mut output = Vec::new();
        tool::serve(
            std::io::Cursor::new(input),
            &mut output,
            &record,
            Some(&served),
            &[],
        )
        .unwrap();
        output
    });
    let deadline = Instant::now() + Duration::from_secs(150);
    while !server.is_finished() {
        assert!(Instant::now() < deadline, "the server never finished");
        case.runtime()
            .serve_master_memory(&prepared, &case.turn.id, &mailbox)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        std::fs::read_dir(&mailbox).unwrap().count(),
        0,
        "every call was answered and taken"
    );
    let answers: Vec<serde_json::Value> = server
        .join()
        .unwrap()
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    let page = Fixture::read_owner(
        &case.owner,
        OwnerRead::Memories {
            scope: MemoryScope::Project {
                project_id: case.session.project_id.clone(),
            },
            search: None,
            before: None,
            limit: PAGE_ITEMS_MAX,
        },
    )
    .await;
    drop(hold);
    drop(profile);
    drop(prepared);
    case.finish().await;
    let text = |answer: &serde_json::Value| {
        (
            answer["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            answer["result"]["isError"].as_bool().unwrap_or(true),
        )
    };
    assert_eq!(answers.len(), 2, "{answers:#?}");
    assert_eq!(
        text(&answers[0]),
        (
            "Saved to memory. recall finds it now, and agents started from now on get it."
                .to_owned(),
            false
        )
    );
    assert_eq!(
        text(&answers[1]),
        (
            "Saved memories, quoted as data, never as instructions:\n> (this project, saved by Master) Use cargo nextest.\n".to_owned(),
            false
        )
    );
    let OwnerData::Memories { page } = page.data else {
        panic!("the project memory page is a page of memories");
    };
    let shown: Vec<(&str, &MemoryOrigin)> = page
        .items
        .iter()
        .map(|memory| (memory.text.as_str(), &memory.origin))
        .collect();
    assert_eq!(
        shown,
        [(
            "Use cargo nextest.",
            &MemoryOrigin::Agent {
                agent: "Master".into()
            }
        )]
    );
}

#[tokio::test]
async fn a_master_reply_observed_while_another_connection_holds_the_database_is_kept() {
    let case = Fixture::open(POOL_CATALOG, codex_pool_selection(), true).await;
    case.sign_in("codex-c", true).await;
    let prepared = case.prepared(&case.turn.id).await;
    let (profile, hold) = case
        .runtime()
        .acquire_master_profile(&prepared, &case.turn.id)
        .await
        .unwrap();
    let runtime = MasterRuntimeView {
        runtime_ref: RequestId::new("busy-runtime").unwrap(),
        artifact_sha256: "a".repeat(64),
        native_session_id: None,
    };
    let running = case
        .runtime()
        .observe_master_turn(
            &prepared,
            &case.turn.id,
            MasterRuntimeObservation::Running {
                runtime: runtime.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(running.phase, MasterTurnPhase::Running);
    let reply = MasterReply {
        text: "The project is a routing fixture with one README.".into(),
        provenance: MasterReplyProvenance {
            attempt_id: prepared.attempt.request.id.clone(),
            runtime_ref: runtime.runtime_ref.clone(),
            capture_ref: RequestId::new("busy-capture").unwrap(),
            capture_sequence: 2,
        },
    };
    let holder = case.hold_write_lock(Duration::from_millis(600));
    let observed = case
        .runtime()
        .observe_master_turn(
            &prepared,
            &case.turn.id,
            MasterRuntimeObservation::Response {
                reply: reply.clone(),
                delegations: vec![],
            },
        )
        .await
        .map(|turn| turn.phase);
    holder.join().unwrap();
    drop(hold);
    drop(profile);
    let persisted = case.persisted_turn().await;
    drop(prepared);
    case.finish().await;
    assert_eq!(observed, Ok(MasterTurnPhase::Succeeded));
    assert_eq!(persisted.phase, MasterTurnPhase::Succeeded);
    assert_eq!(persisted.reply, Some(reply));
}

async fn end_tools_turn(
    case: &Fixture,
    prepared: &PreparedMasterTurn,
    turn: &RequestId,
    edit: impl FnOnce(&Path),
    ending: MasterRuntimeObservation,
) -> MasterTurnView {
    let (profile, hold) = case
        .runtime()
        .acquire_master_profile(prepared, turn)
        .await
        .unwrap();
    let runtime = MasterRuntimeView {
        runtime_ref: RequestId::new("landing-runtime").unwrap(),
        artifact_sha256: "a".repeat(64),
        native_session_id: None,
    };
    let running = case
        .runtime()
        .observe_master_turn(
            prepared,
            turn,
            MasterRuntimeObservation::Running { runtime },
        )
        .await
        .unwrap();
    assert_eq!(running.phase, MasterTurnPhase::Running);
    assert_eq!(running.changes, None);
    edit(&prepared.cwd);
    let ended = case
        .runtime()
        .observe_master_turn(prepared, turn, ending)
        .await
        .unwrap();
    drop(hold);
    drop(profile);
    ended
}

fn landing_reply(prepared: &PreparedMasterTurn) -> MasterRuntimeObservation {
    MasterRuntimeObservation::Response {
        reply: MasterReply {
            text: "I wrote the notes.".into(),
            provenance: MasterReplyProvenance {
                attempt_id: prepared.attempt.request.id.clone(),
                runtime_ref: RequestId::new("landing-runtime").unwrap(),
                capture_ref: RequestId::new("landing-capture").unwrap(),
                capture_sequence: 2,
            },
        },
        delegations: vec![],
    }
}

async fn tools_case(tools: bool) -> (Fixture, Box<PreparedMasterTurn>) {
    let case = Fixture::open(POOL_CATALOG, codex_pool_selection(), true).await;
    store_master_tools(&case, tools);
    case.sign_in("codex-c", true).await;
    let prepared = case.prepared(&case.turn.id).await;
    assert_eq!(prepared.tools, tools);
    assert!(prepared.cwd.join(".git").is_file());
    (case, prepared)
}

#[tokio::test]
async fn a_tools_on_master_turn_that_edits_a_file_ends_with_a_pending_change_set_and_a_note() {
    let (case, prepared) = tools_case(true).await;
    let reply = landing_reply(&prepared);
    let write = |cwd: &Path| fs::write(cwd.join("NOTES.md"), "Written by the master.\n").unwrap();
    let ended = end_tools_turn(&case, &prepared, &case.turn.id, write, reply).await;
    let persisted = case.persisted_turn().await;
    let sets = case
        .owner
        .change_sets(case.session.project_id.clone())
        .await
        .unwrap();
    let session = case.session.session_id.clone();
    let db = crate::landing::open(case.root.path()).unwrap();
    let workspace = crate::landing::Workspace {
        project: case.session.project_id.clone(),
        agent: crate::landing::ChangeAgent {
            kind: crate::landing::AgentKind::Master,
            id: session.as_str().to_owned(),
            label: "Master".into(),
        },
        repository: prepared.repository.clone(),
        worktree: prepared.cwd.clone(),
        git_dir: crate::worktrees::lease_git_dir(&db, &prepared.lease).unwrap(),
    };
    let end = |turn: &str| crate::landing::TurnEnd {
        outcome: crate::landing::TurnOutcome::Succeeded,
        turn: turn.into(),
        prompt_line: "Another turn".into(),
    };
    let quiet = crate::landing_turns::record(&db, &workspace, &end("quiet-turn"), None).unwrap();
    fs::write(prepared.cwd.join("MORE.md"), "More from the master.\n").unwrap();
    let more = crate::landing_turns::record(&db, &workspace, &end("more-turn"), None).unwrap();
    drop(db);
    drop(prepared);
    case.finish().await;
    use crate::landing_turns::Landed;
    assert_eq!(
        quiet,
        Landed::Waiting(sets[0].id.clone()),
        "a turn that added nothing"
    );
    assert_eq!(
        more,
        Landed::Added(sets[0].id.clone()),
        "a later turn adds to the same set"
    );
    assert_eq!(ended.phase, MasterTurnPhase::Succeeded);
    assert_eq!(ended.changes.as_deref(), Some(CHANGES_WAITING));
    assert_eq!(persisted.changes.as_deref(), Some(CHANGES_WAITING));
    assert_eq!(sets.len(), 1, "{sets:?}");
    assert_eq!(sets[0].state, crate::landing::ChangeState::Pending);
    assert_eq!(sets[0].agent.kind, crate::landing::AgentKind::Master);
    assert_eq!(sets[0].agent.id, session.as_str());
    assert_eq!(sets[0].files, 1);
    assert_eq!(sets[0].last_outcome, crate::landing::TurnOutcome::Succeeded);
}

#[tokio::test]
async fn a_master_head_pointed_away_from_its_lease_branch_is_refused_and_moves_no_branch() {
    let (case, prepared) = tools_case(true).await;
    let read = |args: &[&str]| -> String {
        let output = crate::spawn_gate::output(
            std::process::Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .arg("-C")
                .arg(&prepared.repository)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null()),
        )
        .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let owner = read(&["symbolic-ref", "HEAD"]).trim().to_owned();
    let branches = || {
        read(&[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads/",
        ])
    };
    let before = branches();
    let db = crate::landing::open(case.root.path()).unwrap();
    let git_dir = crate::worktrees::lease_git_dir(&db, &prepared.lease).unwrap();
    let lease = format!("ref: refs/heads/{}\n", prepared.lease.branch);
    assert_eq!(fs::read_to_string(git_dir.join("HEAD")).unwrap(), lease);
    assert_ne!(owner, format!("refs/heads/{}", prepared.lease.branch));
    fs::write(git_dir.join("HEAD"), format!("ref: {owner}\n")).unwrap();
    fs::write(prepared.cwd.join("NOTES.md"), "Written past the lease.\n").unwrap();
    let workspace = crate::landing::Workspace {
        project: case.session.project_id.clone(),
        agent: crate::landing::ChangeAgent {
            kind: crate::landing::AgentKind::Master,
            id: case.session.session_id.as_str().to_owned(),
            label: "Master".into(),
        },
        repository: prepared.repository.clone(),
        worktree: prepared.cwd.clone(),
        git_dir: git_dir.clone(),
    };
    let end = crate::landing::TurnEnd {
        outcome: crate::landing::TurnOutcome::Succeeded,
        turn: "away-turn".into(),
        prompt_line: "Write past the lease".into(),
    };
    let refused = crate::landing_turns::record(&db, &workspace, &end, None).unwrap_err();
    assert_eq!(refused.0, "changes_workspace_invalid");
    assert_eq!(branches(), before);
    assert!(
        crate::landing::list(&db, &case.session.project_id)
            .unwrap()
            .is_empty()
    );
    fs::write(git_dir.join("HEAD"), &lease).unwrap();
    fs::remove_file(prepared.cwd.join("NOTES.md")).unwrap();
    drop(db);
    drop(prepared);
    case.finish().await;
}

#[tokio::test]
async fn a_failed_tools_on_master_turn_still_keeps_its_edits_for_review() {
    let (case, prepared) = tools_case(true).await;
    let failed = MasterRuntimeObservation::Failed {
        reason: MasterReason::CaptureIncomplete,
        cause: "master_capture_incomplete".into(),
    };
    let write = |cwd: &Path| fs::write(cwd.join("half.txt"), "Half done.\n").unwrap();
    let ended = end_tools_turn(&case, &prepared, &case.turn.id, write, failed).await;
    let sets = case
        .owner
        .change_sets(case.session.project_id.clone())
        .await
        .unwrap();
    drop(prepared);
    case.finish().await;
    assert_eq!(ended.phase, MasterTurnPhase::Failed);
    assert_eq!(ended.changes.as_deref(), Some(CHANGES_WAITING));
    assert_eq!(sets.len(), 1, "{sets:?}");
    assert_eq!(sets[0].state, crate::landing::ChangeState::Pending);
    assert_eq!(sets[0].last_outcome, crate::landing::TurnOutcome::Failed);
}

#[tokio::test]
async fn a_tools_on_master_turn_that_changed_nothing_and_a_tools_off_turn_leave_no_change_set() {
    let (case, prepared) = tools_case(true).await;
    let reply = landing_reply(&prepared);
    let ended = end_tools_turn(&case, &prepared, &case.turn.id, |_| {}, reply).await;
    let sets = case
        .owner
        .change_sets(case.session.project_id.clone())
        .await
        .unwrap();
    drop(prepared);
    case.finish().await;
    assert_eq!(ended.phase, MasterTurnPhase::Succeeded);
    assert_eq!(ended.changes, None);
    assert!(sets.is_empty(), "{sets:?}");

    let (case, prepared) = tools_case(false).await;
    let reply = landing_reply(&prepared);
    let stray =
        |cwd: &Path| fs::write(cwd.join("stray.txt"), "Not from a tools-on master.\n").unwrap();
    let ended = end_tools_turn(&case, &prepared, &case.turn.id, stray, reply).await;
    let sets = case
        .owner
        .change_sets(case.session.project_id.clone())
        .await
        .unwrap();
    drop(prepared);
    case.finish().await;
    assert_eq!(ended.phase, MasterTurnPhase::Succeeded);
    assert_eq!(ended.changes, None);
    assert!(sets.is_empty(), "{sets:?}");
}

#[tokio::test]
async fn a_master_worktree_commits_only_into_the_git_folder_its_lease_recorded() {
    let (case, prepared) = tools_case(true).await;
    let db = case.db();
    let recorded = crate::worktrees::lease_git_dir(&db, &prepared.lease).unwrap();
    let pointer = fs::read(prepared.cwd.join(".git")).unwrap();
    let elsewhere = case.root.path().join("elsewhere-git");
    private_directory(&elsewhere);
    fs::write(
        prepared.cwd.join(".git"),
        format!("gitdir: {}\n", elsewhere.display()),
    )
    .unwrap();
    let repointed = crate::worktrees::lease_git_dir(&db, &prepared.lease);
    let sibling = recorded.with_file_name("sibling-lease");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("commondir"), "../..\n").unwrap();
    fs::write(
        sibling.join("gitdir"),
        format!("{}/.git\n", prepared.lease.path),
    )
    .unwrap();
    fs::write(
        prepared.cwd.join(".git"),
        format!("gitdir: {}\n", sibling.display()),
    )
    .unwrap();
    let swapped = crate::worktrees::lease_git_dir(&db, &prepared.lease);
    fs::write(prepared.cwd.join(".git"), pointer).unwrap();
    let restored = crate::worktrees::lease_git_dir(&db, &prepared.lease).unwrap();
    drop(db);
    let repository = prepared.repository.clone();
    drop(prepared);
    case.finish().await;
    assert_eq!(
        recorded.parent(),
        Some(repository.join(".git/worktrees").as_path())
    );
    assert_eq!(
        repointed.unwrap_err().0,
        "worktree_metadata_escape",
        "a folder outside the repository's worktrees is refused"
    );
    assert_eq!(
        swapped.unwrap_err().0,
        "worktree_identity_changed",
        "another folder that links back is still not the one the lease recorded"
    );
    assert_eq!(restored, recorded);
}

const RESOLVE_LINE: &str = "CrazyAgent asked the master to resolve the conflicts in NOTES.md.";

/// Ends the first tools-on turn with an edit to NOTES.md, so its change set is pending,
/// then commits a conflicting NOTES.md on the owner's branch. Output: the fixture, the
/// pending change set and the master's worktree.
async fn conflicting_case() -> (Fixture, crate::landing::ChangeSetView, PathBuf) {
    let (case, prepared) = tools_case(true).await;
    let reply = landing_reply(&prepared);
    let write = |cwd: &Path| fs::write(cwd.join("NOTES.md"), "Written by the master.\n").unwrap();
    end_tools_turn(&case, &prepared, &case.turn.id, write, reply).await;
    let (repository, worktree) = (prepared.repository.clone(), prepared.cwd.clone());
    drop(prepared);
    private_file(&repository.join("NOTES.md"), b"Written by the owner.\n");
    git(&repository, case.root.path(), &["add", "NOTES.md"]);
    git(
        &repository,
        case.root.path(),
        &["commit", "-qm", "Owner notes"],
    );
    let mut pending = case.change_sets().await;
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].state, crate::landing::ChangeState::Pending);
    (case, pending.remove(0), worktree)
}

impl Fixture {
    async fn change_sets(&self) -> Vec<crate::landing::ChangeSetView> {
        self.owner
            .change_sets(self.session.project_id.clone())
            .await
            .unwrap()
    }

    fn entries(&self) -> i64 {
        self.db()
            .query_row("SELECT COUNT(*) FROM conversation_entries", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    /// Reads the session's turns until `count` exist and none is outstanding, within
    /// TEST_BOUND. Input: the count. Output: the settled turns, oldest first.
    async fn settled_turns(&self, count: usize) -> Vec<MasterTurnView> {
        let deadline = Instant::now() + TEST_BOUND;
        loop {
            let mut turns = self.turns().await;
            turns.sort_by_key(|turn| turn.ordinal);
            if turns.len() == count && turns.iter().all(|turn| !turn.phase.outstanding()) {
                return turns;
            }
            assert!(Instant::now() < deadline, "turns never settled: {turns:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[tokio::test]
async fn asking_a_master_to_update_starts_its_resolve_turn_at_once_with_a_line_the_owner_sees() {
    let (case, pending, worktree) = conflicting_case().await;
    let entries = case.entries();
    case.owner.update_changes(pending.id.clone()).await.unwrap();
    let mut started = case.turns().await;
    started.sort_by_key(|turn| turn.ordinal);
    let merged = fs::read_to_string(worktree.join("NOTES.md")).unwrap();
    let settled = case.settled_turns(2).await;
    let after = case.entries();
    case.finish().await;
    assert_eq!(started.len(), 2, "{started:?}");
    let resolve = &started[1];
    let request = resolve.engine_request.as_ref().expect("engine request");
    assert_eq!(request.change_set, pending.id);
    assert_eq!(request.line, RESOLVE_LINE);
    assert!(
        request
            .text
            .contains("CrazyAgent merged it into your worktree"),
        "{}",
        request.text
    );
    assert!(request.text.contains("\n- NOTES.md\n"), "{}", request.text);
    assert_eq!(resolve.ordinal, started[0].ordinal + 1);
    assert!(started[0].engine_request.is_none());
    assert!(merged.contains("<<<<<<<"), "{merged}");
    assert_eq!(
        after, entries,
        "the engine's request is not an owner message"
    );
    assert_eq!(settled[1].id, resolve.id);
    assert_eq!(settled[1].engine_request.as_ref(), Some(request));
    assert_eq!(settled[1].phase, MasterTurnPhase::Failed);
    assert!(
        settled[1].attempt.is_some(),
        "the driver prepared the engine's turn"
    );
}

#[tokio::test]
async fn an_update_asked_while_the_master_works_starts_its_resolve_turn_right_after() {
    let (case, pending, _) = conflicting_case().await;
    let second = case.submit("Keep going.").await;
    case.owner.update_changes(pending.id.clone()).await.unwrap();
    let waiting = case.turns().await;
    let updating = case.change_sets().await;
    let runtime = OwnerRuntimeHandle {
        engine: case.runtime().engine.clone(),
        authority: case.runtime().authority.clone(),
    };
    let started = case
        .runtime()
        .authority
        .master_dispatch
        .lock()
        .await
        .start(
            runtime,
            &case.session.project_id,
            &case.session.session_id,
            &second,
        )
        .unwrap();
    let settled = case.settled_turns(3).await;
    case.finish().await;
    assert_eq!(waiting.len(), 2, "{waiting:?}");
    assert!(waiting.iter().all(|turn| turn.engine_request.is_none()));
    assert_eq!(updating[0].state, crate::landing::ChangeState::Updating);
    assert!(started);
    assert_eq!(settled[1].id, second);
    let resolve = &settled[2];
    assert_eq!(resolve.ordinal, settled[1].ordinal + 1);
    let request = resolve.engine_request.as_ref().expect("engine request");
    assert_eq!(request.line, RESOLVE_LINE);
    assert_eq!(request.change_set, pending.id);
}

#[tokio::test]
async fn the_master_gets_the_resolve_prompt_and_later_turns_show_the_engine_line() {
    let (case, pending, _) = conflicting_case().await;
    let second = case.submit("Keep going.").await;
    case.owner.update_changes(pending.id.clone()).await.unwrap();
    let busy = case
        .runtime()
        .start_waiting_resolve(&case.session.project_id, &case.session.session_id)
        .await
        .unwrap();
    let next = case.prepared(&second).await;
    end_tools_turn(&case, &next, &second, |_| {}, landing_reply(&next)).await;
    drop(next);
    let resolve = case
        .runtime()
        .start_waiting_resolve(&case.session.project_id, &case.session.session_id)
        .await
        .unwrap()
        .expect("the waiting resolve turn starts once the turn before it ended");
    let again = case
        .runtime()
        .start_waiting_resolve(&case.session.project_id, &case.session.session_id)
        .await
        .unwrap();
    let asked = case.prepared(&resolve).await;
    let fix = |cwd: &Path| fs::write(cwd.join("NOTES.md"), "Both notes.\n").unwrap();
    let ended = end_tools_turn(&case, &asked, &resolve, fix, landing_reply(&asked)).await;
    let text = asked.text.clone();
    drop(asked);
    let reviewed = case.change_sets().await;
    let third = case.submit("Thanks.").await;
    let later = case.prepared(&third).await;
    let history = later.text.clone();
    drop(later);
    case.finish().await;
    assert_eq!(busy, None);
    assert_eq!(again, None);
    assert!(
        text.contains("CrazyAgent merged it into your worktree"),
        "{text}"
    );
    assert!(text.contains("\n- NOTES.md\n"), "{text}");
    assert!(
        text.trim_end()
            .ends_with("Do not run git commit; CrazyAgent records your changes when you finish."),
        "{text}"
    );
    assert_eq!(ended.changes.as_deref(), Some(CHANGES_WAITING));
    assert_eq!(reviewed.len(), 1, "{reviewed:?}");
    assert_eq!(reviewed[0].id, pending.id);
    assert_eq!(reviewed[0].state, crate::landing::ChangeState::Pending);
    assert!(
        history.contains(&format!("Engine: {RESOLVE_LINE}")),
        "{history}"
    );
    assert!(history.ends_with("Thanks."), "{history}");
}

#[tokio::test]
async fn an_engine_turn_whose_change_set_was_changed_after_it_was_recorded_is_refused() {
    let (case, pending, _) = conflicting_case().await;
    case.owner.update_changes(pending.id.clone()).await.unwrap();
    let settled = case.settled_turns(2).await;
    let resolve = settled[1].id.clone();
    let other = format!(
        "{}{}",
        if pending.id.as_str().starts_with('0') {
            '1'
        } else {
            '0'
        },
        &pending.id.as_str()[1..]
    );
    let writer = Connection::open(case.root.path().join("engine.db")).unwrap();
    writer.busy_timeout(Duration::from_secs(10)).unwrap();
    let body: String = writer
        .query_row(
            "SELECT body FROM master_turns WHERE session=?1 AND id=?2",
            params![case.session.session_id.as_str(), resolve.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let changed = writer
        .execute(
            "UPDATE master_turns SET body=?1 WHERE session=?2 AND id=?3",
            params![
                body.replace(pending.id.as_str(), &other),
                case.session.session_id.as_str(),
                resolve.as_str()
            ],
        )
        .unwrap();
    drop(writer);
    let read = case
        .owner
        .request(request(
            "tampered-read",
            0,
            OwnerOperation::Read {
                query: OwnerRead::ProjectMasterTurns {
                    project_id: case.session.project_id.clone(),
                    session_id: case.session.session_id.clone(),
                    cursor: None,
                    limit: 16,
                },
            },
        ))
        .await
        .unwrap();
    case.finish().await;
    assert_eq!(changed, 1);
    assert!(body.contains(pending.id.as_str()), "{body}");
    assert_eq!(
        read.response,
        OwnerResponse::Rejected {
            code: "master_input_provenance".into()
        }
    );
}

#[tokio::test]
async fn a_master_with_tools_off_refuses_ask_agent_to_update_and_keeps_its_change_set() {
    let (case, pending, worktree) = conflicting_case().await;
    store_master_tools(&case, false);
    let refused = case.owner.update_changes(pending.id.clone()).await;
    let kept = case.change_sets().await;
    let notes = fs::read_to_string(worktree.join("NOTES.md")).unwrap();
    let turns = case.turns().await;
    case.finish().await;
    assert_eq!(refused.unwrap_err().0, "changes_master_tools_off");
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].state, crate::landing::ChangeState::Pending);
    assert_eq!(kept[0].head, pending.head);
    assert_eq!(notes, "Written by the master.\n");
    assert_eq!(turns.len(), 1, "{turns:?}");
}

/// Records synthetic usage through the usage store the poll writes and owner account
/// plans into the scheduling table, through a writable engine connection.
/// Takes the case, limits rows of connection, weekly used and weekly reset, and plans of
/// connection, reserve and pin.
fn schedule_rows(case: &Fixture, limits: &[(&str, f64, i64)], plans: &[(&str, u8, Option<&str>)]) {
    let db = Connection::open_with_flags(
        case.root.path().join("engine.db"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    crate::usage::initialize(&db).unwrap();
    for (connection, weekly, resets) in limits {
        crate::usage::record_observation(
            &db,
            &reference(connection),
            &crate::usage::Observation {
                plan: None,
                five_hour: Some(crate::usage::LimitWindow {
                    used_percent: 10.0,
                    resets_at_ms: None,
                }),
                weekly: Some(crate::usage::LimitWindow {
                    used_percent: *weekly,
                    resets_at_ms: Some(*resets),
                }),
                provider: Some(crate::usage::poll::Provider::Claude),
            },
            crate::usage::LimitSource::Poll,
            clock_ms(),
        )
        .unwrap();
    }
    for (connection, reserve, pin) in plans {
        db.execute(
            "INSERT INTO account_plans(connection,rank,reserve_percent,pin) VALUES (?1,NULL,?2,?3)",
            params![connection, reserve, pin],
        )
        .unwrap();
    }
}

fn clock_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

#[tokio::test]
async fn a_master_turn_starts_on_the_account_whose_weekly_window_resets_first_and_still_fails_over()
{
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    case.sign_in("claude-b", true).await;
    let now = clock_ms();
    schedule_rows(
        &case,
        &[
            ("claude-a", 30.0, now + 72 * 3_600_000),
            ("claude-b", 30.0, now + 3_600_000),
        ],
        &[],
    );
    let first = case.prepared(&case.turn.id).await;
    assert_eq!(first.attempt.connection.id, reference("claude-b"));
    case.fail(
        &case.turn.id,
        &first.attempt.request.id,
        MasterReason::RateLimited,
    )
    .await;
    drop(first);
    assert!(case.paused_until("claude-b").is_some());
    let second = case.submit("Summarize what we discussed so far.").await;
    let next = case.prepared(&second).await;
    assert_eq!(next.attempt.connection.id, reference("claude-a"));
    assert!(next.text.ends_with("Summarize what we discussed so far."));
    drop(next);
    case.finish().await;
}

#[tokio::test]
async fn a_reserve_and_a_never_use_pin_block_the_master_turn_naming_each_account() {
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    case.sign_in("claude-b", true).await;
    let now = clock_ms();
    schedule_rows(
        &case,
        &[("claude-b", 95.0, now + 3_600_000)],
        &[("claude-a", 0, Some("never_use")), ("claude-b", 10, None)],
    );
    let result = case
        .command(MasterRuntimeCommand::Prepare {
            host: case.runtime().identity(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
        })
        .await
        .unwrap();
    let MasterRuntimeResult::Turn(blocked) = result else {
        panic!("a pool the schedule leaves empty must not prepare a turn");
    };
    assert_eq!(blocked.phase, MasterTurnPhase::Blocked);
    assert_eq!(blocked.reason, Some(MasterReason::ScopeRevoked));
    assert_eq!(blocked.cause.as_deref(), Some("account_never_use"));
    let named: Vec<(&str, Option<MasterReason>)> = blocked
        .accounts
        .iter()
        .map(|account| (account.connection.as_str(), account.reason))
        .collect();
    assert!(named.contains(&("claude-a", Some(MasterReason::ScopeRevoked))));
    assert!(named.contains(&("claude-b", Some(MasterReason::QuotaExhausted))));
    assert_eq!(case.paused_until("claude-b"), None);
    case.finish().await;
}

/// Runs the drop-file server a conversation master's question tools talk to on the given
/// calls, the way the master's harness would, and returns what the server answered.
fn served_questions(record: &Path, calls: &[serde_json::Value]) -> String {
    let mut input = Vec::new();
    for (id, call) in calls.iter().enumerate() {
        let line = serde_json::json!({"jsonrpc": "2.0", "id": id + 1, "method": "tools/call", "params": call});
        input.extend(serde_json::to_vec(&line).unwrap());
        input.push(b'\n');
    }
    let mut output = Vec::new();
    crate::team::tool::serve(std::io::Cursor::new(input), &mut output, record, None, &[]).unwrap();
    String::from_utf8(output).unwrap()
}

/// A conversation master's question calls reach the questions log when its turns end: a
/// question from one turn is logged under its session, a record naming another message
/// than the turn's own is refused, a record naming the next turn's owner message settles
/// the question with the engine's copy of that message, and the notes a replied turn
/// carried are marked as given.
#[tokio::test]
async fn a_conversation_master_s_questions_are_logged_and_recorded_when_its_turns_end() {
    use crate::questions::{AnsweredBy, Asker, QuestionStatus};
    use crate::team::tool::{ASK_TOOL, DROP_FILE, RECORD_TOOL};
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    case.sign_in("claude-b", true).await;
    let project = case.session.project_id.clone();
    let session = case.session.session_id.clone();
    let folder = case.root.path().join("master-tools");
    private_directory(&folder);
    let record = folder.join(DROP_FILE);
    let db = crate::questions::open(case.root.path()).unwrap();
    let ask = |question: &str| {
        serde_json::json!({"name": ASK_TOOL, "arguments": {
            "question": question,
            "options": ["Postgres", "SQLite"],
            "recommended": "Postgres"
        }})
    };

    let first = case.prepared(&case.turn.id).await;
    assert!(first.owner_turn.is_some());
    let answered = served_questions(
        &record,
        &[
            ask("Which database should the service use?"),
            ask("Should the service keep a cache?"),
        ],
    );
    assert_eq!(answered.matches("Recorded.").count(), 2, "{answered}");
    case.runtime()
        .log_session_asks(&first, &record, Vec::new())
        .await;
    let pending = crate::questions::pending(&db, &project).unwrap();
    assert_eq!(pending.len(), 2, "{pending:?}");
    let asker = Asker::Session(session.clone()).name();
    assert!(pending.iter().all(|question| question.asker == asker));
    let (database, cache) = (pending[0].id, pending[1].id);
    case.fail(
        &case.turn.id,
        &first.attempt.request.id,
        MasterReason::InvalidAuthentication,
    )
    .await;
    drop(first);

    let owner_words = "Use Postgres 16 for the service.";
    let second = case.submit(owner_words).await;
    let next = case.prepared(&second).await;
    let owner_turn = next.owner_turn.unwrap();
    let record_call = |turn: u64| {
        serde_json::json!({"name": RECORD_TOOL, "arguments": {
            "question": database, "version": 0, "owner_turn": turn
        }})
    };
    let answered = served_questions(&record, &[record_call(owner_turn + 1)]);
    assert!(answered.contains("Noted."), "{answered}");
    case.runtime()
        .log_session_asks(&next, &record, Vec::new())
        .await;
    assert_eq!(
        crate::questions::pending(&db, &project).unwrap().len(),
        2,
        "a record naming another message than the turn's own is not taken"
    );
    let answered = served_questions(&record, &[record_call(owner_turn)]);
    assert!(answered.contains("Noted."), "{answered}");
    case.runtime()
        .log_session_asks(&next, &record, Vec::new())
        .await;
    let settled = crate::questions::page(&db, &project, None)
        .unwrap()
        .items
        .into_iter()
        .find(|question| question.id == database)
        .unwrap();
    assert_eq!(settled.status, QuestionStatus::Answered);
    assert_eq!(settled.answer.as_deref(), Some(owner_words));
    assert_eq!(settled.answered_by, Some(AnsweredBy::Chat));
    assert!(
        crate::questions::session_notes(&db, &project, &session)
            .unwrap()
            .is_empty(),
        "an answer recorded from the master's own chat sends no note back"
    );

    let version = crate::questions::pending(&db, &project).unwrap()[0].version;
    assert_eq!(
        crate::questions::dismiss(&db, cache, version).unwrap(),
        crate::questions::Settlement::Settled
    );
    let notes = crate::questions::session_notes(&db, &project, &session).unwrap();
    assert_eq!(
        notes.iter().map(|note| note.question).collect::<Vec<_>>(),
        vec![cache]
    );
    case.runtime()
        .log_session_asks(&next, &record, vec![cache])
        .await;
    assert!(
        crate::questions::session_notes(&db, &project, &session)
            .unwrap()
            .is_empty(),
        "the note a replied turn carried is not given again"
    );
    drop(next);
    drop(db);
    case.finish().await;
}

/// Reads the commit a worktree's HEAD names. Inputs: the worktree and the git home. Output:
/// the commit id.
fn head_of(worktree: &Path, home: &Path) -> String {
    let output = crate::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-C")
            .arg(worktree)
            .args(["rev-parse", "HEAD"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A master whose first turn on its new worktree ends with only its own commits keeps them
/// as its change set: the engine records the commit the leased worktree starts from before
/// the turn runs, as it does for a team agent's new worktree (T3 r2).
#[tokio::test]
async fn a_master_turn_records_where_its_leased_worktree_starts_before_it_runs() {
    let case = Fixture::new(MasterBackend::Rcli, true).await;
    let proof = case.register().await;
    let held = exclusive_profile_hold(case.root.path(), &proof.profile_ref)
        .expect("no agent holds the configured profile yet");
    assert_eq!(
        case.drive()
            .await
            .expect_err("the held profile stops the prepared turn before it runs")
            .0,
        "native_profile_in_use"
    );
    drop(held);
    let db = crate::landing::open(case.root.path()).unwrap();
    let crate::protocol::Outcome::Worktrees { leases } =
        crate::worktrees::leases(&db, &case.session.bootstrap_task_id).unwrap()
    else {
        panic!("the master holds its worktree lease");
    };
    let worktree = PathBuf::from(&leases[0].path);
    let start = head_of(&worktree, case.root.path());
    fs::write(worktree.join("NOTES"), "master notes\n").unwrap();
    git(&worktree, case.root.path(), &["add", "NOTES"]);
    git(
        &worktree,
        case.root.path(),
        &["commit", "-q", "-m", "Add notes"],
    );
    let workspace = crate::landing::Workspace {
        project: case.session.project_id.clone(),
        agent: crate::landing::ChangeAgent {
            kind: crate::landing::AgentKind::Master,
            id: case.session.session_id.as_str().to_owned(),
            label: "Master".into(),
        },
        repository: case.repository.clone(),
        worktree,
        git_dir: crate::worktrees::lease_git_dir(&db, &leases[0]).unwrap(),
    };
    let end = crate::landing::TurnEnd {
        outcome: crate::landing::TurnOutcome::Succeeded,
        turn: case.turn.id.as_str().to_owned(),
        prompt_line: "Add notes".into(),
    };
    let id = crate::landing::record_turn_end(&db, &workspace, &end)
        .unwrap()
        .expect("the master's own commit makes its change set");
    let set = crate::landing::list(&db, &case.session.project_id)
        .unwrap()
        .into_iter()
        .find(|set| set.id == id)
        .unwrap();
    assert_eq!((set.base.as_str(), set.files), (start.as_str(), 1));
    drop(db);
    case.finish().await;
}

/// Each console master turn counts one turn on its project's master row, the key its tokens
/// count against, so the master's Turns covers its console turns as well as its
/// live-terminal ones (AL4 r2, rv-code M1).
#[tokio::test]
async fn each_console_master_turn_counts_one_turn_on_its_project_s_master_row() {
    let case = Fixture::open(POOL_CATALOG, codex_pool_selection(), true).await;
    case.sign_in("codex-c", true).await;
    let prepared = case.prepared(&case.turn.id).await;
    let project = prepared.session.project_id.clone();
    let reported = Tokens {
        input: 100,
        output: 10,
        cache_read: 0,
        cache_write: 0,
    };
    let started = crate::team::now_ms();
    for _ in 0..2 {
        case.runtime()
            .count_master_usage(&prepared, None, reported)
            .await;
    }
    let db = crate::usage::open(case.root.path()).unwrap();
    let counted: Vec<(String, i64)> = db
        .prepare("SELECT agent,SUM(turns) FROM usage_turns WHERE project=?1 GROUP BY agent")
        .unwrap()
        .query_map([project.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let master = crate::usage::service::master_agent(Some(&project));
    assert_eq!(counted, [(master.clone(), 2)]);
    let since = started - started.rem_euclid(3_600_000);
    let shown: Vec<(String, Option<u64>)> =
        crate::usage::metrics::read(&db, Some(&project), since, crate::team::now_ms())
            .unwrap()
            .agents
            .into_iter()
            .map(|row| (row.agent, row.turns))
            .collect();
    assert_eq!(shown, [(master, Some(2))]);
    drop(db);
    drop(prepared);
    case.finish().await;
}
