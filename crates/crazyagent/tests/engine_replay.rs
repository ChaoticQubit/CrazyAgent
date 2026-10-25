#![cfg(unix)]

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

mod project_creation {
    use super::*;
    use crazyagent::engine::bootstrap::EngineService;
    use crazyagent::engine::owner::{
        OwnerClient, OwnerClientKind, OwnerOperation, OwnerRequest, OwnerResponse, OwnerResult,
    };
    use std::path::Path;
    use std::time::Duration;

    fn specification_cases() -> Vec<(&'static str, ProjectSpec, bool)> {
        [
            ("empty legacy repositories", "Project".into(), vec![], true),
            ("name spacing retained", " Project ".into(), vec![], true),
            ("ASCII name limit", "x".repeat(256), vec![], true),
            ("UTF-8 name limit", "é".repeat(128), vec![], true),
            (
                "repository count limit",
                "P".into(),
                vec!["/repo".into(); 64],
                true,
            ),
            (
                "repository byte limit",
                "P".into(),
                vec!["x".repeat(4096)],
                true,
            ),
            (
                "UTF-8 repository limit",
                "P".into(),
                vec!["é".repeat(2048)],
                true,
            ),
            (
                "relative repository",
                "P".into(),
                vec!["relative/repo".into()],
                true,
            ),
            ("space repository", "P".into(), vec![" ".into()], true),
            (
                "duplicate declarations",
                "P".into(),
                vec!["/repo".into(); 2],
                true,
            ),
            (
                "colon without scheme",
                "P".into(),
                vec!["host:repo".into()],
                true,
            ),
            ("empty name", "".into(), vec![], false),
            ("blank name", "   ".into(), vec![], false),
            ("Unicode blank name", "\u{2003}".into(), vec![], false),
            ("ASCII name overflow", "x".repeat(257), vec![], false),
            ("UTF-8 name overflow", "é".repeat(129), vec![], false),
            ("name newline", "P\n".into(), vec![], false),
            ("name null", "P\0".into(), vec![], false),
            ("name DEL", "P\u{7f}".into(), vec![], false),
            ("name C1 control", "P\u{85}".into(), vec![], false),
            ("empty repository", "P".into(), vec!["".into()], false),
            (
                "ASCII repository overflow",
                "P".into(),
                vec!["x".repeat(4097)],
                false,
            ),
            (
                "UTF-8 repository overflow",
                "P".into(),
                vec!["é".repeat(2048) + "x"],
                false,
            ),
            (
                "repository newline",
                "P".into(),
                vec!["/repo\n".into()],
                false,
            ),
            ("repository null", "P".into(), vec!["/repo\0".into()], false),
            (
                "repository DEL",
                "P".into(),
                vec!["/repo\u{7f}".into()],
                false,
            ),
            (
                "repository C1 control",
                "P".into(),
                vec!["/repo\u{85}".into()],
                false,
            ),
            (
                "repository count overflow",
                "P".into(),
                vec!["/repo".into(); 65],
                false,
            ),
            (
                "remote scheme",
                "P".into(),
                vec!["https://host/repo".into()],
                false,
            ),
            (
                "local scheme",
                "P".into(),
                vec!["file:///repo".into()],
                false,
            ),
        ]
        .into_iter()
        .map(|(label, name, repositories, accepted)| {
            (label, ProjectSpec { name, repositories }, accepted)
        })
        .collect()
    }

    fn owner_creation(spec: &ProjectSpec) -> Result<OwnerRequest, serde_json::Error> {
        serde_json::from_value(serde_json::json!({
            "protocol_version": 1,
            "request_id": "owner-create-project",
            "expected_revision": "0",
            "operation": {
                "kind": "Apply",
                "mutation": {
                    "kind": "CreateProject",
                    "project_id": "owner-project",
                    "spec": spec
                }
            }
        }))
    }

    fn legacy_creation(spec: ProjectSpec) -> WireRequest {
        request(
            "legacy-create-project",
            0,
            Command::CreateProject {
                project_id: ProjectId::new("legacy-project").unwrap(),
                spec,
            },
        )
    }

    #[test]
    fn legacy_specification_acceptance_is_unchanged() {
        for (label, spec, accepted) in specification_cases() {
            let wire = legacy_creation(spec);
            assert!(serde_json::to_vec(&wire).unwrap().len() < FRAME_BYTES_MAX);
            assert_eq!(wire.validate().is_ok(), accepted, "{label}");
        }
    }

    #[test]
    fn owner_specification_acceptance_matches_legacy() {
        for (label, spec, accepted) in specification_cases() {
            let owner = owner_creation(&spec)
                .expect("closed owner CreateProject must deserialize before validation");
            assert_eq!(owner.validate().is_ok(), accepted, "owner: {label}");
            assert_eq!(
                legacy_creation(spec).validate().is_ok(),
                accepted,
                "legacy: {label}"
            );
        }
    }

    fn frame_specification() -> ProjectSpec {
        let mut repositories = vec!["x".repeat(4096); 15];
        repositories.push("/".into());
        ProjectSpec {
            name: "Frame boundary".into(),
            repositories,
        }
    }

    #[test]
    fn legacy_creation_enforces_exact_frame_boundary() {
        let mut wire = legacy_creation(frame_specification());
        let padding = FRAME_BYTES_MAX - serde_json::to_vec(&wire).unwrap().len();
        let Command::CreateProject { spec, .. } = &mut wire.command else {
            panic!("legacy creation required");
        };
        spec.repositories[15].push_str(&"x".repeat(padding));
        assert!(spec.repositories[15].len() <= 4096);
        assert_eq!(serde_json::to_vec(&wire).unwrap().len(), FRAME_BYTES_MAX);
        assert!(wire.validate().is_ok());
        assert_eq!(encode_frame(&wire).unwrap().len(), FRAME_BYTES_MAX + 4);
        let Command::CreateProject { spec, .. } = &mut wire.command else {
            panic!("legacy creation required");
        };
        spec.repositories[15].push('x');
        assert_eq!(
            serde_json::to_vec(&wire).unwrap().len(),
            FRAME_BYTES_MAX + 1
        );
        assert!(wire.validate().is_err());
        assert!(encode_frame(&wire).is_err());
    }

    #[test]
    fn owner_creation_enforces_exact_frame_boundary() {
        let owner = owner_creation(&frame_specification())
            .expect("closed owner CreateProject must deserialize before frame validation");
        let padding = FRAME_BYTES_MAX - serde_json::to_vec(&owner).unwrap().len();
        let mut value = serde_json::to_value(&owner).unwrap();
        value["operation"]["mutation"]["spec"]["repositories"][15] =
            ("/".to_owned() + &"x".repeat(padding)).into();
        let owner: OwnerRequest = serde_json::from_value(value).unwrap();
        assert_eq!(serde_json::to_vec(&owner).unwrap().len(), FRAME_BYTES_MAX);
        assert!(owner.validate().is_ok());
        assert_eq!(encode_frame(&owner).unwrap().len(), FRAME_BYTES_MAX + 4);
        let mut value = serde_json::to_value(&owner).unwrap();
        let repository = value["operation"]["mutation"]["spec"]["repositories"][15]
            .as_str()
            .unwrap()
            .to_owned();
        value["operation"]["mutation"]["spec"]["repositories"][15] = (repository + "x").into();
        let oversized: OwnerRequest = serde_json::from_value(value).unwrap();
        assert_eq!(
            serde_json::to_vec(&oversized).unwrap().len(),
            FRAME_BYTES_MAX + 1
        );
        assert!(oversized.validate().is_err());
        assert!(encode_frame(&oversized).is_err());
    }

    #[test]
    fn owner_creation_keeps_identity_and_unknown_fields_closed() {
        let spec = ProjectSpec {
            name: "Closed input".into(),
            repositories: vec![],
        };
        let owner = owner_creation(&spec)
            .expect("closed owner CreateProject must deserialize before closed-shape checks");
        let value = serde_json::to_value(owner).unwrap();
        for project_id in [String::new(), "bad id".into(), "x".repeat(129)] {
            let mut invalid = value.clone();
            invalid["operation"]["mutation"]["project_id"] = project_id.into();
            assert!(serde_json::from_value::<OwnerRequest>(invalid).is_err());
        }
        for pointer in ["/operation/mutation", "/operation/mutation/spec"] {
            let mut invalid = value.clone();
            invalid
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("admit_repository".into(), true.into());
            assert!(serde_json::from_value::<OwnerRequest>(invalid).is_err());
        }
    }

    async fn start_engine(root: &Path) -> EngineService {
        let config = resolve_config(&root.join("g"), &root.join("p")).unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        EngineService::start(root, "engine.sock", &config, &stopped)
            .await
            .unwrap()
    }

    async fn desktop(root: &Path) -> OwnerClient {
        OwnerClient::discover(root, OwnerClientKind::Desktop, Duration::from_secs(2))
            .await
            .unwrap()
            .0
    }

    async fn reconcile(client: &OwnerClient, create: &OwnerRequest) -> OwnerResponse {
        client
            .request(OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new("after-restart-reconcile").unwrap(),
                expected_revision: Cursor::new(0),
                operation: OwnerOperation::Reconcile {
                    operation_id: create.request_id.clone(),
                    expected_hash: Some(create.mutation_hash().unwrap()),
                },
            })
            .await
            .unwrap()
            .response
    }

    fn assert_persisted_project(root: &Path, spec: ProjectSpec) {
        let db = rusqlite::Connection::open(root.join("engine.db")).unwrap();
        let body: String = db
            .query_row(
                "SELECT body FROM projects WHERE id='owner-project'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let stored: StoredProject = serde_json::from_str(&body).unwrap();
        assert_eq!(stored.project_id, ProjectId::new("owner-project").unwrap());
        assert_eq!(stored.spec, spec);
        for table in ["projects", "events", "owner_operations"] {
            let count: i64 = db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1, "{table}");
        }
    }

    #[tokio::test]
    async fn owner_project_survives_restart_and_exact_replay() {
        let spec = ProjectSpec {
            name: "Durable owner project".into(),
            repositories: vec!["relative/declaration-only".into()],
        };
        let create = owner_creation(&spec)
            .expect("closed owner CreateProject must deserialize before engine execution");
        let root = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut service = start_engine(root.path()).await;
        let old = desktop(root.path()).await;
        let OwnerResponse::Applied { operation: first } =
            old.request(create.clone()).await.unwrap().response
        else {
            panic!("real owner creation receipt required");
        };
        assert_eq!(first.result, OwnerResult::Applied);
        assert!(!first.historical_replay);
        assert_eq!(first.provenance.revision_after, Cursor::new(1));
        service.shutdown().await.unwrap();
        let mut service = start_engine(root.path()).await;
        assert!(old.request(create.clone()).await.is_err());
        let client = desktop(root.path()).await;
        let OwnerResponse::Committed { operation } = reconcile(&client, &create).await else {
            panic!("persisted owner receipt required");
        };
        assert_eq!(operation.provenance, first.provenance);
        assert_eq!(operation.result, first.result);
        let OwnerResponse::Applied { operation } = client.request(create).await.unwrap().response
        else {
            panic!("exact owner replay required");
        };
        assert!(operation.historical_replay);
        assert_eq!(operation.provenance, first.provenance);
        assert_eq!(operation.current_revision, Cursor::new(1));
        service.shutdown().await.unwrap();
        assert_persisted_project(root.path(), spec);
    }
}
