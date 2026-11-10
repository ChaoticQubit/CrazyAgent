#![cfg(unix)]

use crazyagent::client_controller::OwnerController;
use crazyagent::config::ModelAccessEdit;
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{
    ModelAccessCollection, OwnerClientKind, OwnerData, OwnerMutation, OwnerRead, OwnerResponse,
    OwnerResult,
};
use crazyagent::protocol::{CredentialRef, ProjectId, ProjectSpec, StoredProject};
use crazyagent::scheduling::{
    AccountEntry, AccountPlan, Pin, PoolRanking, RankedAccount, Schedule, ScheduleChange,
    ScopedStrategy, Standing, Strategy,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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

fn applied(response: OwnerResponse) -> OwnerResult {
    match response {
        OwnerResponse::Applied { operation } | OwnerResponse::Committed { operation } => {
            operation.result
        }
        other => panic!("actual owner receipt required: {other:?}"),
    }
}

async fn send(controller: &mut OwnerController, change: ScheduleChange) -> OwnerResult {
    let version = controller.account_schedule().await.unwrap().version;
    applied(
        controller
            .set_account_schedule(change, version, 1)
            .await
            .unwrap(),
    )
}

async fn edit(controller: &mut OwnerController, edit: ModelAccessEdit) -> OwnerResult {
    let reply = controller
        .read(OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Pools,
            cursor: None,
            limit: 1,
        })
        .await
        .unwrap();
    let OwnerData::ModelAccessPage { source_hash, .. } = &reply.data else {
        panic!("actual pool page required");
    };
    controller
        .begin_mutation(
            OwnerMutation::ModelAccess {
                expected_config_id: reply.config_id.clone(),
                expected_source_hash: source_hash.clone(),
                edit,
            },
            1,
        )
        .unwrap();
    applied(controller.send_pending().await.unwrap())
}

async fn create_project(controller: &mut OwnerController, id: &str) -> ProjectId {
    let project = ProjectId::new(id).unwrap();
    controller.read(OwnerRead::Capabilities).await.unwrap();
    controller
        .begin_mutation(
            OwnerMutation::CreateProject {
                project_id: project.clone(),
                spec: ProjectSpec {
                    name: format!("Project {id}"),
                    repositories: vec![],
                },
            },
            1,
        )
        .unwrap();
    assert_eq!(
        applied(controller.send_pending().await.unwrap()),
        OwnerResult::Applied
    );
    project
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
    create_project(&mut controller, "project-a").await;
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
                4,
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
        version: 4,
    };
    assert_eq!(controller.account_schedule().await.unwrap(), expected);
    let not_signed_in = |connection: &str| RankedAccount {
        connection: reference(connection),
        standing: Standing::NotSignedIn,
    };
    assert_eq!(
        controller.account_ranking().await.unwrap(),
        [PoolRanking {
            pool: reference("pool-claude"),
            strategy: Strategy::Balanced,
            accounts: vec![not_signed_in("claude-a"), not_signed_in("claude-b")],
            own_order_projects: 0,
        }]
    );
    drop(controller);
    case.restart().await;
    let mut controller = case.controller().await;
    assert_eq!(controller.account_schedule().await.unwrap(), expected);
    drop(controller);
    case.close().await;
}

/// A schedule larger than one owner reply still takes changes and can be shrunk: a change
/// reads only the small owner profile for freshness, never the whole schedule. The schedule
/// is then read whole, page by page.
/// Registers projects in the engine's store, past the count `CreateProject` admits, because
/// a schedule over one reply needs more project rows than one snapshot may hold. Inputs: the
/// engine root and the ids. Output: none.
fn register_projects(root: &Path, ids: impl Iterator<Item = String>) {
    let mut connection = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    let tx = connection.transaction().unwrap();
    for id in ids {
        let stored = StoredProject {
            project_id: ProjectId::new(id.clone()).unwrap(),
            spec: ProjectSpec {
                name: "Registered".into(),
                repositories: vec![],
            },
        };
        tx.execute(
            "INSERT INTO projects VALUES (?1,?2)",
            [id, serde_json::to_string(&stored).unwrap()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

#[tokio::test]
async fn a_schedule_larger_than_one_reply_takes_changes_and_is_read_whole() {
    let case = Case::new().await;
    let mut controller = case.controller().await;
    let project = |n: usize| reference(&format!("{n:04}{}", "p".repeat(120)));
    let overrides = 600;
    let stored = Schedule {
        projects: (0..overrides)
            .map(|n| ScopedStrategy {
                id: project(n),
                strategy: Strategy::Balanced,
            })
            .collect(),
        ..Schedule::default()
    };
    assert!(serde_json::to_vec(&stored).unwrap().len() > crazyagent::protocol::FRAME_BYTES_MAX);
    register_projects(
        case.root.path(),
        (0..overrides).map(|n| project(n).as_str().to_owned()),
    );
    for n in 0..overrides {
        let change = ScheduleChange::ProjectStrategy {
            project: project(n),
            strategy: Some(Strategy::Balanced),
        };
        assert_eq!(
            send(&mut controller, change).await,
            OwnerResult::Applied,
            "{n}"
        );
    }
    let shrink = ScheduleChange::ProjectStrategy {
        project: project(0),
        strategy: None,
    };
    assert_eq!(
        send(&mut controller, shrink).await,
        OwnerResult::Applied,
        "the owner can still shrink a schedule past one reply"
    );
    assert_eq!(
        controller.account_schedule().await.unwrap(),
        Schedule {
            projects: stored.projects[1..].to_vec(),
            version: overrides as u64 + 1,
            ..Schedule::default()
        },
        "a schedule past one reply is read whole, at the version of its last change"
    );
    drop(controller);
    case.close().await;
}

/// Removing a pool, a connection or a project takes its scheduling row with it, so no row
/// outlives what it is about, and the rows of what stays are kept.
#[tokio::test]
async fn a_removed_pool_connection_or_project_takes_its_schedule_row_with_it() {
    let case = Case::new().await;
    let mut controller = case.controller().await;
    let project = create_project(&mut controller, "project-a").await;
    let plan = AccountPlan {
        rank: Some(1),
        ..AccountPlan::default()
    };
    for change in [
        ScheduleChange::PoolStrategy {
            pool: reference("pool-claude"),
            strategy: Some(Strategy::Balanced),
        },
        ScheduleChange::ProjectStrategy {
            project: reference(project.as_str()),
            strategy: Some(Strategy::UseItOrLoseIt),
        },
        ScheduleChange::Account {
            connection: reference("claude-a"),
            plan,
        },
        ScheduleChange::Account {
            connection: reference("claude-b"),
            plan,
        },
    ] {
        assert_eq!(send(&mut controller, change).await, OwnerResult::Applied);
    }
    for removal in [
        ModelAccessEdit::RemovePool {
            id: reference("pool-claude"),
        },
        ModelAccessEdit::RemoveConnection {
            id: reference("claude-b"),
        },
    ] {
        assert_eq!(edit(&mut controller, removal).await, OwnerResult::Applied);
    }
    assert_eq!(
        applied(controller.remove_project(project, 1).await.unwrap()),
        OwnerResult::Applied
    );
    assert_eq!(
        controller.account_schedule().await.unwrap(),
        Schedule {
            accounts: vec![AccountEntry {
                connection: reference("claude-a"),
                plan,
            }],
            version: 7,
            ..Schedule::default()
        },
        "only the connection that stays keeps its plan, at the version of the four changes and the three removals"
    );
    drop(controller);
    case.close().await;
}

/// A row whose pool or connection left the settings outside the engine, as by a hand edit of
/// the settings file, is still shown and can still be deleted, while a new row for it is
/// refused.
#[tokio::test]
async fn a_row_whose_pool_or_connection_was_edited_out_by_hand_can_still_be_deleted() {
    let mut case = Case::new().await;
    let mut controller = case.controller().await;
    let plan = AccountPlan {
        rank: Some(1),
        ..AccountPlan::default()
    };
    let pool = |strategy| ScheduleChange::PoolStrategy {
        pool: reference("pool-claude"),
        strategy,
    };
    let account = |plan| ScheduleChange::Account {
        connection: reference("claude-b"),
        plan,
    };
    assert_eq!(
        send(&mut controller, pool(Some(Strategy::Balanced))).await,
        OwnerResult::Applied
    );
    assert_eq!(
        send(&mut controller, account(plan)).await,
        OwnerResult::Applied
    );
    drop(controller);
    let edited: String = POOL
        .lines()
        .filter(|line| !line.contains("\"pool-claude\"") && !line.contains("\"claude-b\""))
        .map(|line| format!("{line}\n"))
        .collect();
    fs::write(&case.global, edited).unwrap();
    case.restart().await;
    let mut controller = case.controller().await;
    let orphaned = controller.account_schedule().await.unwrap();
    assert_eq!(
        (orphaned.pools.len(), orphaned.accounts.len()),
        (1, 1),
        "the rows the hand edit left are still shown"
    );
    assert_eq!(
        send(&mut controller, pool(Some(Strategy::Balanced))).await,
        OwnerResult::Rejected {
            code: "pool_not_found".into()
        }
    );
    assert_eq!(
        send(&mut controller, account(plan)).await,
        OwnerResult::Rejected {
            code: "connection_not_found".into()
        }
    );
    assert_eq!(
        send(&mut controller, pool(None)).await,
        OwnerResult::Applied,
        "a row whose pool is gone can be deleted"
    );
    assert_eq!(
        send(&mut controller, account(AccountPlan::default())).await,
        OwnerResult::Applied,
        "a row whose connection is gone can be deleted"
    );
    assert_eq!(
        controller.account_schedule().await.unwrap(),
        Schedule {
            version: 4,
            ..Schedule::default()
        },
        "both rows are gone, at the version of the four applied changes"
    );
    drop(controller);
    case.close().await;
}
