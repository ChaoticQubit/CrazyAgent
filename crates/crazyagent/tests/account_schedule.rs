#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{OwnerClientKind, OwnerResponse, OwnerResult};
use crazyagent::protocol::CredentialRef;
use crazyagent::scheduling::{
    AccountEntry, AccountPlan, Pin, Schedule, ScheduleChange, ScopedStrategy, Strategy,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::watch;

const POOL: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["claude-1", "claude-2"]
connections = [
  { id = "claude-a", provider = "anthropic", harness = "claude", protocol = "native", account_ref = "claude-1", endpoint = { kind = "native" }, model = { id = "native-default", author = "anthropic" }, auth = { method = "native_managed", profile_ref = "claude-profile-a" } },
  { id = "claude-b", provider = "anthropic", harness = "claude", protocol = "native", account_ref = "claude-2", endpoint = { kind = "native" }, model = { id = "native-default", author = "anthropic" }, auth = { method = "native_managed", profile_ref = "claude-profile-b" } },
]
pools = [
  { id = "pool-claude", members = ["claude-a", "claude-b"], projects = [], roles = [], fallback_allowed = false },
]
"#;

struct Case {
    root: tempfile::TempDir,
    global: PathBuf,
    service: Option<EngineService>,
    stopped: watch::Receiver<bool>,
    _stop: watch::Sender<bool>,
}

impl Case {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-schedule-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let global = root.path().join("global.toml");
        fs::write(&global, POOL).unwrap();
        fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = watch::channel(false);
        let mut case = Self {
            root,
            global,
            service: None,
            stopped,
            _stop: stop,
        };
        case.start().await;
        case
    }

    async fn start(&mut self) {
        let service = EngineService::start_with_settings_sources(
            self.root.path(),
            "e.sock",
            &self.global,
            &self.root.path().join("project.toml"),
            &self.stopped,
        )
        .await
        .unwrap();
        self.service = Some(service);
    }

    async fn restart(&mut self) {
        self.service.take().unwrap().shutdown().await.unwrap();
        self.start().await;
    }

    async fn controller(&self) -> OwnerController {
        OwnerController::attach(
            self.root.path(),
            OwnerClientKind::Tui,
            Duration::from_secs(5),
        )
        .await
        .unwrap()
    }

    async fn close(mut self) {
        self.service.take().unwrap().shutdown().await.unwrap();
    }
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

async fn send(controller: &mut OwnerController, change: ScheduleChange) -> OwnerResult {
    match controller.set_account_schedule(change, 1).await.unwrap() {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            operation.result
        }
        other => panic!("actual owner receipt required: {other:?}"),
    }
}

#[tokio::test]
async fn scheduling_choices_go_through_the_engine_and_survive_a_restart() {
    let mut case = Case::new().await;
    let mut controller = case.controller().await;
    assert_eq!(
        controller.account_schedule().await.unwrap(),
        Schedule::default()
    );
    let plan = AccountPlan {
        rank: Some(1),
        reserve_percent: 10,
        pin: Some(Pin::UseLast),
    };
    for change in [
        ScheduleChange::AppStrategy {
            strategy: Strategy::ManualRank,
        },
        ScheduleChange::PoolStrategy {
            pool: reference("pool-claude"),
            strategy: Some(Strategy::Balanced),
        },
        ScheduleChange::ProjectStrategy {
            project: reference("project-a"),
            strategy: Some(Strategy::UseItOrLoseIt),
        },
        ScheduleChange::Account {
            connection: reference("claude-b"),
            plan,
        },
    ] {
        assert_eq!(send(&mut controller, change).await, OwnerResult::Applied);
    }
    assert_eq!(
        send(
            &mut controller,
            ScheduleChange::Account {
                connection: reference("claude-missing"),
                plan,
            }
        )
        .await,
        OwnerResult::Rejected {
            code: "connection_not_found".into()
        }
    );
    assert!(
        controller
            .set_account_schedule(
                ScheduleChange::Account {
                    connection: reference("claude-a"),
                    plan: AccountPlan {
                        reserve_percent: 100,
                        ..plan
                    },
                },
                1,
            )
            .await
            .is_err()
    );
    let expected = Schedule {
        strategy: Strategy::ManualRank,
        pools: vec![ScopedStrategy {
            id: reference("pool-claude"),
            strategy: Strategy::Balanced,
        }],
        projects: vec![ScopedStrategy {
            id: reference("project-a"),
            strategy: Strategy::UseItOrLoseIt,
        }],
        accounts: vec![AccountEntry {
            connection: reference("claude-b"),
            plan,
        }],
    };
    assert_eq!(controller.account_schedule().await.unwrap(), expected);
    drop(controller);
    case.restart().await;
    let mut controller = case.controller().await;
    assert_eq!(controller.account_schedule().await.unwrap(), expected);
    drop(controller);
    case.close().await;
}
