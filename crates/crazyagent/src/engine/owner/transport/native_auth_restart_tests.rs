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
    installed: bool,
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
        Self::case(catalog, None)
    }

    /// Opens a case on the installed Claude Code with no stand-in, so every sign-in, status
    /// and logout reply comes from the real harness and each sign-in waits for the owner to
    /// finish it in the browser. Output: the case, or a panic when Claude Code is missing.
    fn live() -> Self {
        let entries = crate::harnesses::catalog(&crate::harnesses::Search::from_environment());
        let entry = entries
            .iter()
            .find(|entry| entry.id == "claude-code")
            .unwrap();
        assert_eq!(
            entry.status,
            crate::harnesses::HarnessStatus::Ready,
            "UNAVAILABLE: Claude Code must be installed for the live restart re-check"
        );
        let claude = std::fs::canonicalize(entry.executable.as_ref().unwrap()).unwrap();
        Self::case(CATALOG, Some(claude))
    }

    /// Opens a case on a private temp root with the catalog and a Claude executable.
    /// Inputs: the catalog and the installed Claude Code, or none for the shell stand-in.
    /// Output: the case.
    fn case(catalog: &str, installed: Option<PathBuf>) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-rst-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("g.toml");
        std::fs::write(&global, catalog).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let live = installed.is_some();
        let claude = installed.unwrap_or_else(|| {
            let claude = root.path().join("claude");
            std::fs::write(&claude, CLAUDE).unwrap();
            std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o700)).unwrap();
            claude
        });
        Self {
            project: root.path().join("p.toml"),
            root,
            global,
            claude,
            installed: live,
        }
    }

    /// How many 100 ms reads a sign-in gets to finish: the stand-in answers at once, a real
    /// sign-in waits up to ten minutes for the owner in the browser.
    fn sign_in_reads(&self) -> u32 {
        if self.installed { 6000 } else { 200 }
    }

    async fn start(&self) -> Run {
        let (mut service, handle, stop) = self.ready().await;
        service.register_native_auth_host(&custodian()).unwrap();
        service.register_claude_executable(&self.claude).unwrap();
        Run::spawn(service, handle, stop)
    }

    /// Opens an engine run on this case's database without running it and without an
    /// account-check host, so it checks no account and makes no profile keychain.
    /// Output: the engine, its owner handle and its stop switch.
    async fn ready(
        &self,
    ) -> (
        EngineService,
        OwnerRuntimeHandle,
        tokio::sync::watch::Sender<bool>,
    ) {
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            self.root.path(),
            "e.sock",
            &self.global,
            &self.project,
            &stopped,
        )
        .await
        .unwrap();
        let running = service.owner_runtime().unwrap();
        let handle = OwnerRuntimeHandle {
            engine: running.engine.clone(),
            authority: running.authority.clone(),
        };
        (service, handle, stop)
    }

    /// Runs an engine with no account-check host: no sign-in is checked or re-proved on it.
    /// Output: the running engine.
    async fn start_bare(&self) -> Run {
        let (service, handle, stop) = self.ready().await;
        Run::spawn(service, handle, stop)
    }

    /// Tells whether the account's sign-in waits for a re-check of the run, read the way the
    /// team runtime reads it. Inputs: the run and the account. Output: the storage answer.
    fn rechecking(&self, run: &Run, model: &crate::config::ModelConnection) -> bool {
        let mut database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        database.busy_timeout(Duration::from_secs(2)).unwrap();
        let tx = database.transaction().unwrap();
        crate::storage::sign_in_rechecking(&tx, &run.handle.identity(), model).unwrap()
    }

    /// Counts the profile fences sign-in operations hold. Output: the count.
    fn fences(&self) -> i64 {
        rusqlite::Connection::open(self.root.path().join("engine.db"))
            .unwrap()
            .query_row("SELECT COUNT(*) FROM native_auth_fences", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    /// Opens the team runtime for project-z with the engine-start re-proof flag it waits on
    /// and a tmux host of this case for live masters. Input: the flag. Output: the runtime,
    /// the project's stored setup and the host.
    fn live_team(
        &self,
        reproving: Arc<std::sync::atomic::AtomicBool>,
    ) -> (
        crate::team::runtime::TeamRuntime,
        crate::project_setup::StoredSetup,
        Arc<dyn crate::terminal::Host>,
    ) {
        let (team, setup) = self.team();
        let base = self.root.path().canonicalize().unwrap();
        let tmux = crate::terminal::tmux::installed(&base)
            .expect("tmux must be installed on this machine");
        let host: Arc<dyn crate::terminal::Host> = Arc::new(crate::terminal::tmux::Tmux::open(
            Some(tmux),
            &base.join(crate::terminal::FOLDER),
        ));
        let team = team
            .with_reproof(reproving)
            .with_terminal(host.clone())
            .unwrap();
        (team, setup, host)
    }

    /// Reads project-z's live master switch and why it last could not start.
    /// Output: the switch, none when never set, and the reason.
    fn live_switch(&self) -> (Option<bool>, Option<String>) {
        let database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        crate::team::master::switch(&database, &ProjectId::new("project-z").unwrap()).unwrap()
    }

    /// Lists project-z's team messages in order. Input: its stored setup. Output: each
    /// message's author, state and text.
    fn messages(
        &self,
        setup: &crate::project_setup::StoredSetup,
    ) -> Vec<(
        crate::team::MessageAuthor,
        crate::team::MessageState,
        String,
    )> {
        let database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        crate::team::status(&database, setup)
            .unwrap()
            .messages
            .into_iter()
            .map(|message| (message.author, message.state, message.text))
            .collect()
    }

    /// Reads project-z's team task states in order. Input: its stored setup. Output: each
    /// task's status.
    fn tasks(&self, setup: &crate::project_setup::StoredSetup) -> Vec<crate::team::TaskStatus> {
        let database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        crate::team::status(&database, setup)
            .unwrap()
            .tasks
            .into_iter()
            .map(|task| task.status)
            .collect()
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
        database
            .execute(
                "INSERT INTO team_project_pauses(project,connection,until_ms) VALUES ('project-z',?1,?2)
                 ON CONFLICT(project,connection) DO UPDATE SET until_ms=excluded.until_ms",
                rusqlite::params![connection().as_str(), now_ms() + 600_000],
            )
            .unwrap();
    }

    /// Whether project-z still holds the account after a limit its own agent reported
    /// (SECAUDIT-RATELIMIT-FORGE). Output: the answer.
    fn project_held(&self) -> bool {
        let database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        database
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM team_project_pauses WHERE project='project-z' AND connection=?1)",
                [connection().as_str()],
                |row| row.get(0),
            )
            .unwrap()
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
        let executable = if self.installed {
            self.claude.clone()
        } else {
            let install = self.root.path().join("harness");
            std::fs::create_dir_all(&install).unwrap();
            let executable = install.join("claude");
            std::fs::write(&executable, CLAUDE).unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            executable
        };
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

    /// Reads the team settings version an owner change to a project must name, as a screen
    /// reads it from the team status. Input: the project. Output: the version.
    fn team_settings_version(&self, project: &crate::protocol::ProjectId) -> u64 {
        let database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        database.busy_timeout(Duration::from_secs(2)).unwrap();
        crate::team::settings_version(&database, project).unwrap()
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
    /// Runs an opened engine until its stop switch is set. Inputs: the engine, its owner
    /// handle and the switch. Output: the running engine.
    fn spawn(
        service: EngineService,
        handle: OwnerRuntimeHandle,
        stop: tokio::sync::watch::Sender<bool>,
    ) -> Self {
        let task = tokio::spawn(service.run(stop.subscribe()));
        Run { handle, stop, task }
    }

    /// The admission of the owner registration this run holds active.
    /// Output: that admission.
    fn admission(&self) -> Admission {
        self.handle
            .authority
            .slots
            .lock()
            .unwrap()
            .iter()
            .find(|slot| slot.registration.active)
            .map(|slot| slot.registration.admission.clone())
            .expect("an active owner registration")
    }

    /// Begins one check of the account's existing sign-in through the engine's own storage
    /// job, the step that commits it and fences its profile before the account-check host
    /// runs it, and holds it there. Input: the case. Output: the admission and the held work.
    async fn begin_check(&self, restart: &Restart) -> (Admission, crate::storage::NativeAuthWork) {
        let admission = self.admission();
        let start = NativeAuthStart {
            request_id: request("restart-held-check"),
            scope: self.scope(restart.config().id).await,
            method: NativeAuthMethod::ClaudeExisting,
        };
        let command = crate::engine::owner::native_auth::Command::Start(start);
        let begun = self
            .handle
            .engine
            .native_auth_job(|reply| Job::NativeAuth(admission.clone(), command, reply))
            .await
            .unwrap();
        assert_eq!(begun.reply.fact.phase, NativeAuthPhase::Starting);
        (admission, begun.work.expect("the begun check's work"))
    }

    /// Ends a held check the way the engine ends one no account-check host ran.
    /// Inputs: its admission and work. Output: the final fact.
    async fn finish_check(
        &self,
        admission: Admission,
        work: crate::storage::NativeAuthWork,
    ) -> NativeAuthFact {
        self.handle
            .engine
            .native_auth_job(|reply| Job::NativeAuthFinish(admission, Box::new(work), reply))
            .await
            .unwrap()
            .fact
    }

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
        for _ in 0..restart.sign_in_reads() {
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
        assert!(restart.project_held());
        first.sign_in(&restart).await;
        assert_eq!(
            restart.held(),
            (false, crate::model_gateway::Health::Unknown)
        );
        assert!(
            !restart.project_held(),
            "a fresh sign-in lifts a project's hold on the account too"
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
        assert!(!restart.project_held());
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
async fn an_owner_check_during_a_renewal_of_an_account_in_use_waits_for_it_and_leaves_sign_in_free()
{
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(90), async {
        let run = restart.start().await;
        let signed = run.sign_in(&restart).await;
        let agent = restart.agent(&run, &signed, "renewing-agent");
        let renewal = agent.hold_renewal(Duration::ZERO).unwrap();
        let renewed = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            drop(renewal);
        });
        let checked = run
            .authenticate(
                &restart,
                connection(),
                "check-during-renewal",
                NativeAuthMethod::ClaudeExisting,
            )
            .await;
        renewed.await.unwrap();
        assert_eq!(
            checked.action,
            NativeAuthAction::SignIn {
                method: NativeAuthMethod::ClaudeExisting
            }
        );
        run.authenticate(
            &restart,
            connection(),
            "check-after-renewal",
            NativeAuthMethod::ClaudeExisting,
        )
        .await;
        assert_eq!(run.status().await, AccountStatus::SignedIn);
        drop(agent);
        run.stop().await;
    })
    .await
    .expect("the check-during-renewal case exceeded 90 seconds");
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

/// macOS only: by DEC-094 no team agent or agent pane runs on Linux, where the launch is
/// refused with `launch_unavailable_on_linux` (asserted in `harnesses::terminal_session::tests`).
#[cfg(target_os = "macos")]
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
            version: restart.team_settings_version(&project),
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

/// macOS only: by DEC-094 no team agent or agent pane runs on Linux, where the launch is
/// refused with `launch_unavailable_on_linux` (asserted in `harnesses::terminal_session::tests`).
#[cfg(target_os = "macos")]
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
            version: restart.team_settings_version(&setup.project_id),
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

/// Waits up to five seconds for the engine-start re-proof flag to clear.
/// Input: the flag. Output: true once it reads false.
async fn cleared(flag: &std::sync::atomic::AtomicBool) -> bool {
    for _ in 0..100 {
        if !flag.load(std::sync::atomic::Ordering::SeqCst) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

const CHECKING: &str = crate::team::master::SIGN_INS_CHECKING;

const SIGN_IN_TO_RUN: &str = "Sign in to an account in this project's pool to run claude-code";

/// DEC-108, storage: an account's sign-in waits for a re-check while a check of its profile
/// is begun and still holds the profile, and is settled when nothing ever signed it in, once
/// that check ended under this run, and for an account that is no managed profile. No
/// account-check host runs, so no profile keychain is made; the earlier-run case is
/// `sign_in_rechecking_due_after_restart`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sign_in_rechecking_running_and_settled() {
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let run = restart.start_bare().await;
        let model = restart.model();
        assert!(
            !restart.rechecking(&run, &model),
            "never signed in and nothing checks it"
        );
        let (admission, work) = run.begin_check(&restart).await;
        assert_eq!(restart.fences(), 1);
        assert!(
            restart.rechecking(&run, &model),
            "a begun check still holds the profile"
        );
        let ended = run.finish_check(admission, work).await;
        assert!(!ended.phase.unfinished(), "{:?}", ended.phase);
        assert_eq!(
            ended.binding.engine_epoch,
            run.handle.identity().engine_epoch
        );
        assert_eq!(restart.fences(), 0);
        assert!(
            !restart.rechecking(&run, &model),
            "a check that ended under this run is settled"
        );
        let endpoint = crate::config::ModelConnection {
            endpoint: crate::config::ModelEndpoint::Http {
                url: "http://127.0.0.1:11434/v1".into(),
            },
            auth: crate::config::ModelAuthentication::None {},
            ..model.clone()
        };
        assert!(
            !restart.rechecking(&run, &endpoint),
            "an endpoint account has no sign-in to re-check"
        );
        run.stop().await;
    })
    .await
    .expect("the re-check storage case exceeded 60 seconds");
}

/// DEC-108, storage: a sign-in proved under an earlier run waits for this run's re-check,
/// and is settled once proved under this run and once logged out under it. Needs a live
/// Claude account: the installed Claude Code answers every status, and the owner finishes
/// the real sign-in in the browser, which on macOS also makes a keychain in the temp profile.
/// Runs only by name with `--ignored` in an owner-approved live window.
#[ignore = "live: needs a live Claude account; the owner finishes a real sign-in through the installed Claude Code"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sign_in_rechecking_due_after_restart() {
    let restart = Restart::live();
    tokio::time::timeout(Duration::from_secs(900), async {
        let model = restart.model();
        let first = restart.start().await;
        first.sign_in(&restart).await;
        assert!(!restart.rechecking(&first, &model), "proved under this run");
        first.stop().await;
        let second = restart.start_bare().await;
        assert!(
            restart.rechecking(&second, &model),
            "proved under an earlier run and not re-checked on this one"
        );
        assert_eq!(restart.fences(), 0, "nothing checks it on this run");
        second.stop().await;
        let third = restart.start().await;
        assert_eq!(
            settled(&restart, &third, AccountStatus::SignedIn).await,
            Ok(())
        );
        assert!(
            !restart.rechecking(&third, &model),
            "re-proved under this run"
        );
        third.log_out(&restart).await;
        assert!(
            !restart.rechecking(&third, &model),
            "not signed in under this run"
        );
        third.stop().await;
    })
    .await
    .expect("the earlier-run re-check case exceeded 900 seconds");
}

/// N-E1: a real Claude sign-in whose custodian stops on an error while the owner is at the
/// browser settles Failed with the custodian's own cause, not still waiting and not the
/// general interruption. Once the installed Claude Code shows its sign-in link, the temp
/// profile's home folder is replaced, so the custodian's next profile check refuses it and the
/// custodian exits non-zero naming `native_profile_custody_unsafe`; the engine's release hold
/// does not read the home, so the attempt settles with that cause. Needs the installed Claude
/// Code; the link is never opened and no account is signed in. Runs only by name with
/// `--ignored` in an owner-approved live window.
#[ignore = "live: needs the installed Claude Code; a real sign-in runs until it shows its sign-in link"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_claude_sign_in_whose_custodian_fails_settles_with_its_cause() {
    let restart = Restart::live();
    tokio::time::timeout(Duration::from_secs(300), async {
        let run = restart.start().await;
        let scope = run.scope(restart.config().id).await;
        let client = run.client();
        let started = client
            .start_native_auth(NativeAuthStart {
                request_id: request("custodian-cause-sign-in"),
                scope: scope.clone(),
                method: NativeAuthMethod::ClaudeOfficial,
            })
            .await
            .unwrap();
        let query = NativeAuthQuery {
            request_id: request("custodian-cause-read"),
            scope: scope.clone(),
            operation_id: started.fact.operation_id.clone(),
        };
        let mut phase = started.fact.phase;
        for _ in 0..1200 {
            phase = client
                .read_native_auth(query.clone())
                .await
                .unwrap()
                .fact
                .phase;
            if phase == NativeAuthPhase::AwaitingOwner || !phase.unfinished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(
            phase,
            NativeAuthPhase::AwaitingOwner,
            "the real sign-in never showed its sign-in link"
        );
        let homes: Vec<PathBuf> = std::fs::read_dir(restart.root.path().join("native-profiles"))
            .unwrap()
            .map(|entry| entry.unwrap().path().join("home"))
            .filter(|home| home.is_dir())
            .collect();
        assert_eq!(homes.len(), 1, "{homes:?}");
        std::fs::rename(&homes[0], restart.root.path().join("replaced-home")).unwrap();
        std::fs::create_dir(&homes[0]).unwrap();
        std::fs::set_permissions(&homes[0], std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut settled = None;
        for _ in 0..1200 {
            let fact = client.read_native_auth(query.clone()).await.unwrap().fact;
            if !fact.phase.unfinished() {
                settled = Some(fact);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let fact = settled.expect("the sign-in kept waiting after its custodian failed");
        assert_eq!(
            (fact.phase, fact.code.as_deref()),
            (
                NativeAuthPhase::Failed,
                Some("native_profile_custody_unsafe")
            )
        );
        assert_eq!(restart.fences(), 0, "the failed custodian left a fence");
        run.stop().await;
    })
    .await
    .expect("the custodian-cause case exceeded 300 seconds");
}

/// DEC-108: the engine-start re-proof flag is true before the engine runs, and clears once
/// that re-proof ends: when it finished with nothing to re-check, when it gave up because no
/// owner registration admits a check, and when the engine stopped it while it still waited.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reproving_clears_when_reproof_ends() {
    use std::sync::atomic::Ordering::SeqCst;
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let (service, handle, stop) = restart.ready().await;
        let flag = handle.reproving();
        assert!(flag.load(SeqCst), "the flag is set before the engine runs");
        let run = Run::spawn(service, handle, stop);
        assert!(cleared(&flag).await, "a re-proof that finished clears it");
        run.stop().await;

        let (service, handle, stop) = restart.ready().await;
        let flag = handle.reproving();
        let registrations: Vec<Registration> = handle
            .authority
            .slots
            .lock()
            .unwrap()
            .iter()
            .map(|slot| slot.registration.clone())
            .collect();
        for registration in registrations {
            handle
                .authority
                .publish(
                    Registration {
                        active: false,
                        ..registration
                    },
                    handle.authority.next_control(),
                )
                .unwrap();
        }
        assert!(
            handle
                .authority
                .slots
                .lock()
                .unwrap()
                .iter()
                .all(|slot| !slot.registration.active)
        );
        assert!(flag.load(SeqCst));
        let run = Run::spawn(service, handle, stop);
        assert!(
            cleared(&flag).await,
            "a re-proof with no owner admission gives up and clears it"
        );
        run.stop().await;

        let (service, handle, stop) = restart.ready().await;
        let flag = handle.reproving();
        let authority = handle.authority.clone();
        let serial = authority.reproof.lock().await;
        let run = Run::spawn(service, handle, stop);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            flag.load(SeqCst),
            "a re-proof waiting for another one still runs"
        );
        run.stop().await;
        assert!(
            !flag.load(SeqCst),
            "the engine's stop aborts the waiting re-proof and clears it"
        );
        drop(serial);
    })
    .await
    .expect("the re-proof flag case exceeded 60 seconds");
}

/// DEC-108: while this run's re-proof runs and the only pool account's check still holds its
/// profile, the switched-on live master waits: over more ticks than it may start in its
/// window it stays on, starts nothing, and its problem line and one team note say it checks
/// sign-ins. Once the re-proof ended with no proof, it is turned off with the sign-in refusal.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_master_waits_for_restart_recheck() {
    use crate::team::{MessageAuthor, MessageState, TeamAction};
    use std::sync::atomic::Ordering::SeqCst;
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let run = restart.start_bare().await;
        run.create_project().await;
        let reproving = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (mut team, setup, _host) = restart.live_team(reproving.clone());
        restart.team_action(TeamAction::LiveMaster {
            project_id: setup.project_id.clone(),
            on: true,
            version: restart.team_settings_version(&setup.project_id),
        });
        let (admission, work) = run.begin_check(&restart).await;
        for _ in 0..5 {
            team.tick().unwrap();
        }
        assert_eq!(
            restart.live_switch(),
            (Some(true), Some(CHECKING.to_owned())),
            "the switch stays on and its problem line says it checks sign-ins"
        );
        assert_eq!(
            restart.messages(&setup),
            [(
                MessageAuthor::Engine,
                MessageState::Note,
                CHECKING.to_owned()
            )],
            "one note"
        );
        assert!(
            restart.team_agents("master").is_empty(),
            "no live start while it waits"
        );
        reproving.store(false, SeqCst);
        team.tick().unwrap();
        let (on, problem) = restart.live_switch();
        assert_eq!(on, Some(false), "{problem:?}");
        let problem = problem.expect("why it was turned off");
        assert!(
            problem.starts_with("The live master was turned off: it could not start: ")
                && problem.contains(SIGN_IN_TO_RUN),
            "{problem}"
        );
        assert!(restart.team_agents("master").is_empty());
        run.finish_check(admission, work).await;
        team.shutdown().unwrap();
        run.stop().await;
    })
    .await
    .expect("the live master wait case exceeded 60 seconds");
}

/// LMS-1: while another process holds the signed-in account's profile for a sign-in, the
/// started live master cannot be handed the account: over more ticks it stays on, its
/// problem line and one team note say the profile is in use. Once the profile is free it
/// is handed the account on the next tick and its wait line is cleared, and its terminal
/// then runs the stand-in Claude Code, which ends at once: a refused handoff fails the
/// master before its terminal starts. The stand-in sign-in stores no Claude record in the
/// profile keychain, so the test adds the one a real sign-in leaves there.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_master_says_it_waits_while_another_process_holds_its_profile() {
    use crate::team::{MessageAuthor, MessageState, TeamAction};
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let run = restart.start().await;
        let signed = run.sign_in(&restart).await;
        run.create_project().await;
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
        keychain.add(
            "unknown",
            &format!(
                "Claude Code-credentials-{}",
                &crate::memory::hash(home.to_str().unwrap().as_bytes())[..8]
            ),
            &serde_json::json!({
                "claudeAiOauth": {
                    "accessToken": "sk-ant-oat01-lms1-unit",
                    "refreshToken": "sk-ant-ort01-lms1-unit",
                    "expiresAt": now_ms() + 365 * 86_400_000,
                    "scopes": ["user:inference", "user:profile"],
                    "subscriptionType": "max"
                }
            })
            .to_string(),
        );
        let path = restart.root.path().join("engine.db");
        let held =
            crate::engine::NativeProfileCustody::capture(&path, &std::fs::metadata(&path).unwrap())
                .unwrap()
                .acquire_use(
                    &run.handle.identity(),
                    &signed.binding,
                    &request("lms1-held-sign-in"),
                    1,
                    crate::engine::bootstrap::NativeProfileUsePurpose::Authentication,
                )
                .unwrap();
        let reproving = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (mut team, setup, _host) = restart.live_team(reproving);
        restart.team_action(TeamAction::LiveMaster {
            project_id: setup.project_id.clone(),
            on: true,
            version: restart.team_settings_version(&setup.project_id),
        });
        for _ in 0..5 {
            team.tick().unwrap();
        }
        let waiting = crate::team::master::PROFILE_IN_USE;
        assert_eq!(
            restart.live_switch(),
            (Some(true), Some(waiting.to_owned())),
            "the switch stays on and its problem line says the profile is in use"
        );
        assert_eq!(
            restart.messages(&setup),
            [(
                MessageAuthor::Engine,
                MessageState::Note,
                waiting.to_owned()
            )],
            "one note"
        );
        assert_eq!(
            restart.team_agents("master").len(),
            1,
            "the master started and waits for its account"
        );
        drop(held);
        team.tick().unwrap();
        assert_eq!(
            restart.live_switch(),
            (Some(true), None),
            "the master is handed the account and its wait line is cleared"
        );
        assert_eq!(restart.messages(&setup).len(), 1, "no second note");
        let mut master = restart.team_agents("master");
        for _ in 0..50 {
            if master.iter().any(|(_, status, _)| status == "failed") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            team.tick().unwrap();
            master = restart.team_agents("master");
        }
        assert_eq!(
            master,
            [(
                "connection-z".to_owned(),
                "failed".to_owned(),
                Some("The agent process closed. (launch_closed)".to_owned())
            )],
            "the master was handed the account and its terminal ran the stand-in, which ends at once"
        );
        team.shutdown().unwrap();
        run.stop().await;
    })
    .await
    .expect("the live master profile wait case exceeded 60 seconds");
}

/// DEC-108: once the account an earlier run signed in passes this run's re-check, the
/// waiting live master starts on the next tick and its wait line is cleared. Needs a live
/// Claude account: the installed Claude Code answers the re-check and runs the master, and
/// the owner finishes the real sign-in in the browser, which on macOS also makes a keychain
/// in the temp profile. Runs only by name with `--ignored` in an owner-approved live window.
#[ignore = "live: needs a live Claude account; the owner finishes a real sign-in through the installed Claude Code"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_master_starts_once_restart_recheck_passes() {
    use crate::team::TeamAction;
    use std::sync::atomic::Ordering::SeqCst;
    let restart = Restart::live();
    tokio::time::timeout(Duration::from_secs(900), async {
        let first = restart.start().await;
        first.sign_in(&restart).await;
        first.create_project().await;
        first.stop().await;
        let second = restart.start_bare().await;
        let reproving = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (mut team, setup, _host) = restart.live_team(reproving.clone());
        restart.team_action(TeamAction::LiveMaster {
            project_id: setup.project_id.clone(),
            on: true,
            version: restart.team_settings_version(&setup.project_id),
        });
        team.tick().unwrap();
        assert_eq!(
            restart.live_switch(),
            (Some(true), Some(CHECKING.to_owned()))
        );
        assert!(restart.team_agents("master").is_empty());
        second.stop().await;
        let third = restart.start().await;
        assert_eq!(
            settled(&restart, &third, AccountStatus::SignedIn).await,
            Ok(())
        );
        team.tick().unwrap();
        assert_eq!(
            restart.live_switch(),
            (Some(true), None),
            "the master starts and its wait line is cleared"
        );
        assert_eq!(restart.team_agents("master").len(), 1);
        reproving.store(false, SeqCst);
        team.shutdown().unwrap();
        third.stop().await;
    })
    .await
    .expect("the live master start case exceeded 900 seconds");
}

/// DEC-108: while this run's re-proof runs and the only pool account's check holds its
/// profile, a queued task stays queued rather than needing a pool, and a message to the
/// master that is not live stays pending. Once the re-proof ended with no proof, the task
/// needs a pool and the message fails with the sign-in refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_task_stays_queued_while_checking() {
    use crate::team::{MessageAuthor, MessageState, TaskStatus, TeamAction};
    use std::sync::atomic::Ordering::SeqCst;
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let run = restart.start_bare().await;
        run.create_project().await;
        let reproving = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (team, setup) = restart.team();
        let mut team = team.with_reproof(reproving.clone());
        let project = setup.project_id.clone();
        let (admission, work) = run.begin_check(&restart).await;
        restart.team_action(TeamAction::Message {
            project_id: project.clone(),
            text: "Plan the page".into(),
        });
        restart.team_action(TeamAction::Delegate {
            project_id: project.clone(),
            harness: None,
            prompt: "Write page.md".into(),
        });
        for _ in 0..3 {
            team.tick().unwrap();
        }
        assert_eq!(restart.tasks(&setup), [TaskStatus::Queued]);
        assert_eq!(
            restart.messages(&setup),
            [(
                MessageAuthor::Owner,
                MessageState::Pending,
                "Plan the page".to_owned()
            )]
        );
        assert!(restart.team_agents("master").is_empty());
        assert!(restart.team_agents("sub").is_empty());
        reproving.store(false, SeqCst);
        team.tick().unwrap();
        assert_eq!(restart.tasks(&setup), [TaskStatus::NeedsPool]);
        assert_eq!(
            restart.messages(&setup),
            [
                (
                    MessageAuthor::Owner,
                    MessageState::Failed,
                    "Plan the page".to_owned()
                ),
                (
                    MessageAuthor::Engine,
                    MessageState::Note,
                    format!("The master could not start: {SIGN_IN_TO_RUN}.")
                ),
            ]
        );
        run.finish_check(admission, work).await;
        team.shutdown().unwrap();
        run.stop().await;
    })
    .await
    .expect("the queued task wait case exceeded 60 seconds");
}

/// DEC-108: an account whose sign-in the provider rejected is not waited for, even while
/// this run's re-proof runs and its check holds the profile: the master's message fails
/// with "Sign in again" and the task needs a pool on the first tick.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_account_not_waited() {
    use crate::team::{MessageAuthor, MessageState, TaskStatus, TeamAction};
    let restart = Restart::new();
    tokio::time::timeout(Duration::from_secs(60), async {
        let run = restart.start_bare().await;
        run.create_project().await;
        let reproving = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (team, setup) = restart.team();
        let mut team = team.with_reproof(reproving);
        let project = setup.project_id.clone();
        let (admission, work) = run.begin_check(&restart).await;
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
        assert_eq!(restart.tasks(&setup), [TaskStatus::NeedsPool]);
        assert_eq!(
            restart.messages(&setup),
            [
                (
                    MessageAuthor::Owner,
                    MessageState::Failed,
                    "Plan the page".to_owned()
                ),
                (
                    MessageAuthor::Engine,
                    MessageState::Note,
                    "The master could not start: Sign in again: the provider rejected the sign-in of every account in this project's pool that can run claude-code.".to_owned()
                ),
            ]
        );
        run.finish_check(admission, work).await;
        team.shutdown().unwrap();
        run.stop().await;
    })
    .await
    .expect("the rejected account case exceeded 60 seconds");
}
