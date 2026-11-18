use super::*;
use crate::config::ModelEndpoint;

mod stop_settlement_tests {
    include!("stop_settlement_tests.rs");
}

mod brief_boundary_tests {
    include!("brief_boundary_tests.rs");
}

mod readiness_tests {
    include!("readiness_tests.rs");
}

mod pruned_history_tests {
    include!("pruned_history_tests.rs");
}
use crate::engine::owner::{ModelAccessCollection, Started};
use crate::master_session::{
    ListedAccountProof, MasterBootstrapAuth, MasterSelection, RcliConfiguredProfileProof,
};
use crate::protocol::{CredentialRef, ProjectSpec};
use crate::routing::{ResourceBudget, RouteRequest};
use crate::storage::{Job, Storage};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use tokio::sync::oneshot;

const CONFIG: &str = r#"version = 1
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

const PROMPT: &str = "Describe this project in one sentence without using tools.";

const LISTED_AUTH: &str = r#"auth = { method = "api_key", credential_ref = "listed-key" }"#;

const LISTED_KEY: &[u8] = b"sk-listed-master-saved-key-0123456789abcdef";

/// The fixture master's selection on one backend and target. Inputs: the backend and
/// the target. Output: the selection.
fn selection(backend: MasterBackend, target: &str) -> MasterSelection {
    MasterSelection {
        backend,
        pool_ref: reference("local-pool"),
        target_profile_ref: reference(target),
        route: RouteRequest {
            preferred_target: Some(reference(target)),
            required_target: Some(reference(target)),
            required_capabilities: vec![],
            account_ref: reference("local-account"),
            budget_ref: reference("conversation-budget"),
            resources: ResourceBudget::unit(),
        },
    }
}

const SPARE: &str = r#"[[model_access.connections]]
id = "spare-connection"
provider = "local-provider"
harness = "rcli"
protocol = "openai-chat"
account_ref = "spare-account"
endpoint = { kind = "http", url = "http://127.0.0.1:11435/v1" }
model = { id = "local-model", author = "local-author" }
auth = { method = "none" }"#;

/// One keyless RCLI key of the fixture's settings, as they hold it. Inputs: its id,
/// account and endpoint. Output: the connection.
fn rcli_key(id: &str, account: &str, url: &str) -> ModelConnection {
    ModelConnection {
        id: reference(id),
        provider: reference("local-provider"),
        harness: reference("rcli"),
        protocol: reference("openai-chat"),
        account_ref: reference(account),
        endpoint: ModelEndpoint::Http { url: url.into() },
        model: crate::config::ModelReference {
            id: "local-model".into(),
            author: reference("local-author"),
        },
        auth: ModelAuthentication::None {},
    }
}

/// The first RCLI key of the fixture's pool, as CONFIG holds it.
fn local_connection() -> ModelConnection {
    rcli_key(
        "local-connection",
        "local-account",
        "http://127.0.0.1:11434/v1",
    )
}

/// The second RCLI key of the pooled fixture, as the settings hold it.
fn spare_connection() -> ModelConnection {
    rcli_key(
        "spare-connection",
        "spare-account",
        "http://127.0.0.1:11435/v1",
    )
}

/// The profile the second RCLI key runs master turns under: its own configured one.
fn spare_profile() -> CredentialRef {
    crate::config::configured_profile_ref(&spare_connection()).unwrap()
}

/// The pool's RCLI key as a keyed OpenAI account saved before DEC-150 addendum 9a,
/// under the builtin provider id rcli keeps, as Fixture::legacy_keyed's settings hold
/// it. The key reference is synthetic. Output: the connection.
fn legacy_keyed_connection() -> ModelConnection {
    ModelConnection {
        provider: reference("openai"),
        auth: ModelAuthentication::ApiKey {
            credential_ref: reference("synthetic-key-ref-legacy"),
        },
        ..local_connection()
    }
}

struct Fixture {
    storage: Storage,
    config: EffectiveConfig,
    started: Started,
    session: MasterSessionView,
    turn: MasterTurnView,
    path: PathBuf,
    global: PathBuf,
    home: PathBuf,
    repository: PathBuf,
    root: tempfile::TempDir,
    _repos: Option<tempfile::TempDir>,
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    revision: u64,
    turn: String,
    counts: [i64; 6],
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn git(repository: &Path, home: &Path, arguments: &[&str]) {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
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
            .args([
                "-c",
                "user.name=Master storage fixture",
                "-c",
                "user.email=master-storage@example.invalid",
            ])
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "actual Git fixture failed: {output:?}"
    );
}

async fn owner_read(storage: &Storage, started: &Started, query: OwnerRead) -> OwnerReadReply {
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerRead(
            started.owners[0].admission.clone(),
            query,
            send,
        ))
        .unwrap_or_else(|_| panic!("actual owner read queue unavailable"));
    receive.await.unwrap().unwrap()
}

async fn owner_apply(storage: &Storage, started: &Started, id: &str, mutation: OwnerMutation) {
    let result = owner_result(storage, started, id, mutation).await;
    assert!(
        matches!(
            result,
            OwnerResult::Applied | OwnerResult::OnboardingStarted { .. }
        ),
        "{result:?}"
    );
}

async fn owner_result(
    storage: &Storage,
    started: &Started,
    id: &str,
    mutation: OwnerMutation,
) -> OwnerResult {
    let basis = owner_read(storage, started, OwnerRead::Capabilities).await;
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
        .unwrap_or_else(|_| panic!("actual owner mutation queue unavailable"));
    receive.await.unwrap().unwrap().result
}

impl Fixture {
    async fn new(with_repository: bool) -> Self {
        Self::new_with_message(with_repository, PROMPT).await
    }

    async fn new_with_message(with_repository: bool, message: &str) -> Self {
        Self::build(with_repository, false, message).await
    }

    /// A master on a project the owner added as an existing folder: the
    /// committed repository outside the runtime is onboarded, not declared.
    async fn onboarded() -> Self {
        Self::build(true, true, PROMPT).await
    }

    async fn build(with_repository: bool, onboarded: bool, message: &str) -> Self {
        let selection = selection(MasterBackend::Rcli, "local-profile");
        Self::build_with(CONFIG, with_repository, onboarded, message, selection).await
    }

    /// A master whose settings already hold what every model access edit writes into
    /// them (config.rs approve_assigned_accounts): its pool's RCLI key has its own
    /// profile among the allowed destinations, as in the native rebind fixtures, so an
    /// edit outside the pool leaves the master's scope as it was (PA-6, DEC-092).
    async fn kept() -> Self {
        let local = crate::config::configured_profile_ref(&local_connection()).unwrap();
        let config = CONFIG.replace(
            r#"allowed_destinations = ["local-profile"]"#,
            &format!(
                r#"allowed_destinations = ["local-profile", "{}"]"#,
                local.as_str()
            ),
        );
        assert!(config.contains(local.as_str()));
        let selection = selection(MasterBackend::Rcli, "local-profile");
        Self::build_with(&config, true, false, PROMPT, selection).await
    }

    /// A master whose pool holds a second RCLI key on its own account and endpoint,
    /// allowed as a master target under its own configured profile (PA-6).
    async fn pooled() -> Self {
        let config = CONFIG
            .replace(
                r#"allowed_destinations = ["local-profile"]"#,
                &format!(
                    r#"allowed_destinations = ["local-profile", "{}"]"#,
                    spare_profile().as_str()
                ),
            )
            .replace(
                r#"approved_accounts = ["local-account"]"#,
                r#"approved_accounts = ["local-account", "spare-account"]"#,
            )
            .replace(
                r#"accounts = ["local-account"]"#,
                r#"accounts = ["local-account", "spare-account"]"#,
            )
            .replace(
                r#"members = ["local-connection"]"#,
                r#"members = ["local-connection", "spare-connection"]"#,
            )
            .replace(
                "[[model_access.pools]]",
                &format!("{SPARE}\n[[model_access.pools]]"),
            );
        for replaced in [
            format!(
                r#"allowed_destinations = ["local-profile", "{}"]"#,
                spare_profile().as_str()
            ),
            r#"approved_accounts = ["local-account", "spare-account"]"#.to_owned(),
            "[model_access]\naccounts = [\"local-account\", \"spare-account\"]".to_owned(),
            r#"members = ["local-connection", "spare-connection"]"#.to_owned(),
            format!("{SPARE}\n[[model_access.pools]]"),
        ] {
            assert!(config.contains(&replaced), "{replaced}");
        }
        let selection = selection(MasterBackend::Rcli, "local-profile");
        Self::build_with(&config, true, false, PROMPT, selection).await
    }

    /// A master whose pool's RCLI key is legacy_keyed_connection: a keyed OpenAI
    /// account saved before DEC-150 addendum 9a under the builtin provider id.
    async fn legacy_keyed() -> Self {
        let provider = "id = \"local-connection\"\nprovider = \"openai\"";
        let auth = r#"auth = { method = "api_key", credential_ref = "synthetic-key-ref-legacy" }"#;
        let config = CONFIG
            .replace(
                "id = \"local-connection\"\nprovider = \"local-provider\"",
                provider,
            )
            .replace(r#"auth = { method = "none" }"#, auth);
        assert!(config.contains(provider) && config.contains(auth));
        let selection = selection(MasterBackend::Rcli, "local-profile");
        Self::build_with(&config, true, false, PROMPT, selection).await
    }

    /// A listed master (DEC-180 T20): its pool's one account is a keyed account of the
    /// stand-in ACP harness, whose connection is the master's target, as a native
    /// master's is.
    async fn listed() -> Self {
        let config = CONFIG
            .replace(
                r#"allowed_destinations = ["local-profile"]"#,
                r#"allowed_destinations = ["local-connection"]"#,
            )
            .replace(r#"harness = "rcli""#, r#"harness = "standin-acp""#)
            .replace(r#"auth = { method = "none" }"#, LISTED_AUTH);
        assert!(config.contains(r#"harness = "standin-acp""#));
        assert!(config.contains(LISTED_AUTH));
        assert!(config.contains(r#"allowed_destinations = ["local-connection"]"#));
        let backend = MasterBackend::Listed("standin-acp");
        let selection = selection(backend, "local-connection");
        Self::build_with(&config, true, false, PROMPT, selection).await
    }

    async fn build_with(
        config: &str,
        with_repository: bool,
        onboarded: bool,
        message: &str,
        selection: MasterSelection,
    ) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-mrc-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        private_file(&global, config.as_bytes());
        let home = root.path().join("configured-home");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let sdk = home.join(".backboard");
        std::fs::create_dir(&sdk).unwrap();
        std::fs::set_permissions(&sdk, std::fs::Permissions::from_mode(0o700)).unwrap();
        private_file(
            &sdk.join("config.json"),
            &serde_json::to_vec(&serde_json::json!({
                "model": {"provider": "local-provider", "model": "local-model"},
                "providers": [{"id": "local-provider", "name": "Local provider",
                    "protocol": "openai-chat", "baseUrl": "http://127.0.0.1:11434/v1",
                    "auth": {"type": "none"}}]
            }))
            .unwrap(),
        );
        let repos = onboarded.then(|| {
            tempfile::Builder::new()
                .prefix("ca-mrc-repos-")
                .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
                .unwrap()
        });
        let repository = match &repos {
            Some(repos) => repos.path().canonicalize().unwrap().join("rcli-project"),
            None => root.path().join("repository"),
        };
        if with_repository {
            std::fs::create_dir(&repository).unwrap();
            git(
                &repository,
                root.path(),
                &["init", "--initial-branch=trunk"],
            );
            private_file(
                &repository.join("README"),
                b"Actual committed master workspace fixture.\n",
            );
            git(&repository, root.path(), &["add", "README"]);
            git(
                &repository,
                root.path(),
                &["commit", "-m", "Initialize master workspace fixture"],
            );
        }
        let path = root.path().join("engine.db");
        let (storage, config) =
            Storage::open_with_settings_sources(&path, &global, &root.path().join("project.toml"))
                .await
                .unwrap();
        let (send, receive) = oneshot::channel();
        storage
            .sender
            .try_send(Job::OwnerStart(send))
            .unwrap_or_else(|_| panic!("actual owner start queue unavailable"));
        let started = receive.await.unwrap().unwrap();
        let project = ProjectId::new("rcli-project").unwrap();
        let create = if onboarded {
            OwnerMutation::StartOnboarding {
                path: repository.to_str().unwrap().into(),
                pool: reference("local-pool"),
            }
        } else {
            OwnerMutation::CreateProject {
                project_id: project.clone(),
                spec: ProjectSpec {
                    name: "Configured local master".into(),
                    repositories: if with_repository {
                        vec![repository.to_str().unwrap().into()]
                    } else {
                        vec![]
                    },
                },
            }
        };
        owner_apply(&storage, &started, "create-rcli-project", create).await;
        let basis = owner_read(
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
            panic!("actual model source basis missing");
        };
        owner_apply(
            &storage,
            &started,
            "open-rcli-master",
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Open {
                    project_id: project.clone(),
                    expected_config_id: basis.config_id,
                    expected_source_hash: source_hash,
                    selection: Box::new(selection),
                },
            },
        )
        .await;
        let state = owner_read(
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
            panic!("actual committed master session missing");
        };
        let session = *session;
        owner_apply(
            &storage,
            &started,
            "submit-rcli-turn",
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Submit {
                    project_id: project.clone(),
                    session_id: session.session_id.clone(),
                    text: message.into(),
                },
            },
        )
        .await;
        let state = owner_read(
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
            panic!("actual submitted master session missing");
        };
        let session = *session;
        let state = owner_read(
            &storage,
            &started,
            OwnerRead::ProjectMasterTurns {
                project_id: project,
                session_id: session.session_id.clone(),
                cursor: None,
                limit: 1,
            },
        )
        .await;
        let OwnerData::ProjectMasterTurns { page } = state.data else {
            panic!("actual queued master turn missing");
        };
        assert_eq!(page.turns.len(), 1);
        let turn = page.turns.into_iter().next().unwrap();
        assert_eq!(turn.phase, MasterTurnPhase::Queued);
        assert_eq!(turn.attempt, None);
        assert_eq!(turn.runtime, None);
        assert_eq!(turn.reply, None);
        Self {
            storage,
            config,
            started,
            session,
            turn,
            path,
            global,
            home,
            repository,
            root,
            _repos: repos,
        }
    }

    async fn runtime(
        &self,
        command: MasterRuntimeCommand,
    ) -> Result<MasterRuntimeResult, EngineError> {
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::MasterRuntime(Box::new(command), send))
            .unwrap_or_else(|_| panic!("actual master runtime queue unavailable"));
        let checkout = match receive.await.unwrap() {
            Ok(MasterRuntimeResult::Checkout(checkout)) => checkout,
            result => return result,
        };
        let checked = tokio::task::spawn_blocking(move || checkout.materialize())
            .await
            .unwrap();
        Box::pin(self.runtime(checked)).await
    }

    async fn register(&self) -> RcliConfiguredProfileProof {
        let result = self
            .runtime(MasterRuntimeCommand::RegisterRcliProfile {
                host: self.started.host.clone(),
                project_id: self.session.project_id.clone(),
                session_id: self.session.session_id.clone(),
                configured_home: self.home.clone(),
            })
            .await
            .unwrap();
        let MasterRuntimeResult::ConfiguredProfile(proof) = result else {
            panic!("actual configured profile receipt missing");
        };
        *proof
    }

    async fn view(&self) -> (MasterSessionView, Vec<MasterTurnView>) {
        let project_id = self.session.project_id.clone();
        let state = owner_read(
            &self.storage,
            &self.started,
            OwnerRead::ProjectMaster {
                project_id: project_id.clone(),
            },
        )
        .await;
        let OwnerData::ProjectMaster {
            session: Some(session),
        } = state.data
        else {
            panic!("actual master session missing");
        };
        let state = owner_read(
            &self.storage,
            &self.started,
            OwnerRead::ProjectMasterTurns {
                project_id,
                session_id: self.session.session_id.clone(),
                cursor: None,
                limit: 8,
            },
        )
        .await;
        let OwnerData::ProjectMasterTurns { page } = state.data else {
            panic!("actual master turns missing");
        };
        (*session, page.turns)
    }

    fn action(&self, id: &str, action: MasterOwnerAction) -> (String, OwnerMutation) {
        (id.into(), OwnerMutation::ProjectMaster { action })
    }

    fn submit(&self, id: &str, text: &str) -> (String, OwnerMutation) {
        self.action(
            id,
            MasterOwnerAction::Submit {
                project_id: self.session.project_id.clone(),
                session_id: self.session.session_id.clone(),
                text: text.into(),
            },
        )
    }

    async fn result(&self, (id, mutation): (String, OwnerMutation)) -> OwnerResult {
        owner_result(&self.storage, &self.started, &id, mutation).await
    }

    fn prepare(&self) -> MasterRuntimeCommand {
        MasterRuntimeCommand::Prepare {
            host: self.started.host.clone(),
            project_id: self.session.project_id.clone(),
            session_id: self.session.session_id.clone(),
            turn_id: self.turn.id.clone(),
        }
    }

    fn db(&self) -> Connection {
        let db = Connection::open_with_flags(
            &self.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        db.busy_timeout(std::time::Duration::from_secs(1)).unwrap();
        db
    }

    fn snapshot(&self) -> Snapshot {
        let mut db = self.db();
        let tx = db.transaction().unwrap();
        let tables = [
            "master_preparations",
            "model_gateway_attempts",
            "model_gateway_observations",
            "native_auth_operations",
            "worktree_leases",
            "bootstrap_workspaces",
        ];
        let counts = tables.map(|table| {
            tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
        });
        let snapshot = Snapshot {
            revision: owner_revision(&tx).unwrap(),
            turn: tx
                .query_row(
                    "SELECT body FROM master_turns WHERE session=?1 AND id=?2",
                    params![self.session.session_id.as_str(), self.turn.id.as_str()],
                    |row| row.get(0),
                )
                .unwrap(),
            counts,
        };
        drop(tx);
        snapshot
    }

    async fn close(self) -> tempfile::TempDir {
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::OwnerStop(self.started.host, send))
            .unwrap_or_else(|_| panic!("actual owner stop queue unavailable"));
        receive.await.unwrap().unwrap();
        let (send, receive) = oneshot::channel();
        self.storage
            .sender
            .try_send(Job::Close(send))
            .unwrap_or_else(|_| panic!("actual actor close queue unavailable"));
        receive.await.unwrap().unwrap();
        assert!(self.root.path().is_dir());
        self.root
    }
}

#[tokio::test]
async fn registered_local_rcli_prepares_real_lease_and_claims_one_producer_without_ready() {
    let case = Fixture::new(true).await;
    let original_config = std::fs::read(case.home.join(".backboard/config.json")).unwrap();
    let proof = case.register().await;
    assert_eq!(case.register().await, proof);
    assert_eq!(proof.database_instance, case.started.host.db_instance);
    assert_eq!(proof.engine_epoch, case.started.host.engine_epoch);
    assert_eq!(proof.config_id, case.config.id);
    assert_eq!(proof.source_hash, case.session.source_hash);
    assert_eq!(proof.profile_ref, case.session.selection.target_profile_ref);
    assert_eq!(proof.connection.harness, reference("rcli"));
    assert_eq!(proof.connection.auth, ModelAuthentication::None {});
    assert_eq!(
        proof.profile_config_sha256,
        format!("{:x}", Sha256::digest(&original_config))
    );
    assert!(proof.generation > 0);
    assert!(!proof.custody_id.is_empty());
    let result = case.runtime(case.prepare()).await.unwrap();
    let MasterRuntimeResult::Prepared(prepared) = result else {
        panic!("registered local RCLI must prepare the actual first conversation");
    };
    assert_eq!(prepared.text, PROMPT);
    assert_eq!(prepared.session.session_id, case.session.session_id);
    assert_eq!(
        prepared.session.bootstrap_task_id,
        case.session.bootstrap_task_id
    );
    assert_eq!(prepared.attempt.request.project_id, case.session.project_id);
    assert_eq!(
        prepared.attempt.request.task_id,
        turn_task_id(&case.session.session_id, &case.turn.id).unwrap()
    );
    assert_eq!(prepared.attempt.request.role, Role::ProjectLeader);
    assert!(
        prepared
            .attempt
            .request
            .route
            .required_capabilities
            .is_empty()
    );
    assert!(prepared.attempt.request.previous_attempt.is_none());
    assert_eq!(prepared.attempt.connection, proof.connection);
    assert!(prepared.attempt.completion.is_none());
    assert_eq!(prepared.lease.state, LeaseState::Ready);
    assert_eq!(prepared.lease.project_id, case.session.project_id);
    assert_eq!(prepared.lease.task_id, case.session.bootstrap_task_id);
    assert_eq!(prepared.lease.owner_session, case.session.session_id);
    assert_eq!(prepared.cwd, PathBuf::from(&prepared.lease.path));
    assert_eq!(prepared.repository, case.repository);
    assert!(prepared.cwd.is_dir());
    assert!(prepared.cwd.join(".git").is_file());
    {
        let mut db = case.db();
        let tx = db.transaction().unwrap();
        let auth = MasterBootstrapAuth::RcliConfigured(proof.clone());
        validate_bootstrap_auth(&tx, &prepared.session, &auth).unwrap();
        let (allocation, lease) =
            super::super::master_bootstrap::ready(&tx, &prepared.session, &auth).unwrap();
        assert_eq!(allocation, prepared.worktree_lease);
        assert_eq!(lease, prepared.lease);
        assert_eq!(
            model_gateway::read_attempt(&tx, &prepared.attempt.request.id).unwrap(),
            Some(prepared.attempt.clone())
        );
        let body: String = tx
            .query_row(
                "SELECT body FROM master_preparations WHERE session=?1 AND turn=?2",
                params![case.session.session_id.as_str(), case.turn.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let record: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(record["auth"], serde_json::to_value(&auth).unwrap());
        let state =
            model_gateway::connection_state(&tx, &proof.connection, now_ms().unwrap()).unwrap();
        assert_eq!(state.health, model_gateway::Health::Unknown);
        assert_eq!(state.quota, model_gateway::Quota::Unknown);
        assert_eq!(state.observed_at_ms, None);
    }
    let acquire = || MasterRuntimeCommand::AcquireProfile {
        host: case.started.host.clone(),
        project_id: case.session.project_id.clone(),
        session_id: case.session.session_id.clone(),
        turn_id: case.turn.id.clone(),
        attempt_id: prepared.attempt.request.id.clone(),
    };
    let MasterRuntimeResult::Profile(guard) = case.runtime(acquire()).await.unwrap() else {
        panic!("actual configured RCLI Execution guard missing");
    };
    let MasterRuntimeResult::Turn(duplicate) = case.runtime(acquire()).await.unwrap() else {
        panic!("duplicate acquisition must return the current turn");
    };
    assert_eq!(duplicate.phase, MasterTurnPhase::Delivering);
    assert_eq!(duplicate.attempt.as_ref(), Some(&prepared.attempt));
    assert_eq!(duplicate.runtime, None);
    assert_eq!(duplicate.reply, None);
    assert_eq!(duplicate.reason, None);
    drop(guard);
    let snapshot = case.snapshot();
    assert_eq!(snapshot.counts, [1, 1, 0, 0, 1, 1]);
    assert_eq!(
        std::fs::read(case.home.join(".backboard/config.json")).unwrap(),
        original_config
    );
    assert!(!case.home.join(".backboard/keys.json").exists());
    case.close().await;
}

/// Saves one account key in the fixture's engine folder, as the owner's key store does.
/// Inputs: the fixture, the key reference and the key.
fn save_key(case: &Fixture, reference: &str, key: &[u8]) {
    let keys = crate::storage::accounts::ACCOUNT_KEYS;
    let folder = case.root.path().join(keys);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    private_file(&folder.join(reference), key);
}

/// Tells whether a file, if it exists, holds some bytes. Inputs: the file and the bytes.
/// Output: the answer.
fn file_holds(path: &Path, needle: &[u8]) -> bool {
    std::fs::read(path)
        .is_ok_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
}

/// DEC-180 T20 Q7: a listed master's keyed pool account reaches a prepared turn and its
/// profile: the proof names that account's own custody slot under this engine run and
/// the session's settings, the attempt runs on that account, the turn's profile is the
/// slot's empty home, and the saved key is in no proof, record or database page.
#[tokio::test]
async fn a_listed_master_turn_is_prepared_on_its_own_keyless_custody_slot() {
    let case = Fixture::listed().await;
    save_key(&case, "listed-key", LISTED_KEY);
    let result = case.runtime(case.prepare()).await;
    let ready = matches!(&result, Ok(MasterRuntimeResult::Prepared(_)));
    assert!(ready, "{:?}", result.as_ref().err());
    let Ok(MasterRuntimeResult::Prepared(prepared)) = result else {
        unreachable!();
    };
    let (auth, body) = {
        let mut db = case.db();
        let tx = db.transaction().unwrap();
        let body: String = tx
            .query_row(
                "SELECT body FROM master_preparations WHERE session=?1 AND turn=?2",
                params![case.session.session_id.as_str(), case.turn.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let record: serde_json::Value = serde_json::from_str(&body).unwrap();
        let auth: MasterBootstrapAuth = serde_json::from_value(record["auth"].clone()).unwrap();
        assert!(validate_bootstrap_auth(&tx, &prepared.session, &auth).is_ok());
        let mut other = auth.clone();
        if let MasterBootstrapAuth::Listed(proof) = &mut other {
            proof.session_id = SessionId::new("other-session").unwrap();
        }
        assert!(validate_bootstrap_auth(&tx, &prepared.session, &other).is_err());
        (auth, body)
    };
    let MasterBootstrapAuth::Listed(proof) = &auth else {
        panic!("a listed master's turn must be proved by its listed account");
    };
    let proof: &ListedAccountProof = proof;
    assert_eq!(proof.session_id, case.session.session_id);
    assert_eq!(proof.database_instance, case.started.host.db_instance);
    assert_eq!(proof.engine_epoch, case.started.host.engine_epoch);
    assert_eq!(proof.config_id, case.config.id);
    assert_eq!(proof.source_hash, case.session.source_hash);
    assert_eq!(proof.connection.harness, reference("standin-acp"));
    let key_ref = reference("listed-key");
    let keyed = ModelAuthentication::ApiKey {
        credential_ref: key_ref,
    };
    assert_eq!(proof.connection.auth, keyed);
    assert_eq!(proof.custody_id.len(), 64);
    assert_eq!(prepared.attempt.connection, proof.connection);
    assert_eq!(prepared.profile.binding(), &auth.binding());
    assert!(body.contains("listed-key"));
    let acquire = MasterRuntimeCommand::AcquireProfile {
        host: case.started.host.clone(),
        project_id: case.session.project_id.clone(),
        session_id: case.session.session_id.clone(),
        turn_id: case.turn.id.clone(),
        attempt_id: prepared.attempt.request.id.clone(),
    };
    let acquired = case.runtime(acquire).await;
    let held = matches!(&acquired, Ok(MasterRuntimeResult::Profile(_)));
    assert!(held, "{:?}", acquired.as_ref().err());
    let Ok(MasterRuntimeResult::Profile(guard)) = acquired else {
        unreachable!();
    };
    let home = guard.profile_path().unwrap();
    assert!(home.is_dir());
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
    drop(guard);
    let wal = case.path.with_extension("db-wal");
    for page in [&case.path, &wal] {
        assert!(!file_holds(page, LISTED_KEY), "{}", page.display());
    }
    assert!(!body.contains(std::str::from_utf8(LISTED_KEY).unwrap()));
    case.close().await;
}

/// DEC-180 T20 Q7: a listed master keeps its conversation when settings outside it
/// change, as the other masters do: the next send moves the session to the new settings
/// and its turn is prepared on the same account under them.
#[tokio::test]
async fn a_listed_master_keeps_its_conversation_after_a_settings_change_outside_it() {
    let case = Fixture::listed().await;
    save_key(&case, "listed-key", LISTED_KEY);
    let (before, prepare) = rebind_after_outside_edit(&case).await;
    let (session, _) = case.view().await;
    assert_eq!(session.session_id, case.session.session_id);
    assert_eq!(session.rebound.len(), 1);
    assert_ne!(session.binding().0, &before);
    let result = case.runtime(prepare).await;
    let ready = matches!(&result, Ok(MasterRuntimeResult::Prepared(_)));
    assert!(ready, "{:?}", result.as_ref().err());
    let Ok(MasterRuntimeResult::Prepared(second)) = result else {
        unreachable!();
    };
    assert_eq!(&second.attempt.config_id, session.binding().0);
    let bound = second.profile.binding();
    assert_eq!(&bound.config_id, session.binding().0);
    assert_eq!(bound.source_hash, session.binding().1);
    assert_eq!(bound.connection, second.attempt.connection);
    case.close().await;
}

/// PA-5 (DEC-141): an RCLI master turn starts with the startup instruction every master
/// of the project gets, the tools-off one since an RCLI master has no tools: the project
/// context, approved memories, skill and MCP names and the landing rules.
#[tokio::test]
async fn an_rcli_master_turn_gets_the_startup_instruction_every_master_gets() {
    let case = Fixture::new(true).await;
    case.register().await;
    let MasterRuntimeResult::Prepared(prepared) = case.runtime(case.prepare()).await.unwrap()
    else {
        panic!("registered local RCLI must prepare its turn");
    };
    assert!(!prepared.tools);
    let expected = {
        let mut db = case.db();
        let tx = db.transaction().unwrap();
        crate::launch_context::for_master(
            &tx,
            &case.session.project_id,
            None,
            case.config.settings.model_access.as_ref(),
        )
        .unwrap()
    };
    assert!(expected.ends_with(crate::launch_context::MASTER_LANDING_RULES));
    assert_eq!(prepared.instructions, expected);
    case.close().await;
}

/// Writes the engine-generated SDK home of one RCLI key, as account setup leaves it.
/// Inputs: the fixture, the key and its endpoint. Output: the home's path.
fn generated_home(case: &Fixture, connection: &ModelConnection, base_url: &str) -> PathBuf {
    let profile = super::super::account_profile::configured_profile_ref(connection).unwrap();
    let homes = case.root.path().join("configured-sdk-homes");
    if !homes.exists() {
        std::fs::create_dir(&homes).unwrap();
        std::fs::set_permissions(&homes, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let home = homes.join(format!("{:x}", Sha256::digest(profile.as_str().as_bytes())));
    let sdk = home.join(".backboard");
    std::fs::create_dir_all(&sdk).unwrap();
    for directory in [&home, &sdk] {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    private_file(
        &sdk.join("config.json"),
        &serde_json::to_vec(&serde_json::json!({
            "model": {"provider": "local-provider", "model": "local-model"},
            "providers": [{"id": "local-provider", "name": "Local provider",
                "protocol": "openai-chat", "baseUrl": base_url,
                "auth": {"type": "none"}}]
        }))
        .unwrap(),
    );
    home
}

/// Writes the SDK home account setup generated for a keyed RCLI key before DEC-150
/// addendum 9a: the engine's own config naming the builtin provider id, and no profile
/// slot, as a save refused at its profile step left it. Inputs: the fixture, the key
/// and the model id the config names. Output: the config's path.
fn legacy_generated_home(case: &Fixture, connection: &ModelConnection, model: &str) -> PathBuf {
    let profile = super::super::account_profile::configured_profile_ref(connection).unwrap();
    let homes = case.root.path().join("configured-sdk-homes");
    if !homes.exists() {
        std::fs::create_dir(&homes).unwrap();
        std::fs::set_permissions(&homes, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let home = homes.join(format!("{:x}", Sha256::digest(profile.as_str().as_bytes())));
    let sdk = home.join(".backboard");
    std::fs::create_dir_all(&sdk).unwrap();
    for directory in [&home, &sdk] {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let ModelEndpoint::Http { url } = &connection.endpoint else {
        panic!("a keyed RCLI key has an HTTP endpoint");
    };
    let config = sdk.join("config.json");
    private_file(
        &config,
        &serde_json::to_vec(&serde_json::json!({
            "model": { "provider": connection.provider.as_str(), "model": model },
            "providers": [{
                "id": connection.provider.as_str(), "name": "Configured provider",
                "protocol": connection.protocol.as_str(), "baseUrl": url,
                "auth": { "type": "apiKey" }
            }]
        }))
        .unwrap(),
    );
    config
}

/// Queues the fixture session's next turn and names it. Inputs: the fixture and the
/// send's id. Output: the queued turn's prepare command.
async fn next_turn(case: &Fixture, id: &str) -> MasterRuntimeCommand {
    assert_eq!(
        case.result(case.submit(id, PROMPT)).await,
        OwnerResult::Applied
    );
    let (_, turns) = case.view().await;
    let queued = turns
        .into_iter()
        .find(|turn| turn.phase == MasterTurnPhase::Queued)
        .expect("the send queues the next turn");
    MasterRuntimeCommand::Prepare {
        host: case.started.host.clone(),
        project_id: case.session.project_id.clone(),
        session_id: case.session.session_id.clone(),
        turn_id: queued.id,
    }
}

/// Settles the fixture's first turn, has the owner add an account outside the master's
/// pool and sends again, which rebinds the session (PA-6, DEC-092). Input: a registered
/// fixture. Output: the config id before the edit and the next turn's prepare command.
async fn rebind_after_outside_edit(case: &Fixture) -> (ConfigId, MasterRuntimeCommand) {
    let MasterRuntimeResult::Prepared(first) = case.runtime(case.prepare()).await.unwrap() else {
        panic!("the registered key must prepare the first turn");
    };
    fail_first(case, &first, MasterReason::RuntimeUnavailable).await;
    let before = add_outside_account(case).await;
    let prepare = next_turn(case, "after-settings-change").await;
    (before, prepare)
}

/// Has the owner add an account outside the master's pool, which moves the settings
/// binding and leaves the master's scope as it was, so the next send rebinds the
/// session (PA-6, DEC-092). Input: the fixture. Output: the config id before the edit.
async fn add_outside_account(case: &Fixture) -> ConfigId {
    let basis = owner_read(
        &case.storage,
        &case.started,
        OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Pools,
            cursor: None,
            limit: 1,
        },
    )
    .await;
    let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
        panic!("actual model source basis missing");
    };
    owner_apply(
        &case.storage,
        &case.started,
        "add-outside-account",
        OwnerMutation::ModelAccess {
            expected_config_id: basis.config_id.clone(),
            expected_source_hash: source_hash,
            edit: crate::config::ModelAccessEdit::AddAccount {
                account_ref: reference("outside-account"),
            },
        },
    )
    .await;
    basis.config_id
}

/// Moves the fixture's configured home to its account's generated SDK home, which the
/// engine registers at the first turn. Input: the fixture. Output: the path of the config
/// file in that home.
fn generated_master_config(case: &Fixture) -> std::path::PathBuf {
    let connection = case
        .config
        .settings
        .model_access
        .as_ref()
        .unwrap()
        .connections[0]
        .clone();
    let profile_ref = super::super::account_profile::configured_profile_ref(&connection).unwrap();
    let homes = case.root.path().join("configured-sdk-homes");
    std::fs::create_dir(&homes).unwrap();
    std::fs::set_permissions(&homes, std::fs::Permissions::from_mode(0o700)).unwrap();
    let generated = homes.join(format!(
        "{:x}",
        Sha256::digest(profile_ref.as_str().as_bytes())
    ));
    std::fs::rename(&case.home, &generated).unwrap();
    generated.join(".backboard/config.json")
}

/// Writes other bytes over a config file and puts its bytes and modified time back, as
/// a confined master can, so only its change time moves (rv-sec3 G4 r2 F1). Input: the
/// config file. Output: none; it asserts the same file and bytes with a new change time.
fn write_and_restore_config(config: &Path) {
    use std::os::unix::fs::MetadataExt;
    let bytes = std::fs::read(config).unwrap();
    let before = std::fs::symlink_metadata(config).unwrap();
    std::fs::write(config, b"{}").unwrap();
    std::fs::write(config, &bytes).unwrap();
    std::fs::File::options()
        .write(true)
        .open(config)
        .unwrap()
        .set_modified(before.modified().unwrap())
        .unwrap();
    let after = std::fs::symlink_metadata(config).unwrap();
    assert_eq!(std::fs::read(config).unwrap(), bytes);
    assert_eq!(
        (after.dev(), after.ino(), after.mode(), after.nlink()),
        (before.dev(), before.ino(), before.mode(), before.nlink())
    );
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    assert_ne!(
        (after.ctime(), after.ctime_nsec()),
        (before.ctime(), before.ctime_nsec())
    );
}

/// Replaces a config file with a copy of the same bytes and mode, a new inode, as a
/// save through a rename does. Input: the config file. Output: none; it asserts the
/// new inode.
fn replace_config(config: &Path) {
    use std::os::unix::fs::MetadataExt;
    let before = std::fs::symlink_metadata(config).unwrap();
    let copy = config.with_extension("json.copy");
    std::fs::write(&copy, std::fs::read(config).unwrap()).unwrap();
    std::fs::set_permissions(
        &copy,
        std::fs::Permissions::from_mode(before.mode() & 0o7777),
    )
    .unwrap();
    std::fs::rename(&copy, config).unwrap();
    assert_ne!(
        std::fs::symlink_metadata(config).unwrap().ino(),
        before.ino()
    );
}

/// Runs the preparation of a master turn that must be refused. Inputs: the fixture, the
/// prepare command and what the turn follows, named in the panic. Output: the refusal's
/// cause, from a refused turn or from the error, or None.
async fn refused_cause(
    case: &Fixture,
    prepare: MasterRuntimeCommand,
    after: &str,
) -> Option<String> {
    match case.runtime(prepare).await {
        Ok(MasterRuntimeResult::Turn(turn)) => turn.cause,
        Ok(MasterRuntimeResult::Prepared(prepared)) => panic!(
            "a turn after {after} must not prepare {:?}",
            prepared.attempt.connection
        ),
        Ok(_) => None,
        Err(failure) => Some(failure.0),
    }
}

/// Checks the generation the fixture's master registration records. Inputs: the
/// fixture and the generation it must hold. Output: none; it asserts.
fn assert_master_generation(case: &Fixture, generation: u64) {
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    let record = registration(
        &tx,
        &case.session.session_id,
        &case.session.selection.target_profile_ref,
    )
    .unwrap()
    .unwrap();
    assert_eq!(record.proof.generation, generation);
}

/// Ends the fixture's first turn with a failure the runtime reported. Inputs: the
/// fixture, the prepared turn and the reason. Output: none; the turn is Failed.
async fn fail_first(case: &Fixture, prepared: &PreparedMasterTurn, reason: MasterReason) {
    case.runtime(MasterRuntimeCommand::Observe {
        host: case.started.host.clone(),
        project_id: case.session.project_id.clone(),
        session_id: case.session.session_id.clone(),
        turn_id: case.turn.id.clone(),
        attempt_id: prepared.attempt.request.id.clone(),
        observation: MasterRuntimeObservation::Failed {
            reason,
            cause: "gateway_rate_limited".into(),
        },
    })
    .await
    .unwrap();
    let (_, turns) = case.view().await;
    let first = turns.iter().find(|turn| turn.id == case.turn.id).unwrap();
    assert_eq!(first.phase, MasterTurnPhase::Failed);
    assert_eq!(first.reason, Some(reason));
}

include!("rcli_configured_rows.rs");
