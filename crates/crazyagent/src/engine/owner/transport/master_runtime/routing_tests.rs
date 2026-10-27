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

fn exclusive_profile_hold(root: &Path, profile_ref: &CredentialRef) -> Option<fs::File> {
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
        .map(|()| file)
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

#[tokio::test]
async fn an_unregistered_rcli_profile_stops_the_turn_with_its_own_cause() {
    let case = Fixture::new(MasterBackend::Rcli, true).await;
    let returned = case.drive().await;
    assert_eq!(
        returned
            .expect_err("an RCLI master without a registered profile cannot start")
            .0,
        "rcli_profile_configuration_required"
    );
    let stopped = case.persisted_turn().await;
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
    let case = Fixture::open(POOL_CATALOG, pool_selection(), true).await;
    case.sign_in("claude-a", true).await;
    case.sign_in("claude-b", true).await;
    let first = case.prepared(&case.turn.id).await;
    assert_eq!(first.attempt.connection.id, reference("claude-a"));
    let unavailable = EngineError("native_profile_credential_unavailable".into());
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
    assert_eq!(failed.phase, MasterTurnPhase::Failed);
    assert_eq!(failed.reason, Some(MasterReason::ReadinessUnknown));
    assert_eq!(
        failed.cause.as_deref(),
        Some("native_profile_credential_unavailable")
    );
    drop(first);
    assert_eq!(case.paused_until("claude-a"), None);
    assert!(!case.rejected("claude-a"));
    assert_eq!(case.stale().await, vec![reference("claude-a")]);
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
    let quiet = crate::landing_turns::record(&db, &workspace, &end("quiet-turn")).unwrap();
    fs::write(prepared.cwd.join("MORE.md"), "More from the master.\n").unwrap();
    let more = crate::landing_turns::record(&db, &workspace, &end("more-turn")).unwrap();
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

#[tokio::test]
async fn asking_a_master_to_update_merges_the_owner_branch_and_its_next_turn_resolves_it() {
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
    let project = case.session.project_id.clone();
    let pending = case.owner.change_sets(project.clone()).await.unwrap();
    case.owner
        .update_changes(pending[0].id.clone())
        .await
        .unwrap();
    let updating = case.owner.change_sets(project.clone()).await.unwrap();
    let merged = fs::read_to_string(worktree.join("NOTES.md")).unwrap();
    let second = case.submit("Carry on with the notes.").await;
    let next = case.prepared(&second).await;
    let resolve = |cwd: &Path| fs::write(cwd.join("NOTES.md"), "Both notes.\n").unwrap();
    let ended = end_tools_turn(&case, &next, &second, resolve, landing_reply(&next)).await;
    let reviewed = case.owner.change_sets(project).await.unwrap();
    let text = next.text.clone();
    drop(next);
    case.finish().await;
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(updating[0].state, crate::landing::ChangeState::Updating);
    assert!(merged.contains("<<<<<<<"), "{merged}");
    assert!(
        text.contains("CrazyAgent merged it into your worktree"),
        "{text}"
    );
    assert!(text.contains("\n- NOTES.md\n"), "{text}");
    assert!(text.ends_with("Carry on with the notes."), "{text}");
    assert_eq!(ended.changes.as_deref(), Some(CHANGES_WAITING));
    assert_eq!(reviewed.len(), 1, "{reviewed:?}");
    assert_eq!(reviewed[0].id, pending[0].id);
    assert_eq!(reviewed[0].state, crate::landing::ChangeState::Pending);
}
