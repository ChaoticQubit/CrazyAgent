use crazyagent::{config::resolve_config, engine::Engine, protocol::*};
use std::os::unix::fs::PermissionsExt;

fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn request(id: &str, revision: u64, command: Command) -> WireRequest {
    WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("desktop").unwrap(),
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        command,
    }
}

fn create(id: &str) -> Command {
    Command::CreateProject {
        project_id: ProjectId::new(id).unwrap(),
        spec: ProjectSpec {
            name: "Inventory".into(),
            repositories: vec!["/tmp/inventory".into()],
        },
    }
}

#[tokio::test]
async fn replay_after_lost_ack_executes_once() {
    let root = private_root();
    let config = resolve_config(&root.path().join("global"), &root.path().join("project")).unwrap();
    let db = root.path().join("engine.db");
    let engine = Engine::open(&db, config.clone()).await.unwrap();
    let operation = request("create-1", 0, create("project-1"));
    let first = engine.apply(operation.clone()).await.unwrap();
    engine.close().await.unwrap();
    drop(engine);
    let engine = Engine::open(&db, config).await.unwrap();
    let replay = engine.apply(operation.clone()).await.unwrap();
    assert_eq!(first, replay);
    assert_eq!(replay.revision, Cursor::new(1));
    let snapshot = engine
        .apply(request("snapshot", 0, Command::Snapshot))
        .await
        .unwrap();
    let Outcome::Snapshot { projects, .. } = snapshot.outcome else {
        panic!("snapshot required")
    };
    assert_eq!(projects.len(), 1);
    let mut changed = operation;
    changed.command = create("different-project");
    assert!(engine.apply(changed).await.is_err());
    let stale = engine
        .apply(request("stale", 0, create("project-2")))
        .await
        .unwrap();
    assert!(matches!(stale.outcome, Outcome::Rejected { .. }));
    engine.close().await.unwrap();
}

#[tokio::test]
async fn sqlite_write_failure_does_not_acknowledge_success() {
    let root = private_root();
    let cfg = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let db = root.path().join("engine.db");
    let engine = Engine::open(&db, cfg.clone()).await.unwrap();
    engine.close().await.unwrap();
    let readonly = Engine::open_readonly(&db, cfg).await.unwrap();
    assert!(
        readonly
            .apply(request("fail", 0, create("project")))
            .await
            .is_err()
    );
    readonly.close().await.unwrap();
}

#[tokio::test]
async fn task_config_is_pinned_and_revocations_are_immediate() {
    let root = private_root();
    let g = root.path().join("global.toml");
    let p = root.path().join("project.toml");
    std::fs::write(&g, "allowed_destinations = ['local-profile']").unwrap();
    let original = resolve_config(&g, &p).unwrap();
    let engine = Engine::open(&root.path().join("engine.db"), original.clone())
        .await
        .unwrap();
    engine
        .apply(request("project", 0, create("project")))
        .await
        .unwrap();
    let context = ExecutionContext {
        project_id: ProjectId::new("project").unwrap(),
        task_id: TaskId::new("task").unwrap(),
        config_id: original.id.clone(),
        snapshot_id: SnapshotId::new("snapshot").unwrap(),
        baseline_id: BaselineId::new("baseline").unwrap(),
        allowed_destinations: vec![CredentialRef::new("local-profile").unwrap()],
        budget_ref: CredentialRef::new("approved-budget").unwrap(),
    };
    engine
        .apply(request(
            "pin",
            1,
            Command::PinTask {
                context: context.clone(),
            },
        ))
        .await
        .unwrap();
    let get_task = request(
        "same-task-read",
        0,
        Command::TaskContext {
            task_id: context.task_id.clone(),
        },
    );
    let before = engine.apply(get_task.clone()).await.unwrap();
    assert!(
        matches!(before.outcome, Outcome::Task { context } if context.allowed_destinations.len() == 1)
    );
    std::fs::write(
        &p,
        "timeout_ms = 1500\nrevoked_destinations = ['local-profile']",
    )
    .unwrap();
    let latest = resolve_config(&g, &p).unwrap();
    engine.update_config(latest.clone()).await.unwrap();
    let after = engine.apply(get_task).await.unwrap();
    assert!(
        matches!(after.outcome, Outcome::Task { context } if context.allowed_destinations.is_empty())
    );
    let task = engine
        .apply(request(
            "get-task",
            0,
            Command::TaskContext {
                task_id: context.task_id.clone(),
            },
        ))
        .await
        .unwrap();
    let Outcome::Task { context: pinned } = task.outcome else {
        panic!("task required")
    };
    assert_eq!(pinned.config_id, original.id);
    assert!(pinned.allowed_destinations.is_empty());
    engine.close().await.unwrap();
    drop(engine);
    let engine = Engine::open(&root.path().join("engine.db"), latest.clone())
        .await
        .unwrap();
    let old_settings = engine
        .apply(request(
            "old-settings",
            0,
            Command::TaskConfig {
                task_id: context.task_id.clone(),
            },
        ))
        .await
        .unwrap();
    let Outcome::Configuration {
        config: old_settings,
    } = old_settings.outcome
    else {
        panic!("pinned settings required")
    };
    assert_eq!(
        old_settings.settings.timeout_ms,
        original.settings.timeout_ms
    );
    engine
        .apply(request(
            "migrate",
            2,
            Command::MigrateTask {
                task_id: context.task_id.clone(),
                config_id: latest.id.clone(),
            },
        ))
        .await
        .unwrap();
    let task = engine
        .apply(request(
            "get-migrated",
            0,
            Command::TaskContext {
                task_id: context.task_id,
            },
        ))
        .await
        .unwrap();
    let Outcome::Task { context: migrated } = task.outcome else {
        panic!("task required")
    };
    assert_eq!(migrated.config_id, latest.id);
    engine.close().await.unwrap();
}

#[tokio::test]
async fn operation_identity_is_scoped_by_client() {
    let root = private_root();
    let g = root.path().join("g");
    let p = root.path().join("p");
    let cfg = resolve_config(&g, &p).unwrap();
    let engine = Engine::open(&root.path().join("db"), cfg).await.unwrap();
    let first = request("same-id", 0, create("p1"));
    assert_eq!(
        engine.apply(first.clone()).await.unwrap().revision,
        Cursor::new(1)
    );
    let mut other = first;
    other.client_id = ClientId::new("other-client").unwrap();
    other.expected_revision = Cursor::new(1);
    other.command = create("p2");
    assert_eq!(engine.apply(other).await.unwrap().revision, Cursor::new(2));
    engine.close().await.unwrap();
}

#[tokio::test]
async fn cursor_gap_requires_snapshot() {
    let root = private_root();
    let g = root.path().join("g");
    std::fs::write(&g, "events_retained = 1").unwrap();
    let cfg = resolve_config(&g, &root.path().join("p")).unwrap();
    let engine = Engine::open(&root.path().join("db"), cfg).await.unwrap();
    for i in 0..3 {
        engine
            .apply(request(&format!("op-{i}"), i, create(&format!("p-{i}"))))
            .await
            .unwrap();
    }
    let reply = engine
        .apply(request(
            "gap",
            0,
            Command::Events {
                after: Cursor::new(0),
            },
        ))
        .await
        .unwrap();
    assert_eq!(reply.outcome, Outcome::ResyncRequired);
    let reply = engine
        .apply(request(
            "next",
            0,
            Command::Events {
                after: Cursor::new(2),
            },
        ))
        .await
        .unwrap();
    let Outcome::Events { events } = reply.outcome else {
        panic!("events required")
    };
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].cursor, Cursor::new(3));
    engine.close().await.unwrap();
}

#[tokio::test]
async fn one_engine_owns_database_and_config_identity_is_checked() {
    let root = private_root();
    let cfg = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let path = root.path().join("db");
    let first = Engine::open(&path, cfg.clone()).await.unwrap();
    assert!(Engine::open(&path, cfg.clone()).await.is_err());
    let mut forged = cfg;
    forged.settings.timeout_ms += 1;
    assert!(first.update_config(forged).await.is_err());
    first.close().await.unwrap();
}

#[tokio::test]
async fn real_sqlite_contention_and_queue_saturation_are_bounded() {
    let root = private_root();
    let g = root.path().join("g");
    std::fs::write(&g, "queue_capacity = 1").unwrap();
    let cfg = resolve_config(&g, &root.path().join("p")).unwrap();
    let path = root.path().join("db");
    let engine = std::sync::Arc::new(Engine::open(&path, cfg).await.unwrap());
    let blocker = rusqlite::Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut jobs = tokio::task::JoinSet::new();
    for i in 0..16 {
        let engine = engine.clone();
        jobs.spawn(async move {
            engine
                .apply(request(
                    &format!("saturation-{i}"),
                    0,
                    create(&format!("p-{i}")),
                ))
                .await
        });
    }
    let mut full = 0;
    while let Some(result) = jobs.join_next().await {
        let error = result.unwrap().unwrap_err();
        if error.0.contains("queue unavailable") {
            full += 1;
        }
    }
    assert!(full > 0);
    blocker.execute_batch("ROLLBACK").unwrap();
    let reply = engine
        .apply(request("after-contention", 0, create("real")))
        .await
        .unwrap();
    assert_eq!(reply.revision, Cursor::new(1));
    engine.close().await.unwrap();
}

#[tokio::test]
async fn state_quota_failure_rolls_back() {
    let root = private_root();
    let cfg = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    let path = root.path().join("db");
    let engine = Engine::open(&path, cfg.clone()).await.unwrap();
    let mut big = request("large", 0, create("large"));
    if let Command::CreateProject { spec, .. } = &mut big.command {
        spec.repositories = vec!["/".to_owned() + &"x".repeat(3999); 15];
    }
    assert_eq!(
        engine.apply(big.clone()).await.unwrap().revision,
        Cursor::new(1)
    );
    big.request_id = RequestId::new("overflow").unwrap();
    big.expected_revision = Cursor::new(1);
    if let Command::CreateProject { project_id, .. } = &mut big.command {
        *project_id = ProjectId::new("overflow").unwrap();
    }
    assert!(engine.apply(big).await.is_err());
    engine.close().await.unwrap();
    let engine = Engine::open(&path, cfg).await.unwrap();
    let reply = engine
        .apply(request("after-quota", 0, Command::Snapshot))
        .await
        .unwrap();
    assert_eq!(reply.revision, Cursor::new(1));
    let Outcome::Snapshot { projects, .. } = reply.outcome else {
        panic!("snapshot required")
    };
    assert_eq!(projects.len(), 1);
    engine.close().await.unwrap();
}
