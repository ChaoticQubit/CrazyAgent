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

#[tokio::test]
async fn reservation_pins_exact_worker_connection() {
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, CATALOG).unwrap();
    let config = resolve_config(&global, &directory.path().join("project.toml")).unwrap();
    let context = context_for(&config, "project-a");
    let mut plan = pool_plan(context.clone(), "account-b", Some("worker-pool"));
    plan.tasks[0].route.preferred_target = None;
    plan.tasks[0].route.required_target = Some(CredentialRef::new("profile-b").unwrap());
    let engine = Engine::open(&directory.path().join("engine.sqlite"), config.clone())
        .await
        .unwrap();
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
    let reply = engine
        .apply(request(
            Command::CoordinationState {
                plan_id: plan.id.clone(),
            },
            6,
            5,
        ))
        .await
        .unwrap();
    engine.close().await.unwrap();
    assert_eq!(reply.revision.value(), 5);
    let Outcome::Coordination { state } = reply.outcome else {
        panic!("reserved worker state required")
    };
    assert_eq!(state.plan, plan);
    assert_eq!(state.reservations.len(), 1);
    let reservation = serde_json::to_value(&state.reservations[&plan.tasks[0].id]).unwrap();
    assert_eq!(reservation["cursor"], json!("5"));
    assert_eq!(
        reservation["binding"],
        json!({
            "plan_id": "plan-a",
            "task_id": "item-a",
            "session_id": "session-a",
            "pool_ref": "worker-pool",
            "target_profile_ref": "profile-b",
            "connection": {
                "id": "connection-b", "provider": "other-service", "harness": "rcli",
                "protocol": "openai-compatible", "account_ref": "account-b",
                "endpoint": { "kind": "http", "url": "http://127.0.0.1:11435/v1" },
                "model": { "id": "example/model", "author": "independent-author" },
                "auth": { "method": "none" }
            },
            "config_id": config.id,
            "budget_ref": "budget-a",
            "resources": {
                "slots": 1, "cpu_millis": 1000, "memory_mib": 512, "processes": 4,
                "output_bytes": 1048576, "wall_time_ms": 60000, "spend_microunits": 0
            },
            "attempt_cursor": "5"
        })
    );
}

async fn worker_binding_fixture(
    catalog: &str,
) -> (tempfile::TempDir, Engine, EffectiveConfig, WorkPlan) {
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let global = directory.path().join("global.toml");
    std::fs::write(&global, catalog).unwrap();
    let config = resolve_config(&global, &directory.path().join("project.toml")).unwrap();
    let context = context_for(&config, "project-a");
    let mut plan = pool_plan(context.clone(), "account-b", Some("worker-pool"));
    plan.tasks[0].route.preferred_target = None;
    plan.tasks[0].route.required_target = Some(CredentialRef::new("profile-b").unwrap());
    let engine = Engine::open(&directory.path().join("engine.sqlite"), config.clone())
        .await
        .unwrap();
    create_task(&engine, context).await;
    assert_eq!(plan.config_id, config.id);
    assert_eq!(plan.tasks[0].role, Role::Worker);
    (directory, engine, config, plan)
}

async fn approve_worker_binding_plan(engine: &Engine, plan: &WorkPlan) {
    for (index, command) in [
        Command::ProposePlan { plan: plan.clone() },
        Command::OwnerCoordination {
            action: OwnerAction::ApprovePlan {
                plan_id: plan.id.clone(),
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
}

fn worker_reserve_request(plan: &WorkPlan) -> WireRequest {
    request(
        Command::OwnerCoordination {
            action: OwnerAction::Reserve {
                plan_id: plan.id.clone(),
                item_id: plan.tasks[0].id.clone(),
            },
        },
        5,
        4,
    )
}

async fn worker_binding_state(
    engine: &Engine,
    plan: &WorkPlan,
    sequence: u64,
    revision: u64,
) -> crazyagent::coordination::PlanState {
    let reply = engine
        .apply(request(
            Command::CoordinationState {
                plan_id: plan.id.clone(),
            },
            sequence,
            revision,
        ))
        .await
        .unwrap();
    assert_eq!(reply.revision.value(), revision);
    let Outcome::Coordination { state } = reply.outcome else {
        panic!("actual worker coordination state required")
    };
    assert_eq!(state.plan, *plan);
    *state
}

#[tokio::test]
async fn worker_binding_missing_account_connection_allocates_nothing() {
    let catalog = CATALOG.replace("account_ref = \"account-b\"", "account_ref = \"account-c\"");
    let (_directory, engine, _config, plan) = worker_binding_fixture(&catalog).await;
    let reply = engine
        .apply(request(Command::ProposePlan { plan: plan.clone() }, 3, 2))
        .await
        .unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "route_pool_account_denied".into()
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

#[tokio::test]
async fn worker_binding_ambiguous_account_connections_preserve_unallocated_state() {
    let catalog = CATALOG.replace("account_ref = \"account-a\"", "account_ref = \"account-b\"");
    let (_directory, engine, _config, plan) = worker_binding_fixture(&catalog).await;
    approve_worker_binding_plan(&engine, &plan).await;
    let before = worker_binding_state(&engine, &plan, 6, 4).await;
    assert!(before.reservations.is_empty());
    let reply = engine.apply(worker_reserve_request(&plan)).await.unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "route_pool_connection_ambiguous".into(),
        }
    );
    assert_eq!(reply.revision.value(), 4);
    assert_eq!(worker_binding_state(&engine, &plan, 7, 4).await, before);
    engine.close().await.unwrap();
}

#[tokio::test]
async fn worker_binding_selection_uses_plan_config_after_catalog_reload() {
    let (directory, engine, pinned, plan) = worker_binding_fixture(CATALOG).await;
    approve_worker_binding_plan(&engine, &plan).await;
    std::fs::write(directory.path().join("global.toml"), "").unwrap();
    let current = resolve_config(
        &directory.path().join("global.toml"),
        &directory.path().join("project.toml"),
    )
    .unwrap();
    assert_ne!(current.id, pinned.id);
    assert!(current.settings.model_access.is_none());
    engine.update_config(current).await.unwrap();
    let reply = engine.apply(worker_reserve_request(&plan)).await.unwrap();
    assert_eq!(reply.outcome, Outcome::Applied);
    assert_eq!(reply.revision.value(), 5);
    let state = worker_binding_state(&engine, &plan, 6, 5).await;
    let value = serde_json::to_value(&state.reservations[&plan.tasks[0].id]).unwrap();
    assert_eq!(value["binding"]["config_id"], json!(pinned.id));
    assert_eq!(
        value["binding"]["connection"],
        serde_json::to_value(&pinned.settings.model_access.as_ref().unwrap().connections[1],)
            .unwrap()
    );
    engine.close().await.unwrap();
}

#[tokio::test]
async fn worker_binding_replay_and_restart_preserve_exact_snapshot() {
    let (directory, engine, pinned, plan) = worker_binding_fixture(CATALOG).await;
    approve_worker_binding_plan(&engine, &plan).await;
    let reserve = worker_reserve_request(&plan);
    let committed = engine.apply(reserve.clone()).await.unwrap();
    assert_eq!(committed.outcome, Outcome::Applied);
    assert_eq!(committed.revision.value(), 5);
    let before = worker_binding_state(&engine, &plan, 6, 5).await;
    assert!(
        serde_json::to_value(&before.reservations[&plan.tasks[0].id]).unwrap()["binding"]
            .is_object()
    );
    assert_eq!(engine.apply(reserve.clone()).await.unwrap(), committed);
    assert_eq!(worker_binding_state(&engine, &plan, 7, 5).await, before);
    let changed = CATALOG
        .replace("other-service", "replacement-service")
        .replace(":11435/v1", ":11436/v1")
        .replace("example/model", "replacement/model")
        .replace("account_ref = \"account-b\"", "account_ref = \"account-c\"")
        .replace(
            "auth = { method = \"none\" }",
            "auth = { method = \"api_key\", credential_ref = \"reference-only-key\" }",
        );
    std::fs::write(directory.path().join("global.toml"), changed).unwrap();
    let current = resolve_config(
        &directory.path().join("global.toml"),
        &directory.path().join("project.toml"),
    )
    .unwrap();
    assert_ne!(current.id, pinned.id);
    assert_ne!(current.settings.model_access, pinned.settings.model_access);
    engine.update_config(current.clone()).await.unwrap();
    engine.close().await.unwrap();
    let engine = Engine::open(&directory.path().join("engine.sqlite"), current)
        .await
        .unwrap();
    assert_eq!(worker_binding_state(&engine, &plan, 8, 5).await, before);
    assert_eq!(engine.apply(reserve).await.unwrap(), committed);
    let duplicate = request(
        Command::OwnerCoordination {
            action: OwnerAction::Reserve {
                plan_id: plan.id.clone(),
                item_id: plan.tasks[0].id.clone(),
            },
        },
        9,
        5,
    );
    let reply = engine.apply(duplicate).await.unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "already_reserved".into()
        }
    );
    assert_eq!(reply.revision.value(), 5);
    assert_eq!(worker_binding_state(&engine, &plan, 10, 5).await, before);
    let reply = engine
        .apply(request(
            Command::TaskConfig {
                task_id: plan.scope.task_id.clone(),
            },
            11,
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
    assert_eq!(reply.revision.value(), 5);
    engine.close().await.unwrap();
}

#[tokio::test]
async fn worker_binding_revocation_before_reserve_leaves_no_allocation() {
    let (directory, engine, _config, plan) = worker_binding_fixture(CATALOG).await;
    approve_worker_binding_plan(&engine, &plan).await;
    let before = worker_binding_state(&engine, &plan, 6, 4).await;
    std::fs::write(
        directory.path().join("project.toml"),
        "revoked_destinations=['profile-b']",
    )
    .unwrap();
    let revoked = resolve_config(
        &directory.path().join("global.toml"),
        &directory.path().join("project.toml"),
    )
    .unwrap();
    engine.update_config(revoked).await.unwrap();
    let reply = engine.apply(worker_reserve_request(&plan)).await.unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "plan_authority_revoked".into()
        }
    );
    assert_eq!(reply.revision.value(), 4);
    assert!(before.reservations.is_empty());
    assert_eq!(worker_binding_state(&engine, &plan, 7, 4).await, before);
    engine.close().await.unwrap();
}

#[tokio::test]
async fn worker_binding_snapshot_never_overrides_live_revocation_or_native_refusal() {
    let (directory, engine, _config, plan) = worker_binding_fixture(CATALOG).await;
    approve_worker_binding_plan(&engine, &plan).await;
    let reserve = worker_reserve_request(&plan);
    assert_eq!(
        engine.apply(reserve.clone()).await.unwrap().outcome,
        Outcome::Applied
    );
    let before = worker_binding_state(&engine, &plan, 6, 5).await;
    let route = || Command::RouteWorkItem {
        plan_id: plan.id.clone(),
        item_id: plan.tasks[0].id.clone(),
    };
    let reply = engine.apply(request(route(), 7, 5)).await.unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "native_admission_unavailable".into()
        }
    );
    assert_eq!(reply.revision.value(), 5);
    std::fs::write(
        directory.path().join("project.toml"),
        "revoked_destinations=['profile-b']",
    )
    .unwrap();
    let revoked = resolve_config(
        &directory.path().join("global.toml"),
        &directory.path().join("project.toml"),
    )
    .unwrap();
    engine.update_config(revoked).await.unwrap();
    assert_eq!(
        engine.apply(reserve).await.unwrap().outcome,
        Outcome::Applied
    );
    let reply = engine.apply(request(route(), 8, 5)).await.unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "plan_authority_revoked".into()
        }
    );
    assert_eq!(reply.revision.value(), 5);
    assert_eq!(worker_binding_state(&engine, &plan, 9, 5).await, before);
    engine.close().await.unwrap();
}

#[tokio::test]
async fn unsupported_worker_binding_combinations_remain_unbound_and_unavailable() {
    let catalog = CATALOG.replace(
        "roles = [\"worker\"]",
        "roles = [\"worker\", \"project_leader\"]",
    );
    for variant in 0..3 {
        let (_directory, engine, _config, mut plan) = worker_binding_fixture(&catalog).await;
        match variant {
            0 => plan.tasks[0].pool_ref = None,
            1 => plan.tasks[0].route.required_target = None,
            2 => plan.tasks[0].role = Role::ProjectLeader,
            _ => unreachable!(),
        }
        approve_worker_binding_plan(&engine, &plan).await;
        let reply = engine.apply(worker_reserve_request(&plan)).await.unwrap();
        assert_eq!(reply.outcome, Outcome::Applied);
        assert_eq!(reply.revision.value(), 5);
        let state = worker_binding_state(&engine, &plan, 6, 5).await;
        let value = serde_json::to_value(&state.reservations[&plan.tasks[0].id]).unwrap();
        assert!(value.get("binding").is_none());
        let reply = engine
            .apply(request(
                Command::RouteWorkItem {
                    plan_id: plan.id.clone(),
                    item_id: plan.tasks[0].id.clone(),
                },
                7,
                5,
            ))
            .await
            .unwrap();
        assert_eq!(
            reply.outcome,
            Outcome::Rejected {
                code: "native_admission_unavailable".into(),
            }
        );
        assert_eq!(reply.revision.value(), 5);
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn historical_reservation_without_binding_is_not_reconstructed_on_restart() {
    let (directory, engine, config, plan) = worker_binding_fixture(CATALOG).await;
    approve_worker_binding_plan(&engine, &plan).await;
    assert_eq!(
        engine
            .apply(worker_reserve_request(&plan))
            .await
            .unwrap()
            .outcome,
        Outcome::Applied,
    );
    let state = worker_binding_state(&engine, &plan, 6, 5).await;
    let mut legacy = serde_json::to_value(&state).unwrap();
    let reservation = legacy["reservations"]["item-a"].as_object_mut().unwrap();
    assert!(reservation.remove("binding").unwrap().is_object());
    let legacy_state: crazyagent::coordination::PlanState =
        serde_json::from_value(legacy.clone()).unwrap();
    engine.close().await.unwrap();
    let database = directory.path().join("engine.sqlite");
    let connection = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE coordination_plans SET body=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&legacy).unwrap(), plan.id.as_str()],
            )
            .unwrap(),
        1
    );
    drop(connection);
    let engine = Engine::open(&database, config).await.unwrap();
    let restored = worker_binding_state(&engine, &plan, 7, 5).await;
    assert_eq!(restored, legacy_state);
    assert_eq!(restored.reservations.len(), 1);
    assert_eq!(serde_json::to_value(&restored).unwrap(), legacy);
    let reply = engine
        .apply(request(
            Command::RouteWorkItem {
                plan_id: plan.id.clone(),
                item_id: plan.tasks[0].id.clone(),
            },
            8,
            5,
        ))
        .await
        .unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "native_admission_unavailable".into()
        }
    );
    assert_eq!(reply.revision.value(), 5);
    assert_eq!(worker_binding_state(&engine, &plan, 9, 5).await, restored);
    engine.close().await.unwrap();
}

#[tokio::test]
async fn ambiguous_worker_binding_cannot_block_a_valid_planning_allocation() {
    let catalog = CATALOG.replace("account_ref = \"account-a\"", "account_ref = \"account-b\"");
    let (_directory, engine, _config, mut plan) = worker_binding_fixture(&catalog).await;
    let mut other = plan.tasks[0].clone();
    other.id = TaskId::new("item-b").unwrap();
    other.session_id = SessionId::new("session-b").unwrap();
    other.pool_ref = None;
    plan.tasks.push(other.clone());
    approve_worker_binding_plan(&engine, &plan).await;
    let reply = engine
        .apply(request(
            Command::OwnerCoordination {
                action: OwnerAction::Reserve {
                    plan_id: plan.id.clone(),
                    item_id: other.id.clone(),
                },
            },
            5,
            4,
        ))
        .await
        .unwrap();
    assert_eq!(reply.outcome, Outcome::Applied);
    assert_eq!(reply.revision.value(), 5);
    let before = worker_binding_state(&engine, &plan, 6, 5).await;
    assert_eq!(before.reservations.len(), 1);
    assert!(!before.reservations.contains_key(&plan.tasks[0].id));
    let reservation = serde_json::to_value(&before.reservations[&other.id]).unwrap();
    assert!(reservation.get("binding").is_none());
    let reply = engine
        .apply(request(
            Command::OwnerCoordination {
                action: OwnerAction::Reserve {
                    plan_id: plan.id.clone(),
                    item_id: plan.tasks[0].id.clone(),
                },
            },
            7,
            5,
        ))
        .await
        .unwrap();
    assert_eq!(
        reply.outcome,
        Outcome::Rejected {
            code: "route_pool_connection_ambiguous".into(),
        }
    );
    assert_eq!(reply.revision.value(), 5);
    assert_eq!(worker_binding_state(&engine, &plan, 8, 5).await, before);
    engine.close().await.unwrap();
}

#[tokio::test]
async fn imp1038_unassigned_and_partial_pools_cannot_persist_a_plan() {
    for (projects, roles) in [
        ("[]", "[]"),
        ("[]", "[\"worker\"]"),
        ("[\"project-a\"]", "[]"),
    ] {
        let catalog = CATALOG
            .replace(
                "projects = [\"project-a\"]",
                &format!("projects = {projects}"),
            )
            .replace("roles = [\"worker\"]", &format!("roles = {roles}"));
        rejected_pool(
            "project-a",
            "account-a",
            "worker-pool",
            &catalog,
            "route_pool_scope_denied",
        )
        .await;
    }
}
