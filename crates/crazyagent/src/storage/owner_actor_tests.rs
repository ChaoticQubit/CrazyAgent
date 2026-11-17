use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn gateway_actor_schema_initialization_is_writable_only() {
    let (_root, path, _) = fixture();
    let (db, lock) = open_database(&path, false).unwrap();
    assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name IN ('model_gateway_observations','model_gateway_attempts')",
                [],
                |row| row.get::<_, i64>(0)
            ).unwrap(),
            2
        );
    db.execute_batch("DROP TABLE model_gateway_attempts; DROP TABLE model_gateway_observations;")
        .unwrap();
    drop(db);
    drop(lock);
    let (db, lock) = open_database(&path, true).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name LIKE 'model_gateway_%'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(db);
    drop(lock);
    let (db, _lock) = open_database(&path, false).unwrap();
    assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name IN ('model_gateway_observations','model_gateway_attempts')",
                [],
                |row| row.get::<_, i64>(0)
            ).unwrap(),
            2
        );
}

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, EffectiveConfig) {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("db");
    let cfg =
        crate::config::resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    (root, path, cfg)
}

async fn start(storage: &Storage) -> Started {
    let (tx, rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerStart(tx))
        .unwrap_or_else(|_| panic!("queue"));
    rx.await.unwrap().unwrap()
}

#[test]
fn settings_reader_asserts_impossible_internal_metadata_corruption() {
    for case in 0..4 {
        let (_root, path, mut cfg) = fixture();
        let (mut db, _lock) = open_database(&path, false).unwrap();
        let started = owner_start(&mut db).unwrap();
        let owner = &started.owners[0].admission;
        let query = serde_json::from_value(serde_json::json!({"kind":"Settings"})).unwrap();
        assert!(owner_read(&mut db, &cfg, &path, owner, &query).is_ok());
        match case {
            0 => cfg.descriptors.clear(),
            1 | 3 => {
                let descriptor = cfg.descriptors.values().next().unwrap().clone();
                for index in 0..26 {
                    cfg.descriptors
                        .insert(format!("invalid-{index}"), descriptor.clone());
                }
            }
            _ => {
                cfg.descriptors.get_mut("timeout_ms").unwrap().name = "corrupt".into();
            }
        }
        let query = if case >= 2 {
            serde_json::from_value(serde_json::json!({"kind":"Setting","name":"timeout_ms"}))
                .unwrap()
        } else {
            query
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            owner_read(&mut db, &cfg, &path, owner, &query)
        }));
        assert!(
            outcome.is_err(),
            "internal corruption case {case} must trip an invariant"
        );
        let tx = db.transaction().unwrap();
        assert_eq!(owner_revision(&tx).unwrap(), 0);
    }
}

#[tokio::test]
async fn queued_owner_read_rechecks_epoch_after_revoke() {
    let (_root, path, cfg) = fixture();
    let storage = Storage::open(&path, cfg, false).await.unwrap();
    let started = start(&storage).await;
    let owner = started.owners[0].admission.clone();
    let control = OwnerControl {
        operation_id: RequestId::new("revoke").unwrap(),
        target: OwnerClientKind::Tui,
        expected_epoch: owner.owner_epoch,
        decision: OwnerDecision::Revoke,
    };
    let (tx, rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerControl(started.host, control, tx))
        .unwrap_or_else(|_| panic!("queue"));
    let (read_tx, read_rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerRead(owner, OwnerRead::Capabilities, read_tx))
        .unwrap_or_else(|_| panic!("queue"));
    assert!(!rx.await.unwrap().unwrap().0.active);
    assert!(read_rx.await.unwrap().is_err());
    let (tx, rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::Close(tx))
        .unwrap_or_else(|_| panic!("queue"));
    rx.await.unwrap().unwrap();
}

fn mutation(id: &str) -> OwnerRequest {
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(0),
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::Coordination {
                action: crate::coordination::OwnerAction::ApprovePlan {
                    plan_id: PlanId::new("missing").unwrap(),
                },
            },
        },
    }
}

#[tokio::test]
async fn fifo_apply_revoke_and_dropped_ack_have_real_actor_barriers() {
    let (_root, path, cfg) = fixture();
    use crate::verification::*;
    let draft = BaselineDraft {
        version: 1,
        project_id: ProjectId::new("p").unwrap(),
        origin: BaselineOrigin::Imported,
        requirements: "exact accepted checks".into(),
        checks: vec![
            CheckSpec {
                id: RequestId::new("code").unwrap(),
                kind: CheckKind::Code,
                minimum_risk: Risk::Low,
                content: b"code".to_vec(),
            },
            CheckSpec {
                id: RequestId::new("integrity").unwrap(),
                kind: CheckKind::TestIntegrity,
                minimum_risk: Risk::Low,
                content: b"integrity".to_vec(),
            },
        ],
        rules: vec![],
        retry_limit: 0,
    };
    let baseline = draft.identity().unwrap();
    {
        let (mut db, _lock) = open_database(&path, false).unwrap();
        for (n, command) in [
            Command::CreateProject {
                project_id: draft.project_id.clone(),
                spec: ProjectSpec {
                    name: "Project".into(),
                    repositories: vec![],
                },
            },
            Command::ProposeBaseline { draft },
        ]
        .into_iter()
        .enumerate()
        {
            let req = WireRequest {
                protocol_version: PROTOCOL_VERSION,
                client_id: ClientId::new("host").unwrap(),
                request_id: RequestId::new(format!("seed-{n}")).unwrap(),
                expected_revision: Cursor::new(n as u64),
                command,
            };
            assert_eq!(
                apply(&mut db, &cfg, &path, req).unwrap().outcome,
                Outcome::Applied
            );
        }
    }
    let storage = Storage::open(&path, cfg.clone(), false).await.unwrap();
    let started = start(&storage).await;
    let owner = started.owners[0].admission.clone();
    let req = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("lost-ack").unwrap(),
        expected_revision: Cursor::new(2),
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::ApproveBaseline {
                baseline_id: baseline.clone(),
            },
        },
    };
    let (tx, rx) = oneshot::channel();
    drop(rx);
    storage
        .sender
        .try_send(Job::OwnerApply(owner.clone(), req.clone(), tx))
        .unwrap_or_else(|_| panic!("queue"));
    let (tx, rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerReconcile(
            owner.clone(),
            req.request_id.clone(),
            Some(req.mutation_hash().unwrap()),
            tx,
        ))
        .unwrap_or_else(|_| panic!("queue"));
    let recovered = rx.await.unwrap().unwrap();
    let OwnerResponse::Committed {
        operation: original,
    } = recovered
    else {
        panic!("committed")
    };
    assert_eq!(original.result, OwnerResult::Applied);
    let control = OwnerControl {
        operation_id: RequestId::new("lost-ack").unwrap(),
        target: OwnerClientKind::Tui,
        expected_epoch: 1,
        decision: OwnerDecision::Revoke,
    };
    let (tx, rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerControl(started.host.clone(), control.clone(), tx))
        .unwrap_or_else(|_| panic!("queue"));
    let (late_tx, late_rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerApply(
            owner.clone(),
            mutation("after-revoke"),
            late_tx,
        ))
        .unwrap_or_else(|_| panic!("queue"));
    assert!(!rx.await.unwrap().unwrap().0.active);
    assert!(late_rx.await.unwrap().is_err());
    let (tx, rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerReconcile(
            owner.clone(),
            req.request_id.clone(),
            None,
            tx,
        ))
        .unwrap_or_else(|_| panic!("queue"));
    assert!(rx.await.unwrap().is_err());
    let reactivate = OwnerControl {
        operation_id: RequestId::new("reactivate").unwrap(),
        expected_epoch: 2,
        decision: OwnerDecision::Reactivate,
        ..control.clone()
    };
    let (tx, rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerControl(started.host, reactivate, tx))
        .unwrap_or_else(|_| panic!("queue"));
    assert!(rx.await.unwrap().unwrap().0.active);
    let (tx, rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::Close(tx))
        .unwrap_or_else(|_| panic!("queue"));
    rx.await.unwrap().unwrap();
    let storage = Storage::open(&path, cfg, false).await.unwrap();
    let restarted = start(&storage).await;
    assert_eq!(restarted.owners[0].admission.owner_id, owner.owner_id);
    assert_ne!(restarted.host.engine_epoch, owner.engine_epoch);
    let (tx, rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerApply(owner, req.clone(), tx))
        .unwrap_or_else(|_| panic!("queue"));
    assert!(rx.await.unwrap().is_err());
    let (tx, rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerApply(
            restarted.owners[0].admission.clone(),
            req,
            tx,
        ))
        .unwrap_or_else(|_| panic!("queue"));
    let replay = rx.await.unwrap().unwrap();
    assert!(replay.historical_replay);
    assert_eq!(replay.provenance, original.provenance);
    let (tx, rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::OwnerControlReconcile(restarted.host, control, tx))
        .unwrap_or_else(|_| panic!("queue"));
    assert!(!rx.await.unwrap().unwrap().unwrap().active);
    let (tx, rx) = oneshot::channel();
    storage
        .sender
        .try_send(Job::Close(tx))
        .unwrap_or_else(|_| panic!("queue"));
    rx.await.unwrap().unwrap();
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM owner_operations WHERE origin='native_owner'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM operations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row("SELECT revision FROM state", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        db.query_row(
            "SELECT approved FROM verification_baselines WHERE id=?1",
            [baseline.as_str()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn operation_history_keeps_a_recent_window_and_replays_recent_mutations() {
    let (_root, path, cfg) = fixture();
    let (mut db, _lock) = open_database(&path, false).unwrap();
    let request = |client: &str, id: String, revision: u64, command: Command| WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new(client).unwrap(),
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        command,
    };
    let create = |id: &str| Command::CreateProject {
        project_id: ProjectId::new(id).unwrap(),
        spec: ProjectSpec {
            name: "Project".into(),
            repositories: vec![],
        },
    };
    let rows = |db: &Connection, client: &str| -> (usize, usize) {
        db.query_row(
                "SELECT count(*), count(*) FILTER (WHERE length(reply)>0) FROM operations WHERE client=?1",
                [client],
                |row| Ok((row.get::<_, i64>(0)? as usize, row.get::<_, i64>(1)? as usize)),
            )
            .unwrap()
    };
    let created = request("tui-a", "create".into(), 0, create("p"));
    let first = apply(&mut db, &cfg, &path, created.clone()).unwrap();
    assert_eq!(first.outcome, Outcome::Applied);
    for n in 0..OPERATIONS_PER_CLIENT_MAX + 8 {
        let poll = request("tui-a", format!("poll-{n}"), 1, Command::Snapshot);
        assert!(matches!(
            apply(&mut db, &cfg, &path, poll).unwrap().outcome,
            Outcome::Snapshot { .. }
        ));
    }
    assert_eq!(rows(&db, "tui-a"), (OPERATIONS_PER_CLIENT_MAX + 1, 1));
    assert_eq!(apply(&mut db, &cfg, &path, created).unwrap(), first);
    let newest = request(
        "tui-a",
        format!("poll-{}", OPERATIONS_PER_CLIENT_MAX + 7),
        1,
        create("q"),
    );
    assert_eq!(
        apply(&mut db, &cfg, &path, newest).unwrap_err().0,
        "operation_identity_conflict"
    );
    for n in 0..OPERATIONS_PER_CLIENT_MAX + 8 {
        let stale = request("tui-a", format!("stale-{n}"), 0, create("r"));
        assert_eq!(
            apply(&mut db, &cfg, &path, stale).unwrap().outcome,
            Outcome::Rejected {
                code: "stale_revision".into()
            }
        );
    }
    assert_eq!(
        rows(&db, "tui-a"),
        (2 * OPERATIONS_PER_CLIENT_MAX, OPERATIONS_PER_CLIENT_MAX)
    );
    let clients = OPERATIONS_MAX / OPERATIONS_PER_CLIENT_MAX + 1;
    for client in 0..clients {
        for n in 0..OPERATIONS_PER_CLIENT_MAX {
            let poll = request(
                &format!("tui-{client}"),
                format!("poll-{n}"),
                1,
                Command::Snapshot,
            );
            assert!(matches!(
                apply(&mut db, &cfg, &path, poll).unwrap().outcome,
                Outcome::Snapshot { .. }
            ));
        }
    }
    let total: i64 = db
        .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(total as usize, OPERATIONS_MAX);
    assert_eq!(
        rows(&db, &format!("tui-{}", clients - 1)),
        (OPERATIONS_PER_CLIENT_MAX, 0)
    );
    assert_eq!(rows(&db, "tui-a"), (0, 0));
    assert_eq!(
        db.query_row("SELECT revision FROM state", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

/// An engine database an earlier b19 build wrote keeps its `team_settings_revision` table,
/// which no code of this engine reads or writes: it opens again, the owner starts on it,
/// and removing a project works and leaves that table as it was (I3).
/// Input: a fresh engine database given the b19 table. Output: none.
#[test]
fn a_database_with_the_b19_team_settings_revision_table_opens_and_removes_projects() {
    let (_root, path, cfg) = fixture();
    {
        let (db, _lock) = open_database(&path, false).unwrap();
        db.execute_batch(
            "CREATE TABLE team_settings_revision (
                     project TEXT PRIMARY KEY,
                     revision INTEGER NOT NULL CHECK(revision > 0)
                 );
                 INSERT INTO team_settings_revision VALUES ('old', 4);",
        )
        .unwrap();
    }
    let (mut db, _lock) = open_database(&path, false).unwrap();
    let started = owner_start(&mut db).unwrap();
    owner_stop(&mut db, &started.host).unwrap();
    db.execute("INSERT INTO projects(id, body) VALUES ('old', '{}')", [])
        .unwrap();
    let tx = db.transaction().unwrap();
    assert_eq!(
        crate::project_setup::execute(
            &tx,
            &cfg,
            &crate::project_setup::SetupAction::Remove {
                project_id: crate::protocol::ProjectId::new("old").unwrap(),
            },
            2,
        )
        .unwrap(),
        crate::protocol::Outcome::Applied
    );
    tx.commit().unwrap();
    let kept = db
        .prepare("SELECT project, revision FROM team_settings_revision")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        kept,
        [("old".to_owned(), 4)],
        "the engine leaves the old table as it was"
    );
}

/// A project's change sets stay when the project is removed, so their numbers stay too:
/// sets A and B keep 1 and 2 through the removal, and a set D the project adds once it is
/// added again under the same id gets 3 (AL17, rv-code M1).
/// Input: a fresh engine database. Output: none.
#[test]
fn removing_a_project_keeps_its_change_set_numbers() {
    let (_root, path, cfg) = fixture();
    let (mut db, _lock) = open_database(&path, false).unwrap();
    let started = owner_start(&mut db).unwrap();
    owner_stop(&mut db, &started.host).unwrap();
    let add_project = |db: &Connection| {
        db.execute("INSERT INTO projects(id, body) VALUES ('kept', '{}')", [])
            .unwrap();
    };
    let add_set = |db: &Connection, digit: char, created_ms: i64| {
        db.execute(
                "INSERT INTO landing_change_sets(id,project,agent_kind,agent_id,label,repository,\
                 worktree,git_dir,base,head,state,last_outcome,turn,prompt_line,files,added,\
                 removed,conflicts,created_ms,updated_ms) VALUES (?1,'kept','sub','agent-1',\
                 'Agent 1','/r','/w','/g','b','h','discarded','succeeded','t','Fix',1,1,0,'[]',?2,?2)",
                params![digit.to_string().repeat(32), created_ms],
            )
            .unwrap();
        crate::landing::number_change_sets(db).unwrap();
    };
    let numbers = |db: &Connection| -> Vec<(char, i64)> {
        db.prepare("SELECT change_set, number FROM landing_change_set_numbers ORDER BY number")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
            .map(|row| {
                let (id, number) = row.unwrap();
                (id.chars().next().unwrap(), number)
            })
            .collect()
    };
    add_project(&db);
    crate::landing::number_change_sets(&db).unwrap();
    add_set(&db, 'a', 1);
    add_set(&db, 'b', 2);
    assert_eq!(numbers(&db), [('a', 1), ('b', 2)]);

    let tx = db.transaction().unwrap();
    assert_eq!(
        crate::project_setup::execute(
            &tx,
            &cfg,
            &crate::project_setup::SetupAction::Remove {
                project_id: crate::protocol::ProjectId::new("kept").unwrap(),
            },
            2,
        )
        .unwrap(),
        crate::protocol::Outcome::Applied
    );
    tx.commit().unwrap();
    assert_eq!(
        numbers(&db),
        [('a', 1), ('b', 2)],
        "the removal keeps the sets, so it keeps their numbers"
    );

    add_project(&db);
    add_set(&db, 'd', 3);
    assert_eq!(
        numbers(&db),
        [('a', 1), ('b', 2), ('d', 3)],
        "a set the project adds once it is back gets the next number"
    );
}

/// The owner's questions log keeps out of the names the owner schema check reserves, so
/// an engine whose owner start made the log (F12) and whose questions write kept it
/// starts again ready. Input: a fresh engine database. Output: none; the second owner
/// start succeeds.
#[test]
fn an_engine_that_created_the_questions_log_restarts_ready() {
    let (_root, path, _cfg) = fixture();
    let (mut db, _lock) = open_database(&path, false).unwrap();
    let started = owner_start(&mut db).unwrap();
    owner_stop(&mut db, &started.host).unwrap();
    let logged = |db: &Connection| -> bool {
        db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='questions_log')",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert!(logged(&db), "the owner start makes the questions log");
    let tx = db.transaction().unwrap();
    crate::questions::forget_project(&tx, &crate::protocol::ProjectId::new("asked").unwrap())
        .unwrap();
    tx.commit().unwrap();
    assert!(logged(&db), "a questions write keeps the log");
    let restarted = owner_start(&mut db).unwrap();
    owner_stop(&mut db, &restarted.host).unwrap();
}

#[test]
fn owner_schema_is_additive_and_unknown_schema_is_preserved() {
    let (_root, path, _cfg) = fixture();
    let (mut db, _lock) = open_database(&path, false).unwrap();
    db.execute(
        "INSERT INTO operations VALUES ('owner:tui','legacy',x'01',x'02')",
        [],
    )
    .unwrap();
    let started = owner_start(&mut db).unwrap();
    assert_ne!(
        started.owners[0].admission.owner_id,
        started.owners[1].admission.owner_id
    );
    assert_eq!(
        db.query_row(
            "SELECT request FROM operations WHERE client='owner:tui'",
            [],
            |r| r.get::<_, Vec<u8>>(0)
        )
        .unwrap(),
        vec![1]
    );
    owner_stop(&mut db, &started.host).unwrap();
    db.execute("UPDATE owner_meta SET schema_version=2", [])
        .unwrap_err();
    db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE owner_meta SET schema_version=2; PRAGMA ignore_check_constraints=OFF;").unwrap();
    assert!(owner_start(&mut db).is_err());
    assert_eq!(
        db.query_row("SELECT schema_version FROM owner_meta", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    let other = tempfile::tempdir().unwrap();
    std::fs::set_permissions(other.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (mut conflict, _other_lock) = open_database(&other.path().join("db"), false).unwrap();
    conflict
        .execute("CREATE TABLE owner_meta (unknown TEXT)", [])
        .unwrap();
    conflict
        .execute("INSERT INTO owner_meta VALUES ('preserve')", [])
        .unwrap();
    assert!(owner_start(&mut conflict).is_err());
    assert_eq!(
        conflict
            .query_row("SELECT unknown FROM owner_meta", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "preserve"
    );
    assert_eq!(
        conflict
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name='owner_identities'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
#[should_panic(expected = "assertion `left == right` failed")]
fn internal_ledger_hash_shape_is_a_runtime_invariant() {
    let (_root, path, cfg) = fixture();
    let (mut db, _lock) = open_database(&path, false).unwrap();
    let started = owner_start(&mut db).unwrap();
    let result = owner_apply(
        &mut db,
        &cfg,
        &path,
        &started.owners[0].admission,
        &mutation("shape"),
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    owner_record(&tx, &result.provenance, &[0; 31], b"{}").unwrap();
}

#[test]
fn epoch_exhaustion_revokes_and_cannot_reactivate() {
    let (_root, path, _cfg) = fixture();
    let (mut db, _lock) = open_database(&path, false).unwrap();
    let started = owner_start(&mut db).unwrap();
    db.execute(
        "UPDATE owner_identities SET owner_epoch=?1 WHERE client_kind='tui'",
        [i64::MAX],
    )
    .unwrap();
    let control = OwnerControl {
        operation_id: RequestId::new("exhausted").unwrap(),
        target: OwnerClientKind::Tui,
        expected_epoch: i64::MAX,
        decision: OwnerDecision::Revoke,
    };
    let (reply, _) = owner_control(&mut db, &started.host, &control).unwrap();
    assert!(!reply.active);
    assert!(reply.epoch_exhausted_revoked);
    assert_eq!(reply.owner_epoch, i64::MAX);
    let reactivate = OwnerControl {
        operation_id: RequestId::new("reactivate").unwrap(),
        decision: OwnerDecision::Reactivate,
        ..control.clone()
    };
    assert!(owner_control(&mut db, &started.host, &reactivate).is_err());
    assert!(
        !owner_control(&mut db, &started.host, &control)
            .unwrap()
            .0
            .active
    );
}

#[test]
fn owner_ledger_abort_rolls_back_domain_effect_and_pages_bound_corrupt_body() {
    let (_root, path, cfg) = fixture();
    let (mut db, _lock) = open_database(&path, false).unwrap();
    let started = owner_start(&mut db).unwrap();
    let a = &started.owners[0].admission;
    db.execute(
        "INSERT INTO projects VALUES ('huge',?1)",
        ["x".repeat(FRAME_BYTES_MAX + 1)],
    )
    .unwrap();
    assert!(
        owner_read(
            &mut db,
            &cfg,
            &path,
            a,
            &OwnerRead::Page {
                view: OwnerView::Projects,
                cursor: None,
                limit: 2
            }
        )
        .is_err()
    );
    db.execute("DELETE FROM projects WHERE id='huge'", [])
        .unwrap();
    use crate::verification::*;
    let draft = BaselineDraft {
        version: 1,
        project_id: ProjectId::new("p").unwrap(),
        origin: BaselineOrigin::Imported,
        requirements: "approved checks".into(),
        checks: vec![
            CheckSpec {
                id: RequestId::new("code").unwrap(),
                kind: CheckKind::Code,
                minimum_risk: Risk::Low,
                content: b"code".to_vec(),
            },
            CheckSpec {
                id: RequestId::new("integrity").unwrap(),
                kind: CheckKind::TestIntegrity,
                minimum_risk: Risk::Low,
                content: b"integrity".to_vec(),
            },
        ],
        rules: vec![],
        retry_limit: 0,
    };
    let baseline = draft.identity().unwrap();
    for (n, command) in [
        Command::CreateProject {
            project_id: draft.project_id.clone(),
            spec: ProjectSpec {
                name: "Project".into(),
                repositories: vec![],
            },
        },
        Command::ProposeBaseline { draft },
    ]
    .into_iter()
    .enumerate()
    {
        let input = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            client_id: ClientId::new("host").unwrap(),
            request_id: RequestId::new(format!("fixture-{n}")).unwrap(),
            expected_revision: Cursor::new(n as u64),
            command,
        };
        assert_eq!(
            apply(&mut db, &cfg, &path, input).unwrap().outcome,
            Outcome::Applied
        );
    }
    db.execute_batch("CREATE TRIGGER owner_abort BEFORE INSERT ON owner_operations BEGIN SELECT RAISE(ABORT,'real_sqlite_abort'); END;").unwrap();
    let before: i64 = db
        .query_row("SELECT COUNT(*) FROM owner_operations", [], |r| r.get(0))
        .unwrap();
    let approve = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("aborted").unwrap(),
        expected_revision: Cursor::new(2),
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::ApproveBaseline {
                baseline_id: baseline.clone(),
            },
        },
    };
    assert!(
        owner_apply(&mut db, &cfg, &path, a, &approve)
            .unwrap_err()
            .0
            .contains("real_sqlite_abort")
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM owner_operations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert_eq!(
        db.query_row("SELECT revision FROM state", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row(
            "SELECT approved FROM verification_baselines WHERE id=?1",
            [baseline.as_str()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
}

/// v2.5 L6 (leader 03:14, fix11 L7 Q6): an engine-own write moves the revision and writes
/// one `events` row at it, so a client catching up from `events` sees the change and its
/// next save applies instead of failing `stale_revision`. The write goes through
/// `Appender::opening`, which is `Appender::own` behind the open's proof.
#[test]
fn a_client_save_after_an_own_write_applies() {
    let (_root, path, cfg) = fixture();
    let (mut db, _lock) = open_database(&path, false).unwrap();
    let request = |id: &str, revision: u64, command: Command| WireRequest {
        protocol_version: PROTOCOL_VERSION,
        client_id: ClientId::new("tui-a").unwrap(),
        request_id: RequestId::new(id).unwrap(),
        expected_revision: Cursor::new(revision),
        command,
    };
    let create = |id: &str| Command::CreateProject {
        project_id: ProjectId::new(id).unwrap(),
        spec: ProjectSpec {
            name: "Project".into(),
            repositories: vec![],
        },
    };
    let created = apply(&mut db, &cfg, &path, request("create-p", 0, create("p"))).unwrap();
    assert_eq!(created.outcome, Outcome::Applied);

    let tx = db.transaction().unwrap();
    let opening = super::opening::Opening::new(7);
    let appender = crate::state_log::Appender::opening(&tx, opening).unwrap();
    assert_eq!(appender.revision(), 2);
    drop(appender);
    tx.commit().unwrap();

    let after = Command::Events {
        after: Cursor::new(1),
    };
    let caught = apply(&mut db, &cfg, &path, request("events-1", 1, after)).unwrap();
    let Outcome::Events { events } = caught.outcome else {
        panic!("a catch-up reads events: {:?}", caught.outcome);
    };
    assert_eq!(events.len(), 1, "the own write is one event");
    assert_eq!(events[0].cursor.value(), 2);
    assert_eq!(events[0].client_id.as_str(), "owner:engine");
    let stale = apply(&mut db, &cfg, &path, request("create-q", 1, create("q"))).unwrap();
    assert_eq!(
        stale.outcome,
        Outcome::Rejected {
            code: "stale_revision".into()
        }
    );
    let seen = events[0].cursor.value();
    let saved = apply(&mut db, &cfg, &path, request("create-r", seen, create("r"))).unwrap();
    assert_eq!(
        saved.outcome,
        Outcome::Applied,
        "a save at the caught-up revision"
    );
}
