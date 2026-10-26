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
        let root = tempfile::Builder::new()
            .prefix("ca-rst-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("g.toml");
        std::fs::write(&global, CATALOG).unwrap();
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
        self.config()
            .settings
            .model_access
            .unwrap()
            .connections
            .into_iter()
            .find(|item| item.id == connection())
            .unwrap()
    }

    fn hold(&self) {
        let now = now_ms();
        let config = self.config();
        let mut database = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        database.busy_timeout(Duration::from_secs(2)).unwrap();
        let tx = database.transaction().unwrap();
        tx.execute(
            "INSERT INTO team_paused_accounts(connection,until_ms) VALUES (?1,?2)
             ON CONFLICT(connection) DO UPDATE SET until_ms=excluded.until_ms",
            rusqlite::params![connection().as_str(), now + 600_000],
        )
        .unwrap();
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
                connection: self.model(),
                sequence: u64::try_from(sequence).unwrap() + 1,
                observed_at_ms: u64::try_from(now).unwrap(),
                valid_until_ms: u64::try_from(now).unwrap() + 60_000,
                kind: crate::model_gateway::ObservationKind::InvalidAuthentication,
            },
        )
        .unwrap();
        tx.commit().unwrap();
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
        let response = self
            .client()
            .request(OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: request("restart-source"),
                expected_revision: Cursor::new(0),
                operation: OwnerOperation::Read {
                    query: OwnerRead::ModelAccessRecord {
                        collection: ModelAccessCollection::Connections,
                        id: connection(),
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
            connection: connection(),
            expected_config_id: config_id,
            expected_source_hash: source_hash,
        }
    }

    async fn sign_in(&self, restart: &Restart) -> NativeAuthFact {
        let scope = self.scope(restart.config().id).await;
        let client = self.client();
        let started = client
            .start_native_auth(NativeAuthStart {
                request_id: request("restart-sign-in"),
                scope: scope.clone(),
                method: NativeAuthMethod::ClaudeOfficial,
            })
            .await
            .unwrap();
        for _ in 0..200 {
            let read = client
                .read_native_auth(NativeAuthQuery {
                    request_id: request("restart-sign-in-read"),
                    scope: scope.clone(),
                    operation_id: started.fact.operation_id.clone(),
                })
                .await
                .unwrap();
            if !read.fact.phase.unfinished() {
                assert_eq!(read.fact.phase, NativeAuthPhase::AuthenticatedObserved);
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
        let page = self.client().list_accounts(None, 50).await.unwrap();
        page.accounts
            .into_iter()
            .find(|account| account.id == connection())
            .expect("the configured account is listed")
            .status
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
            settled(&restart, &second, AccountStatus::SignedIn).await,
            Ok(())
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
