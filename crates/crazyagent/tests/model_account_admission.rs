use crazyagent::{
    config::{EffectiveConfig, resolve_config},
    coordination::{OwnerAction, Role, WorkItem, WorkPlan},
    engine::Engine,
    protocol::*,
    routing::{ResourceBudget, RouteRequest},
};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

const CATALOG: &str = r#"
allowed_destinations = ["profile-a", "profile-b"]
[routing]
approved_accounts = ["account-a", "account-b", "account-c"]
[model_access]
accounts = ["account-a", "account-b", "account-c"]
[[model_access.connections]]
id = "connection-a"
provider = "local-service"
harness = "rcli"
protocol = "openai-compatible"
account_ref = "account-a"
endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }
model = { id = "example/model", author = "independent-author" }
auth = { method = "none" }
[[model_access.connections]]
id = "connection-b"
provider = "other-service"
harness = "rcli"
protocol = "openai-compatible"
account_ref = "account-b"
endpoint = { kind = "http", url = "http://127.0.0.1:11435/v1" }
model = { id = "example/model", author = "independent-author" }
auth = { method = "none" }
[[model_access.pools]]
id = "worker-pool"
members = ["connection-a", "connection-b"]
projects = ["project-a"]
roles = ["worker"]
fallback_allowed = true
"#;

fn request(command: Command, sequence: u64, revision: u64) -> WireRequest {
    WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("pool-policy-test").unwrap(),
        request_id: RequestId::new(format!("operation-{sequence}")).unwrap(),
        expected_revision: Cursor::new(revision),
        command,
    }
}

#[tokio::test]
async fn pool_role_scope_is_enforced_before_plan_persistence() {
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, CATALOG).unwrap();
    let config = resolve_config(&global, &directory.path().join("project.toml")).unwrap();
    let context = ExecutionContext {
        project_id: ProjectId::new("project-a").unwrap(),
        task_id: TaskId::new("task-a").unwrap(),
        config_id: config.id.clone(),
        snapshot_id: SnapshotId::new("snapshot-a").unwrap(),
        baseline_id: BaselineId::new("baseline-a").unwrap(),
        allowed_destinations: config.settings.allowed_destinations.clone(),
        budget_ref: CredentialRef::new("budget-a").unwrap(),
    };
    let plan: WorkPlan = serde_json::from_value(json!({
        "id": "plan-a",
        "scope": context,
        "tasks": [{
            "id": "item-a",
            "session_id": "session-a",
            "role": "ProjectLeader",
            "native_descendant": null,
            "pool_ref": "worker-pool",
            "route": {
                "preferred_target": "profile-a",
                "required_target": null,
                "required_capabilities": ["coding"],
                "account_ref": "account-a",
                "budget_ref": "budget-a",
                "resources": ResourceBudget::unit()
            }
        }],
        "dependencies": [],
        "authority": "Human",
        "config_id": config.id
    }))
    .expect("a named pool must be accepted by the WorkItem wire contract");
    let engine = Engine::open(&directory.path().join("engine.sqlite"), config)
        .await
        .unwrap();
    for (sequence, command) in [
        Command::CreateProject {
            project_id: context.project_id.clone(),
            spec: ProjectSpec {
                name: "Pool policy project".into(),
                repositories: vec![],
            },
        },
        Command::PinTask { context },
    ]
    .into_iter()
    .enumerate()
    {
        let reply = engine
            .apply(request(command, sequence as u64 + 1, sequence as u64))
            .await
            .unwrap();
        assert_eq!(reply.outcome, Outcome::Applied);
        assert_eq!(reply.revision.value(), sequence as u64 + 1);
    }
    let reply = engine
        .apply(request(Command::ProposePlan { plan: plan.clone() }, 3, 2))
        .await
        .unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "route_pool_scope_denied".into()
        }
    );
    assert_eq!(reply.revision.value(), 2);
    let reply = engine
        .apply(request(
            Command::CoordinationState { plan_id: plan.id },
            4,
            2,
        ))
        .await
        .unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "plan_missing".into()
        }
    );
    assert_eq!(reply.revision.value(), 2);
    engine.close().await.unwrap();
}

fn context_for(config: &EffectiveConfig, project: &str) -> ExecutionContext {
    ExecutionContext {
        project_id: ProjectId::new(project).unwrap(),
        task_id: TaskId::new("task-a").unwrap(),
        config_id: config.id.clone(),
        snapshot_id: SnapshotId::new("snapshot-a").unwrap(),
        baseline_id: BaselineId::new("baseline-a").unwrap(),
        allowed_destinations: config.settings.allowed_destinations.clone(),
        budget_ref: CredentialRef::new("budget-a").unwrap(),
    }
}

fn pool_plan(context: ExecutionContext, account: &str, pool: Option<&str>) -> WorkPlan {
    WorkPlan {
        id: PlanId::new("plan-a").unwrap(),
        config_id: context.config_id.clone(),
        tasks: vec![WorkItem {
            id: TaskId::new("item-a").unwrap(),
            session_id: SessionId::new("session-a").unwrap(),
            role: Role::Worker,
            native_descendant: None,
            pool_ref: pool.map(|id| CredentialRef::new(id).unwrap()),
            route: RouteRequest {
                preferred_target: Some(CredentialRef::new("profile-a").unwrap()),
                required_target: None,
                required_capabilities: vec!["coding".into()],
                account_ref: CredentialRef::new(account).unwrap(),
                budget_ref: context.budget_ref.clone(),
                resources: ResourceBudget::unit(),
            },
        }],
        scope: context,
        dependencies: vec![],
        authority: crazyagent::coordination::PlanAuthority::Human,
    }
}

async fn create_task(engine: &Engine, context: ExecutionContext) {
    for (index, command) in [
        Command::CreateProject {
            project_id: context.project_id.clone(),
            spec: ProjectSpec {
                name: "Pool policy project".into(),
                repositories: vec![],
            },
        },
        Command::PinTask { context },
    ]
    .into_iter()
    .enumerate()
    {
        let reply = engine
            .apply(request(command, index as u64 + 1, index as u64))
            .await
            .unwrap();
        assert_eq!(reply.outcome, Outcome::Applied);
        assert_eq!(reply.revision.value(), index as u64 + 1);
    }
}

async fn rejected_pool(project: &str, account: &str, pool: &str, catalog: &str, code: &str) {
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, catalog).unwrap();
    let config = resolve_config(&global, &directory.path().join("project.toml")).unwrap();
    let context = context_for(&config, project);
    let plan = pool_plan(context.clone(), account, Some(pool));
    let engine = Engine::open(&directory.path().join("engine.sqlite"), config)
        .await
        .unwrap();
    create_task(&engine, context).await;
    let reply = engine
        .apply(request(Command::ProposePlan { plan: plan.clone() }, 3, 2))
        .await
        .unwrap();
    assert_eq!(reply.outcome, Outcome::Rejected { code: code.into() });
    assert_eq!(reply.revision.value(), 2);
    let reply = engine
        .apply(request(
            Command::CoordinationState { plan_id: plan.id },
            4,
            2,
        ))
        .await
        .unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "plan_missing".into()
        }
    );
    assert_eq!(reply.revision.value(), 2);
    engine.close().await.unwrap();
}

#[tokio::test]
async fn pool_project_scope_is_enforced_before_plan_persistence() {
    rejected_pool(
        "project-b",
        "account-a",
        "worker-pool",
        CATALOG,
        "route_pool_scope_denied",
    )
    .await;
}

#[tokio::test]
async fn missing_pool_or_catalog_cannot_persist_a_plan() {
    rejected_pool(
        "project-a",
        "account-a",
        "missing-pool",
        CATALOG,
        "route_pool_missing",
    )
    .await;
    rejected_pool(
        "project-a",
        "account-a",
        "worker-pool",
        "allowed_destinations = ['profile-a']\n[routing]\napproved_accounts = ['account-a']",
        "route_pool_missing",
    )
    .await;
}

#[tokio::test]
async fn approved_account_outside_pool_cannot_persist_a_plan() {
    rejected_pool(
        "project-a",
        "account-c",
        "worker-pool",
        CATALOG,
        "route_pool_account_denied",
    )
    .await;
}

#[tokio::test]
async fn pool_membership_cannot_approve_an_account() {
    let catalog = CATALOG.replace(
        "approved_accounts = [\"account-a\", \"account-b\", \"account-c\"]",
        "approved_accounts = [\"account-b\"]",
    );
    rejected_pool(
        "project-a",
        "account-a",
        "worker-pool",
        &catalog,
        "route_account_denied",
    )
    .await;
}

#[tokio::test]
async fn unknown_role_label_cannot_grant_worker_scope() {
    let catalog = CATALOG.replace("roles = [\"worker\"]", "roles = [\"worker-extra\"]");
    rejected_pool(
        "project-a",
        "account-a",
        "worker-pool",
        &catalog,
        "route_pool_scope_denied",
    )
    .await;
}

#[test]
fn absent_pool_ref_retains_the_existing_wire_shape() {
    let directory = tempfile::tempdir().unwrap();
    let config = resolve_config(
        &directory.path().join("global"),
        &directory.path().join("project"),
    )
    .unwrap();
    let plan = pool_plan(context_for(&config, "project-a"), "account-a", None);
    let value = serde_json::to_value(&plan).unwrap();
    assert!(value["tasks"][0].get("pool_ref").is_none());
    let decoded: WorkPlan = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(decoded, plan);
    assert_eq!(serde_json::to_value(decoded).unwrap(), value);
}

#[tokio::test]
async fn pool_policy_is_pinned_across_reload_restart_and_rechecked_before_routing() {
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = directory.path().join("global.toml");
    let project = directory.path().join("project.toml");
    let database = directory.path().join("engine.sqlite");
    std::fs::write(&global, CATALOG).unwrap();
    let pinned = resolve_config(&global, &project).unwrap();
    let context = context_for(&pinned, "project-a");
    let plan = pool_plan(context.clone(), "account-a", Some("worker-pool"));
    let engine = Engine::open(&database, pinned.clone()).await.unwrap();
    create_task(&engine, context).await;
    for (index, command) in [
        Command::ProposePlan { plan: plan.clone() },
        Command::OwnerCoordination {
            action: OwnerAction::ApprovePlan {
                plan_id: plan.id.clone(),
            },
        },
        Command::OwnerCoordination {
            action: OwnerAction::Reserve {
                plan_id: plan.id.clone(),
                item_id: plan.tasks[0].id.clone(),
            },
        },
    ]
    .into_iter()
    .enumerate()
    {
        let reply = engine
            .apply(request(command, index as u64 + 3, index as u64 + 2))
            .await
            .unwrap();
        assert_eq!(reply.outcome, Outcome::Applied);
        assert_eq!(reply.revision.value(), index as u64 + 3);
    }
    let route = || Command::RouteWorkItem {
        plan_id: plan.id.clone(),
        item_id: plan.tasks[0].id.clone(),
    };
    let reply = engine.apply(request(route(), 6, 5)).await.unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "native_admission_unavailable".into()
        }
    );
    assert_eq!(reply.revision.value(), 5);
    std::fs::write(&global, "").unwrap();
    let current = resolve_config(&global, &project).unwrap();
    assert_ne!(current.id, pinned.id);
    engine.update_config(current.clone()).await.unwrap();
    let reply = engine.apply(request(route(), 7, 5)).await.unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "native_admission_unavailable".into()
        }
    );
    assert_eq!(reply.revision.value(), 5);
    engine.close().await.unwrap();
    let engine = Engine::open(&database, current.clone()).await.unwrap();
    let reply = engine.apply(request(route(), 8, 5)).await.unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "native_admission_unavailable".into()
        }
    );
    assert_eq!(reply.revision.value(), 5);
    let reply = engine
        .apply(request(
            Command::TaskConfig {
                task_id: plan.scope.task_id.clone(),
            },
            9,
            5,
        ))
        .await
        .unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Configuration {
            config: Box::new(pinned)
        }
    );
    let reply = engine
        .apply(request(
            Command::CoordinationState {
                plan_id: plan.id.clone(),
            },
            10,
            5,
        ))
        .await
        .unwrap();
    let Outcome::Coordination { state } = reply.outcome else {
        panic!("persisted pool plan required")
    };
    assert_eq!(state.plan, plan);
    assert_eq!(state.reservations.len(), 1);
    let reservation = &state.reservations[&plan.tasks[0].id];
    assert_eq!(reservation.config_id, plan.config_id);
    assert_eq!(
        reservation.provenance,
        "engine_planning_hold_not_native_dispatch"
    );
    assert_eq!(reply.revision.value(), 5);
    engine.close().await.unwrap();
    let mut changed = *state;
    changed.plan.tasks[0].role = Role::ProjectLeader;
    let connection = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE coordination_plans SET body=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&changed).unwrap(), plan.id.as_str()],
            )
            .unwrap(),
        1
    );
    drop(connection);
    let engine = Engine::open(&database, current).await.unwrap();
    let reply = engine.apply(request(route(), 11, 5)).await.unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "route_pool_scope_denied".into()
        }
    );
    assert_eq!(reply.revision.value(), 5);
    let reply = engine
        .apply(request(
            Command::CoordinationState {
                plan_id: plan.id.clone(),
            },
            12,
            5,
        ))
        .await
        .unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Coordination {
            state: Box::new(changed)
        }
    );
    assert_eq!(reply.revision.value(), 5);
    engine.close().await.unwrap();
}
