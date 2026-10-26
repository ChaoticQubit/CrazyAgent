use super::*;
use crate::accounts::AccountStatus;
use crate::engine::bootstrap::EngineService;
use std::os::unix::fs::PermissionsExt;

const CATALOG: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-z"]
[[model_access.connections]]
id = "connection-z"
provider = "provider-z"
harness = "claude"
protocol = "native"
account_ref = "account-z"
endpoint = { kind = "native" }
model = { id = "model-z", author = "author-z" }
auth = { method = "native_managed", profile_ref = "profile-z" }
[[model_access.pools]]
id = "pool-z"
members = ["connection-z"]
projects = ["project-z"]
roles = ["leader"]
fallback_allowed = false
"#;

const TWO: &str = r#"
timeout_ms = 1000
[model_access]
accounts = ["account-y", "account-z"]
[[model_access.connections]]
id = "connection-y"
provider = "provider-y"
harness = "claude"
protocol = "native"
account_ref = "account-y"
endpoint = { kind = "native" }
model = { id = "model-y", author = "author-y" }
auth = { method = "native_managed", profile_ref = "profile-y" }
[[model_access.connections]]
id = "connection-z"
provider = "provider-z"
harness = "claude"
protocol = "native"
account_ref = "account-z"
endpoint = { kind = "native" }
model = { id = "model-z", author = "author-z" }
auth = { method = "native_managed", profile_ref = "profile-z" }
[[model_access.pools]]
id = "pool-z"
members = ["connection-y", "connection-z"]
projects = ["project-z"]
roles = ["leader"]
fallback_allowed = false
"#;

const CLAUDE: &str = r#"#!/bin/sh
case "$2" in
login) exit 0 ;;
logout)
  : > "$0.logged-out"
  exit 0 ;;
status)
  if [ -f "$0.logged-out" ]; then
    printf '{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty","configDirectory":"%s"}' "$CLAUDE_CONFIG_DIR"
    exit 1
  fi
  printf '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","configDirectory":"%s"}' "$CLAUDE_CONFIG_DIR"
  exit 0 ;;
esac
exit 2
"#;

struct Run {
    handle: OwnerRuntimeHandle,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), EngineError>>,
}

struct Restart {
    root: tempfile::TempDir,
    global: PathBuf,
    project: PathBuf,
    claude: PathBuf,
}

fn request(value: &str) -> RequestId {
    RequestId::new(value).unwrap()
}

fn connection() -> CredentialRef {
    CredentialRef::new("connection-z").unwrap()
}

fn custodian() -> PathBuf {
    let path = std::env::var_os("CRAZYAGENT_TEST_NATIVE_AUTH_HOST")
        .map(PathBuf::from)
        .expect("the test run supplies the built crazyagent custodian executable path");
    assert!(path.is_absolute());
    path
}

impl Restart {
    fn new() -> Self {
        Self::with(CATALOG)
    }

    fn with(catalog: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-rst-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("g.toml");
        std::fs::write(&global, catalog).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let claude = root.path().join("claude");
        std::fs::write(&claude, CLAUDE).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            project: root.path().join("p.toml"),
            root,
            global,
            claude,
        }
    }

    async fn start(&self) -> Run {
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let mut service = EngineService::start_with_settings_sources(
            self.root.path(),
            "e.sock",
            &self.global,
            &self.project,
            &stopped,
        )
        .await
        .unwrap();
        service.register_native_auth_host(&custodian()).unwrap();
        service.register_claude_executable(&self.claude).unwrap();
        let running = service.owner_runtime().unwrap();
        let handle = OwnerRuntimeHandle {
            engine: running.engine.clone(),
            authority: running.authority.clone(),
        };
        let task = tokio::spawn(service.run(stopped));
        Run { handle, stop, task }
    }

    fn log_out(&self) {
        std::fs::write(self.root.path().join("claude.logged-out"), b"").unwrap();
    }

    fn config(&self) -> crate::config::EffectiveConfig {
        crate::config::resolve_config(&self.global, &self.project).unwrap()
    }

    fn latest_phase(&self) -> NativeAuthPhase {
        let database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        let fact: Vec<u8> = database
            .query_row(
                "SELECT fact FROM native_auth_operations ORDER BY generation DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        crate::engine::owner::native_auth::decode_fact(&fact)
            .unwrap()
            .phase
    }

    fn launch(&self) -> Result<(), String> {
        let database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        let catalog = self.config().settings.model_access.unwrap();
        let connection = catalog
            .connections
            .iter()
            .find(|item| item.id == connection())
            .unwrap();
        crate::research::accounts::managed_sign_in(&database, connection).map(|_| ())
    }

    fn model(&self) -> crate::config::ModelConnection {
        self.model_of(&connection())
    }

    fn model_of(&self, id: &CredentialRef) -> crate::config::ModelConnection {
        self.config()
            .settings
            .model_access
            .unwrap()
            .connections
            .into_iter()
            .find(|item| &item.id == id)
            .unwrap()
    }

    fn hold(&self) {
        self.reject(&self.model());
        let database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        database.busy_timeout(Duration::from_secs(2)).unwrap();
        database
            .execute(
                "INSERT INTO team_paused_accounts(connection,until_ms) VALUES (?1,?2)
                 ON CONFLICT(connection) DO UPDATE SET until_ms=excluded.until_ms",
                rusqlite::params![connection().as_str(), now_ms() + 600_000],
            )
            .unwrap();
    }

    /// Records the provider's rejection of one account's sign-in as its newest gateway
    /// observation, with no pause. Input: the account's connection. Output: none.
    fn reject(&self, model: &crate::config::ModelConnection) {
        let now = now_ms();
        let config = self.config();
        let mut database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        database.busy_timeout(Duration::from_secs(2)).unwrap();
        let tx = database.transaction().unwrap();
        let sequence: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(sequence),0) FROM model_gateway_observations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        crate::model_gateway::observe(
            &tx,
            &config,
            &crate::model_gateway::Observation {
                id: request(&format!("restart-rejected-{sequence}")),
                connection: model.clone(),
                sequence: u64::try_from(sequence).unwrap() + 1,
                observed_at_ms: u64::try_from(now).unwrap(),
                valid_until_ms: u64::try_from(now).unwrap() + 60_000,
                kind: crate::model_gateway::ObservationKind::InvalidAuthentication,
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    /// Holds the account's current sign-in for a check before its next use, the way a
    /// master turn does when this engine could not hand out its credential. Input: the
    /// run the account was signed in on. Output: none.
    fn doubt(&self, run: &Run) {
        let mut database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        database.busy_timeout(Duration::from_secs(2)).unwrap();
        let tx = database.transaction().unwrap();
        crate::storage::doubt_sign_in(&tx, &run.handle.identity(), &self.model()).unwrap();
        tx.commit().unwrap();
    }

    /// Picks the research account for project-z from the engine's own settings, sign-ins
    /// and observations, the pick every research launch uses. Output: the pick.
    fn research_pick(&self) -> crate::research::accounts::AccountPick {
        let projects = self.root.path().join("projects");
        std::fs::create_dir_all(projects.join("project-z")).unwrap();
        crate::research::accounts::SettingsResolver::new(
            &self.global,
            &self.project,
            &projects,
            &self.root.path().join("engine.db"),
        )
        .unwrap()
        .resolve("project-z", "claude")
        .unwrap()
        .1
    }

    /// Opens the team runtime the engine binary runs beside the engine, over this engine's
    /// database and settings, for project-z: a Claude Code master and one Claude Code
    /// sub-agent. Output: the runtime and the project's stored setup.
    fn team(
        &self,
    ) -> (
        crate::team::runtime::TeamRuntime,
        crate::project_setup::StoredSetup,
    ) {
        let install = self.root.path().join("harness");
        std::fs::create_dir_all(&install).unwrap();
        let executable = install.join("claude");
        std::fs::write(&executable, CLAUDE).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let folder = self.root.path().join("team-project");
        std::fs::create_dir_all(&folder).unwrap();
        let setup = crate::project_setup::StoredSetup {
            project_id: ProjectId::new("project-z").unwrap(),
            name: "Team".into(),
            about: String::new(),
            tech_stack: vec![],
            platforms: vec![],
            research: vec![],
            pool: None,
            team: crate::project_setup::TeamPlan {
                master: "claude-code".into(),
                master_tools: false,
                sub_harnesses: vec![crate::project_setup::HarnessSlot {
                    harness: "claude-code".into(),
                    agents: 1,
                    tools: false,
                }],
                min_agents: 2,
                max_agents: 4,
            },
            documents: vec![],
            folder: folder.to_str().unwrap().into(),
            created_ms: 1,
            revision: 1,
        };
        let database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        database.busy_timeout(Duration::from_secs(2)).unwrap();
        database
            .execute(
                "INSERT INTO project_setups VALUES (?1,?2)",
                rusqlite::params![
                    setup.project_id.as_str(),
                    serde_json::to_string(&setup).unwrap()
                ],
            )
            .unwrap();
        let access = self.config().settings.model_access.unwrap();
        let team = crate::team::runtime::TeamRuntime::open(
            &self.root.path().join("engine.db"),
            &self.root.path().join("team"),
            std::collections::BTreeMap::from([("claude-code", executable)]),
            Box::new(move || Some(access.clone())),
            crate::team::runtime::TeamLimits::default(),
        )
        .unwrap();
        (team, setup)
    }

    /// Applies one owner team action for project-z the way the engine does.
    /// Input: the action. Output: none; the action must apply.
    fn team_action(&self, action: crate::team::TeamAction) {
        let mut database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        database.busy_timeout(Duration::from_secs(2)).unwrap();
        let access = self.config().settings.model_access.unwrap();
        let tx = database.transaction().unwrap();
        assert_eq!(
            crate::team::execute(&tx, &access, &action).unwrap(),
            crate::protocol::Outcome::Applied
        );
        tx.commit().unwrap();
    }

    /// Lists project-z's team agents of one role, oldest first.
    /// Input: the role name. Output: each agent's connection, status and error.
    fn team_agents(&self, role: &str) -> Vec<(String, String, Option<String>)> {
        let database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        let mut statement = database
            .prepare(
                "SELECT connection,status,error FROM team_agents WHERE project='project-z' AND role=?1 ORDER BY id",
            )
            .unwrap();
        statement
            .query_map([role], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// Holds the account the way a running agent or research run does, with a shared
    /// execution lease on its profile, until the lease is dropped. Inputs: the run the
    /// account was signed in on, the sign-in and a launch name. Output: the held lease.
    fn agent(
        &self,
        run: &Run,
        signed: &NativeAuthFact,
        name: &str,
    ) -> crate::engine::bootstrap::NativeProfileUse {
        let path = self.root.path().join("engine.db");
        crate::engine::NativeProfileCustody::capture(&path, &std::fs::metadata(&path).unwrap())
            .unwrap()
            .acquire_use(
                &run.handle.identity(),
                &signed.binding,
                &request(name),
                1,
                crate::engine::bootstrap::NativeProfileUsePurpose::Execution,
            )
            .unwrap()
    }

    fn held(&self) -> (bool, crate::model_gateway::Health) {
        let database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        let paused = database
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM team_paused_accounts WHERE connection=?1)",
                [connection().as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let state = crate::model_gateway::connection_state(
            &database,
            &self.model(),
            u64::try_from(now_ms()).unwrap(),
        )
        .unwrap();
        (paused, state.health)
    }
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

impl Run {
    fn client(&self) -> OwnerClient {
        OwnerClient::attach(
            &self.handle.authority.root.path,
            OwnerClientKind::Tui,
            Duration::from_secs(2),
        )
        .unwrap()
    }

    async fn scope(&self, config_id: ConfigId) -> NativeAuthScope {
        self.scope_of(config_id, connection()).await
    }

    async fn scope_of(&self, config_id: ConfigId, id: CredentialRef) -> NativeAuthScope {
        let response = self
            .client()
            .request(OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: request("restart-source"),
                expected_revision: Cursor::new(0),
                operation: OwnerOperation::Read {
                    query: OwnerRead::ModelAccessRecord {
                        collection: ModelAccessCollection::Connections,
                        id: id.clone(),
                    },
                },
            })
            .await
            .unwrap();
        let OwnerResponse::Read { state } = response.response else {
            panic!("the engine did not return the connection source")
        };
        let OwnerData::ModelAccessRecord { source_hash, .. } = state.data else {
            panic!("the engine did not return the connection record")
        };
        NativeAuthScope {
            expected_database_instance: self.handle.identity().db_instance,
            connection: id,
            expected_config_id: config_id,
            expected_source_hash: source_hash,
        }
    }

    /// Creates project-z through the owner, as Add project does, so the engine knows the
    /// project its team agents run for. Output: none; the engine must apply it.
    async fn create_project(&self) {
        let client = self.client();
        let read = client
            .request(OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: request("restart-capabilities"),
                expected_revision: Cursor::new(0),
                operation: OwnerOperation::Read {
                    query: OwnerRead::Capabilities,
                },
            })
            .await
            .unwrap();
        let OwnerResponse::Read { state } = read.response else {
            panic!("the engine did not return its capabilities")
        };
        let reply = client
            .request(OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: request("restart-create-project"),
                expected_revision: state.revision,
                operation: OwnerOperation::Apply {
                    mutation: crate::engine::owner::OwnerMutation::CreateProject {
                        project_id: ProjectId::new("project-z").unwrap(),
                        spec: crate::protocol::ProjectSpec {
                            name: "Team".into(),
                            repositories: vec![],
                        },
                    },
                },
            })
            .await
            .unwrap();
        let OwnerResponse::Applied { operation } = reply.response else {
            panic!(
                "the engine did not create the project: {:?}",
                reply.response
            )
        };
        assert_eq!(operation.result, crate::engine::owner::OwnerResult::Applied);
    }

    async fn sign_in(&self, restart: &Restart) -> NativeAuthFact {
        self.sign_in_to(restart, connection(), "restart-sign-in")
            .await
    }

    async fn sign_in_to(&self, restart: &Restart, id: CredentialRef, name: &str) -> NativeAuthFact {
        self.authenticate(restart, id, name, NativeAuthMethod::ClaudeOfficial)
            .await
    }

    /// Runs one real sign-in operation on the account through the owner and follows it
    /// until it finishes authenticated. Inputs: the case, the account, the request name
    /// and the method, a sign-in or a check of the existing one. Output: the final fact.
    async fn authenticate(
        &self,
        restart: &Restart,
        id: CredentialRef,
        name: &str,
        method: NativeAuthMethod,
    ) -> NativeAuthFact {
        let fact = self.finished(restart, id, name, method).await;
        assert_eq!(fact.phase, NativeAuthPhase::AuthenticatedObserved);
        fact
    }

    /// Runs one real sign-in operation on the account through the owner and follows it
    /// until it finishes, whatever it observed. Inputs: the case, the account, the request
    /// name and the method. Output: the final fact.
    async fn finished(
        &self,
        restart: &Restart,
        id: CredentialRef,
        name: &str,
        method: NativeAuthMethod,
    ) -> NativeAuthFact {
        let scope = self.scope_of(restart.config().id, id).await;
        let client = self.client();
        let started = client
            .start_native_auth(NativeAuthStart {
                request_id: request(name),
                scope: scope.clone(),
                method,
            })
            .await
            .unwrap();
        for _ in 0..200 {
            let read = client
                .read_native_auth(NativeAuthQuery {
                    request_id: request(&format!("{name}-read")),
                    scope: scope.clone(),
                    operation_id: started.fact.operation_id.clone(),
                })
                .await
                .unwrap();
            if !read.fact.phase.unfinished() {
                return read.fact;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the real Claude sign-in never finished");
    }

    async fn log_out(&self, restart: &Restart) -> NativeAuthFact {
        let scope = self.scope(restart.config().id).await;
        let client = self.client();
        let started = client
            .logout_native_auth(NativeAuthLogout {
                request_id: request("restart-logout"),
                scope: scope.clone(),
            })
            .await
            .unwrap();
        for _ in 0..200 {
            let read = client
                .read_native_auth(NativeAuthQuery {
                    request_id: request("restart-logout-read"),
                    scope: scope.clone(),
                    operation_id: started.fact.operation_id.clone(),
                })
                .await
                .unwrap();
            if !read.fact.phase.unfinished() {
                assert_eq!(read.fact.action, NativeAuthAction::Logout);
                assert_eq!(read.fact.phase, NativeAuthPhase::UnauthenticatedObserved);
                return read.fact;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the real Claude logout never finished");
    }

    async fn status(&self) -> AccountStatus {
        self.account().await.status
    }

    async fn account(&self) -> crate::accounts::AccountView {
        let page = self.client().list_accounts(None, 50).await.unwrap();
        page.accounts
            .into_iter()
            .find(|account| account.id == connection())
            .expect("the configured account is listed")
    }

    async fn stop(self) {
        self.stop.send_replace(true);
        let Run { handle, task, .. } = self;
        drop(handle);
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

async fn settled(restart: &Restart, run: &Run, wanted: AccountStatus) -> Result<(), String> {
    for _ in 0..150 {
        let launch = restart.launch();
        if launch != Err("master_authentication_stale".into()) {
            assert_eq!(run.status().await, wanted);
            return launch;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the earlier sign-in was never re-proved on the new engine run");
}

async fn signed_in_then_restarted(restart: &Restart) -> Run {
    let first = restart.start().await;
    let fact = first.sign_in(restart).await;
    assert_eq!(first.status().await, AccountStatus::SignedIn);
    assert_eq!(restart.launch(), Ok(()));
    assert_eq!(
        fact.binding.engine_epoch,
        first.handle.identity().engine_epoch
    );
    let old_epoch = first.handle.identity().engine_epoch;
    first.stop().await;
    let second = restart.start().await;
    assert_ne!(second.handle.identity().engine_epoch, old_epoch);
    second
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_signed_in_account_launches_after_an_engine_restart_without_a_new_sign_in() {
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let second = signed_in_then_restarted(&restart).await;
        assert_eq!(
            settled(&restart, &second, AccountStatus::SignedIn).await,
            Ok(())
        );
        assert_eq!(
            restart.latest_phase(),
            NativeAuthPhase::AuthenticatedObserved
        );
        second.stop().await;
    })
    .await
    .expect("the restart case exceeded 60 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_profile_logged_out_between_runs_shows_not_signed_in_and_cannot_launch() {
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let first = restart.start().await;
        first.sign_in(&restart).await;
        first.stop().await;
        restart.log_out();
        let second = restart.start().await;
        assert_eq!(
            settled(&restart, &second, AccountStatus::NotSignedIn).await,
            Err("master_authentication_unknown".into())
        );
        assert_eq!(
            restart.latest_phase(),
            NativeAuthPhase::UnauthenticatedObserved
        );
        second.stop().await;
    })
    .await
    .expect("the logged-out restart case exceeded 60 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_that_cannot_launch_is_never_shown_signed_in() {
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let second = signed_in_then_restarted(&restart).await;
        for _ in 0..100 {
            let status = second.status().await;
            let launch = restart.launch();
            assert!(
                launch.is_ok() || status != AccountStatus::SignedIn,
                "the account shows signed in while launch refuses: {launch:?}"
            );
            if launch.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        second.stop().await;
    })
    .await
    .expect("the status case exceeded 60 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_logged_out_account_reads_as_not_signed_in_for_every_launch() {
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let run = restart.start().await;
        run.sign_in(&restart).await;
        assert_eq!(restart.launch(), Ok(()));
        run.log_out(&restart).await;
        assert_eq!(
            restart.launch(),
            Err("master_authentication_unknown".into())
        );
        assert_eq!(run.status().await, AccountStatus::NotSignedIn);
        run.stop().await;
    })
    .await
    .expect("the logout case exceeded 60 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_sign_in_lifts_the_pause_and_rejection_that_a_sign_in_check_keeps() {
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let first = restart.start().await;
        restart.hold();
        assert_eq!(
            restart.held(),
            (true, crate::model_gateway::Health::InvalidAuthentication)
        );
        first.sign_in(&restart).await;
        assert_eq!(
            restart.held(),
            (false, crate::model_gateway::Health::Unknown)
        );
        restart.hold();
        first.stop().await;
        let second = restart.start().await;
        assert_eq!(
            settled(&restart, &second, AccountStatus::NotSignedIn).await,
            Err("master_sign_in_rejected".into())
        );
        assert_eq!(
            restart.held(),
            (true, crate::model_gateway::Health::InvalidAuthentication)
        );
        second.stop().await;
    })
    .await
    .expect("the sign-in hold case exceeded 60 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_account_reads_rejected_until_it_signs_in_again_on_the_same_account() {
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let run = restart.start().await;
        run.sign_in(&restart).await;
        assert_eq!(run.status().await, AccountStatus::SignedIn);
        restart.hold();
        let rejected = run.account().await;
        assert_eq!(
            (rejected.status, rejected.status_label.as_str()),
            (AccountStatus::NotSignedIn, "Sign-in rejected")
        );
        run.authenticate(
            &restart,
            connection(),
            "rejected-check",
            NativeAuthMethod::ClaudeExisting,
        )
        .await;
        assert_eq!(run.account().await, rejected);
        run.sign_in_to(&restart, connection(), "rejected-sign-in-again")
            .await;
        let again = run.account().await;
        assert_eq!(
            (again.id, again.status, again.status_label.as_str()),
            (connection(), AccountStatus::SignedIn, "Signed in")
        );
        assert_eq!(
            restart.held(),
            (false, crate::model_gateway::Health::Unknown)
        );
        run.stop().await;
    })
    .await
    .expect("the rejected account case exceeded 60 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_still_in_use_when_the_engine_starts_is_re_proved_once_it_is_free() {
    let restart = Restart::with(TWO);
    tokio::time::timeout(Duration::from_secs(60), async {
        let first = restart.start().await;
        let mut signed = Vec::new();
        for (id, name) in [("connection-y", "restart-y"), ("connection-z", "restart-z")] {
            signed.push(
                first
                    .sign_in_to(&restart, CredentialRef::new(id).unwrap(), name)
                    .await,
            );
        }
        let path = restart.root.path().join("engine.db");
        let mut custody =
            crate::engine::NativeProfileCustody::capture(&path, &std::fs::metadata(&path).unwrap())
                .unwrap();
        let launch = custody
            .acquire_use(
                &first.handle.identity(),
                &signed[0].binding,
                &request("restart-earlier-launch"),
                1,
                crate::engine::bootstrap::NativeProfileUsePurpose::Execution,
            )
            .unwrap();
        first.stop().await;
        let second = restart.start().await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        drop(launch);
        let database = rusqlite::Connection::open(&path).unwrap();
        let mut proved = Vec::new();
        for _ in 0..300 {
            proved = database
                .prepare(
                    "SELECT COUNT(*) FROM native_auth_operations
                     WHERE phase='authenticated_observed' GROUP BY profile_key ORDER BY profile_key",
                )
                .unwrap()
                .query_map([], |row| row.get::<_, i64>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            if proved == [2, 2] {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(
            proved,
            [2, 2],
            "both earlier sign-ins are re-proved in the new run, the held one once it is free"
        );
        second.stop().await;
    })
    .await
    .expect("the in-use restart case exceeded 60 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_an_agent_keeps_using_across_an_engine_start_is_re_proved_while_it_runs() {
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let first = restart.start().await;
        let signed = first.sign_in(&restart).await;
        let agent = restart.agent(&first, &signed, "restart-running-agent");
        first.stop().await;
        let second = restart.start().await;
        assert_eq!(
            settled(&restart, &second, AccountStatus::SignedIn).await,
            Ok(())
        );
        assert_eq!(
            restart.latest_phase(),
            NativeAuthPhase::AuthenticatedObserved
        );
        drop(agent);
        second.stop().await;
    })
    .await
    .expect("the running-agent restart case exceeded 60 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_check_of_an_account_an_agent_is_using_runs_beside_the_agent() {
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let run = restart.start().await;
        let signed = run.sign_in(&restart).await;
        let agent = restart.agent(&run, &signed, "check-running-agent");
        let checked = run
            .authenticate(
                &restart,
                connection(),
                "check-beside-agent",
                NativeAuthMethod::ClaudeExisting,
            )
            .await;
        assert_eq!(
            checked.action,
            NativeAuthAction::SignIn {
                method: NativeAuthMethod::ClaudeExisting
            }
        );
        assert_eq!(restart.launch(), Ok(()));
        assert_eq!(run.status().await, AccountStatus::SignedIn);
        drop(agent);
        run.stop().await;
    })
    .await
    .expect("the check-beside-agent case exceeded 60 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_whose_credential_the_engine_could_not_use_is_checked_before_its_next_use() {
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let run = restart.start().await;
        run.sign_in(&restart).await;
        assert_eq!(restart.launch(), Ok(()));
        restart.doubt(&run);
        let doubted = run.account().await;
        assert_eq!(
            (doubted.status, doubted.status_label.as_str()),
            (AccountStatus::NotSignedIn, "Not signed in")
        );
        assert_eq!(restart.launch(), Err("master_authentication_stale".into()));
        assert_eq!(
            restart.held(),
            (false, crate::model_gateway::Health::Unknown)
        );
        run.authenticate(
            &restart,
            connection(),
            "doubted-check",
            NativeAuthMethod::ClaudeExisting,
        )
        .await;
        assert_eq!(run.status().await, AccountStatus::SignedIn);
        assert_eq!(restart.launch(), Ok(()));
        run.stop().await;
    })
    .await
    .expect("the doubted account case exceeded 60 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn research_passes_over_an_account_whose_sign_in_was_rejected_to_the_next_one() {
    use crate::research::accounts::AccountPick;
    let restart = Restart::with(TWO);
    tokio::time::timeout(Duration::from_secs(60), async {
        let run = restart.start().await;
        let first = CredentialRef::new("connection-y").unwrap();
        run.sign_in_to(&restart, first.clone(), "research-sign-in-y")
            .await;
        run.sign_in(&restart).await;
        let picked = |pick: AccountPick| match pick {
            AccountPick::Account { connection, .. } => Some(connection),
            _ => None,
        };
        assert_eq!(picked(restart.research_pick()), Some(first.clone()));
        restart.reject(&restart.model_of(&first));
        assert_eq!(picked(restart.research_pick()), Some(connection()));
        restart.reject(&restart.model());
        assert!(matches!(
            restart.research_pick(),
            AccountPick::NoUsableAccount
        ));
        run.stop().await;
    })
    .await
    .expect("the research rejection case exceeded 60 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn team_agents_pass_over_an_account_whose_sign_in_was_rejected_to_the_next_one() {
    use crate::team::{MessageAuthor, MessageState, TaskStatus, TeamAction};
    let restart = Restart::with(TWO);
    tokio::time::timeout(Duration::from_secs(60), async {
        let run = restart.start().await;
        let first = CredentialRef::new("connection-y").unwrap();
        run.sign_in_to(&restart, first.clone(), "team-sign-in-y")
            .await;
        run.sign_in(&restart).await;
        restart.reject(&restart.model_of(&first));
        run.create_project().await;
        let (mut team, setup) = restart.team();
        let project = setup.project_id.clone();
        restart.team_action(TeamAction::KeepWarm {
            project_id: project.clone(),
            on: true,
        });
        team.tick().unwrap();
        assert_eq!(
            restart.team_agents("sub"),
            [("connection-z".to_owned(), "idle".to_owned(), None)],
            "the warm sub-agent starts on the account whose sign-in was not rejected"
        );
        restart.reject(&restart.model());
        restart.team_action(TeamAction::Message {
            project_id: project.clone(),
            text: "Plan the page".into(),
        });
        restart.team_action(TeamAction::Delegate {
            project_id: project.clone(),
            harness: None,
            prompt: "Write page.md".into(),
        });
        team.tick().unwrap();
        let database = rusqlite::Connection::open(restart.root.path().join("engine.db")).unwrap();
        let parked = crate::team::status(&database, &setup).unwrap();
        assert_eq!(
            parked
                .messages
                .iter()
                .map(|m| (m.author, m.state, m.text.as_str()))
                .collect::<Vec<_>>(),
            [
                (MessageAuthor::Owner, MessageState::Failed, "Plan the page"),
                (
                    MessageAuthor::Engine,
                    MessageState::Note,
                    "The master could not start: Sign in again: the provider rejected the sign-in of every account in this project's pool that can run claude-code."
                ),
            ]
        );
        assert_eq!(parked.tasks[0].status, TaskStatus::NeedsPool);
        assert!(restart.team_agents("master").is_empty());
        team.shutdown().unwrap();
        run.stop().await;
    })
    .await
    .expect("the team rejection case exceeded 60 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_team_agent_whose_account_sign_in_is_rejected_stops_and_the_next_account_takes_over()
 {
    use crate::team::TeamAction;
    let restart = Restart::with(TWO);
    tokio::time::timeout(Duration::from_secs(60), async {
        let run = restart.start().await;
        let first = CredentialRef::new("connection-y").unwrap();
        run.sign_in_to(&restart, first.clone(), "team-idle-sign-in-y")
            .await;
        run.sign_in(&restart).await;
        run.create_project().await;
        let (mut team, setup) = restart.team();
        restart.team_action(TeamAction::KeepWarm {
            project_id: setup.project_id.clone(),
            on: true,
        });
        team.tick().unwrap();
        assert_eq!(
            restart.team_agents("sub"),
            [("connection-y".to_owned(), "idle".to_owned(), None)]
        );
        restart.reject(&restart.model_of(&first));
        let mut agents = Vec::new();
        for _ in 0..100 {
            team.tick().unwrap();
            agents = restart.team_agents("sub");
            if agents
                .first()
                .is_some_and(|(_, status, _)| status == "stopped")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(
            agents,
            [
                ("connection-y".to_owned(), "stopped".to_owned(), None),
                ("connection-z".to_owned(), "idle".to_owned(), None),
            ],
            "the idle agent on the rejected account stops and the next account keeps the team warm"
        );
        team.shutdown().unwrap();
        run.stop().await;
    })
    .await
    .expect("the idle team agent rejection case exceeded 60 seconds");
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_existing_claude_sign_in_whose_token_lapsed_and_cannot_be_renewed_is_not_reported_signed_in()
 {
    let mut restart = Restart::new();
    restart.claude = crate::team::runtime::managed_agent::claude();
    tokio::time::timeout(Duration::from_secs(120), async {
        let run = restart.start().await;
        let empty = run
            .finished(
                &restart,
                connection(),
                "check-empty-profile",
                NativeAuthMethod::ClaudeExisting,
            )
            .await;
        assert_eq!(empty.phase, NativeAuthPhase::UnauthenticatedObserved);
        let profiles = restart.root.path().join("native-profiles");
        let homes: Vec<PathBuf> = std::fs::read_dir(&profiles)
            .unwrap()
            .map(|entry| entry.unwrap().path().join("home"))
            .filter(|home| home.is_dir())
            .collect();
        assert_eq!(homes.len(), 1, "{homes:?}");
        let home = &homes[0];
        let keychain = crate::credentials::test_keychain::TestKeychain::adopt(
            home,
            home.join("Library/Keychains/login.keychain-db"),
        );
        let service = format!(
            "Claude Code-credentials-{}",
            &crate::memory::hash(home.to_str().unwrap().as_bytes())[..8]
        );
        keychain.add(
            "unknown",
            &service,
            &serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "sk-ant-oat01-reproof-unit",
                    "refreshToken": "sk-ant-ort01-reproof-unit",
                    "expiresAt": now_ms() - 60_000,
                    "scopes": ["user:inference", "user:profile"],
                    "subscriptionType": "max"
                }
            })
            .to_string(),
        );
        let lapsed = run
            .finished(
                &restart,
                connection(),
                "check-lapsed-sign-in",
                NativeAuthMethod::ClaudeExisting,
            )
            .await;
        let stored: serde_json::Value = serde_json::from_str(&keychain.find(&service)).unwrap();
        assert_eq!(
            (lapsed.phase, lapsed.code.as_deref()),
            (NativeAuthPhase::UnauthenticatedObserved, None),
            "the check reported a lapsed sign-in that Claude could not renew; record expiresAt now {}",
            stored["claudeAiOauth"]["expiresAt"]
        );
        assert_eq!(
            stored["claudeAiOauth"]["expiresAt"],
            serde_json::json!(0),
            "the real claude never reached the token endpoint"
        );
        run.stop().await;
    })
    .await
    .expect("the lapsed Claude sign-in check exceeded 120 seconds");
}
